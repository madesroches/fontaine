use std::ops::ControlFlow;

use crate::bug_prompt::{BugAnswer, BugPrompt};
use crate::{Action, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::bug_report::{BugReport, DEFAULT_TURNS, Facts, Level, Rendered};
use fontaine_core::bug_submit::{self, Submit};
use fontaine_core::config_catalog::Running;
use fontaine_core::redact::{Redactor, session_secrets};
use fontaine_tools::error_chain::chain;

impl<R: FnMut(&mut crate::App)> Ui<R> {
    /// `/bug [description | draft-id | discard draft-id]`: previews the redacted report, and on
    /// confirmation files it. Cancelling writes and sends nothing.
    pub(crate) async fn bug(&mut self, runtime: &AgentRuntime, argument: &str) -> ControlFlow<()> {
        let drafts = fontaine_core::bug_drafts::list(&self.data_dir);
        if argument == "discard" {
            self.app
                .transcript
                .error("/bug discard needs a draft id: /bug discard <id>");
            return ControlFlow::Continue(());
        }
        if let Some(id) = argument.strip_prefix("discard ").map(str::trim)
            && drafts.iter().any(|d| d.id == id)
        {
            self.discard_draft(&drafts, id);
            return ControlFlow::Continue(());
        }
        let draft_id = drafts
            .iter()
            .find(|d| d.id == argument)
            .map(|d| d.id.clone());
        let description = match drafts.iter().find(|d| d.id == argument) {
            Some(draft) => draft.description(),
            None if argument.is_empty() => {
                if drafts.is_empty() {
                    self.app
                        .transcript
                        .error("/bug needs a description: /bug <what went wrong>");
                } else {
                    let list: Vec<String> = drafts
                        .iter()
                        .map(|d| format!("  {} — {}", d.id, d.title))
                        .collect();
                    self.app.transcript.notice(format!(
                        "queued drafts (load one with /bug <id>, drop one with /bug discard <id>, or describe the bug yourself):\n{}",
                        list.join("\n")
                    ));
                }
                return ControlFlow::Continue(());
            }
            None => argument.to_string(),
        };
        let mut facts = self.bug_facts(runtime, description);
        let warnings = std::cell::RefCell::new(Vec::new());
        let secrets = session_secrets(&runtime.config, &self.data_dir, &*self.env, &|m| {
            warnings.borrow_mut().push(m.to_string())
        });
        for warning in warnings.into_inner() {
            self.app.transcript.error(format!("/bug: {warning}"));
        }
        let redactor = Redactor::new(secrets);
        let mut level = Level::Minimal;
        let gh = std::env::var_os("PATH").is_some()
            && tokio::task::spawn_blocking(|| bug_submit::gh_ready("gh"))
                .await
                .unwrap_or(false);
        let rendered = render(&facts, level, &redactor);
        self.app.begin_bug(BugPrompt::new(
            rendered.title.clone(),
            rendered.body.clone(),
            level,
            gh,
        ));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.bug_prompt = None;
                return ControlFlow::Continue(());
            };
            match self.app.handle_input(input) {
                Action::Bug(BugAnswer::Level(next)) => {
                    level = next;
                    if level == Level::Full && facts.transcript.is_empty() {
                        match fontaine_core::export::transcript_tail(
                            &runtime.session,
                            DEFAULT_TURNS,
                        )
                        .await
                        {
                            Ok(tail) => facts.transcript = tail,
                            Err(e) => self
                                .app
                                .transcript
                                .error(format!("/bug: no transcript: {}", chain(&e))),
                        }
                    }
                    let rendered = render(&facts, level, &redactor);
                    if let Some(prompt) = &mut self.app.bug_prompt {
                        prompt.set_text(rendered.title, rendered.body, level);
                    }
                }
                Action::Bug(BugAnswer::Cancel) => {
                    self.app
                        .transcript
                        .notice("bug report cancelled; nothing was written or sent");
                    return ControlFlow::Continue(());
                }
                Action::Bug(BugAnswer::Submit { method, level }) => {
                    let rendered = render(&facts, level, &redactor);
                    let done = self.submit_bug(runtime, method, level, rendered).await;
                    if done
                        && let Some(id) = &draft_id
                        && let Err(e) = fontaine_core::bug_drafts::remove(&self.data_dir, id)
                    {
                        self.app
                            .transcript
                            .error(format!("/bug: cannot delete draft {id}: {}", chain(&e)));
                    }
                    return ControlFlow::Continue(());
                }
                Action::Quit => return ControlFlow::Break(()),
                _ => {}
            }
        }
    }

    fn discard_draft(&mut self, drafts: &[fontaine_core::bug_drafts::Draft], id: &str) {
        if !drafts.iter().any(|d| d.id == id) {
            self.app
                .transcript
                .error(format!("/bug: no queued draft {id}"));
            return;
        }
        match fontaine_core::bug_drafts::remove(&self.data_dir, id) {
            Ok(()) => self.app.transcript.notice(format!("discarded draft {id}")),
            Err(e) => self
                .app
                .transcript
                .error(format!("/bug: cannot delete draft {id}: {}", chain(&e))),
        }
    }

    async fn submit_bug(
        &mut self,
        runtime: &AgentRuntime,
        method: Submit,
        level: Level,
        rendered: Rendered,
    ) -> bool {
        let bundle =
            match bug_submit::write_bundle(&self.data_dir, &runtime.session.id, &rendered.bundle) {
                Ok(path) => path,
                Err(e) => {
                    self.app.transcript.error(format!("/bug: {}", chain(&e)));
                    return false;
                }
            };
        let (used, url) = match method {
            Submit::Gh => {
                let (title, body) = (rendered.title.clone(), rendered.body.clone());
                let filed = tokio::task::spawn_blocking(move || {
                    bug_submit::file_with_gh("gh", &title, &body)
                })
                .await;
                match filed {
                    Ok(Ok(url)) => (Submit::Gh, url),
                    Ok(Err(e)) => {
                        self.app
                            .transcript
                            .error(format!("/bug: {}; falling back to a URL", chain(&e)));
                        (
                            Submit::Url,
                            bug_submit::prefilled_url(&rendered.title, &rendered.url_body, &bundle),
                        )
                    }
                    Err(e) => {
                        self.app
                            .transcript
                            .error(format!("/bug: the gh task failed: {}", chain(&e)));
                        return false;
                    }
                }
            }
            Submit::Url => (
                Submit::Url,
                bug_submit::prefilled_url(&rendered.title, &rendered.url_body, &bundle),
            ),
        };
        runtime
            .state
            .record_bug_report(used.as_str(), level.as_str(), &url);
        let what = match used {
            Submit::Gh => "filed the bug report",
            Submit::Url => "open this URL to file the bug report (nothing was sent)",
        };
        self.app.transcript.notice(format!(
            "{what}: {url}\nthe full report is saved at {}",
            bundle.display()
        ));
        true
    }

    fn bug_facts(&mut self, runtime: &AgentRuntime, description: String) -> Facts {
        let build = crate::BUILD_INFO.get();
        let guardian = runtime.state.guardian();
        let main = runtime.models.main.reference.to_string();
        let theme = self
            .current_theme()
            .unwrap_or(runtime.config.tui.theme)
            .as_str()
            .to_string();
        let running = Running {
            theme: &theme,
            main_model: &main,
            guardian_model: Some(guardian.label()),
            effort: runtime.state.effort(),
        };
        let section =
            fontaine_core::doctor_report::settings_section(&runtime.config, &running, false);
        let config_rows = section
            .findings
            .iter()
            .map(|f| format!("{} = {}", f.subject, f.detail))
            .collect();
        let config_scopes = runtime
            .config
            .scopes
            .iter()
            .filter(|s| s.loaded)
            .map(|s| s.slot.label().to_string())
            .collect();
        let verdicts = match runtime.session.audit.read() {
            Ok(records) => records
                .iter()
                .filter(|r| r.guardian_verdict.is_some())
                .rev()
                .take(20)
                .map(|r| {
                    format!(
                        "{}: {} ({})",
                        r.tool,
                        r.guardian_verdict.as_deref().unwrap_or_default(),
                        r.tier.as_str()
                    )
                })
                .collect(),
            Err(e) => {
                self.app
                    .transcript
                    .error(format!("/bug: cannot read the audit log: {}", chain(&e)));
                vec!["verdicts unavailable".to_string()]
            }
        };
        let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((0, 0));
        Facts {
            version: build
                .map_or(env!("CARGO_PKG_VERSION"), |b| b.version)
                .to_string(),
            git_sha: build.map_or("unknown", |b| b.git_sha).to_string(),
            target: build.map_or("unknown", |b| b.target).to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            term: (self.env)("TERM")
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into()),
            terminal_size: format!("{cols}x{rows}"),
            install_method: install_method(),
            provider: runtime.models.main.reference.account.clone(),
            model: main.clone(),
            guardian_profile: runtime.guardian_profile().as_str().to_string(),
            guardian_model: guardian.label().to_string(),
            permission_mode: runtime.state.mode().as_str().to_string(),
            config_rows,
            config_scopes,
            warnings: self.app.recent_warnings.iter().cloned().collect(),
            verdicts,
            session_id: runtime.session.id.clone(),
            description,
            transcript: String::new(),
        }
    }
}

fn render(facts: &Facts, level: Level, redactor: &Redactor) -> Rendered {
    BugReport::build(facts, level).render(redactor)
}

/// Where the running binary lives, as a hint for the maintainers: not a path.
fn install_method() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    let text = exe.to_string_lossy();
    if text.contains("/.cargo/") || text.contains("\\.cargo\\") {
        "cargo install"
    } else if text.contains("/target/") || text.contains("\\target\\") {
        "built from source"
    } else if text.contains("/Cellar/") || text.contains("/homebrew/") {
        "homebrew"
    } else {
        "unknown"
    }
    .to_string()
}
