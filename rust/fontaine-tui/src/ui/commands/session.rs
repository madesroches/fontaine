use std::ops::ControlFlow::{self, Break, Continue};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::resume_picker::ResumePicker;
use crate::{App, SessionTarget, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::ledger::Record;
use fontaine_core::session::{self, Session};
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::ROOT_AGENT;

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/resume [name-or-id]`: switches to the session named, or opens the picker.
    pub(crate) fn resume(&mut self, runtime: &AgentRuntime, argument: &str) -> ControlFlow<()> {
        if argument.is_empty() {
            let others = session::list(&self.data_dir, Some(&self.app.primary_root))
                .into_iter()
                .filter(|info| info.id != runtime.session.id)
                // Every launch writes a start, so a session nothing was said in is only noise.
                .filter(|info| info.name.is_some() || info.first_prompt.is_some())
                .collect();
            self.app.resume = Some(ResumePicker::new(others, SystemTime::now()));
            return Continue(());
        }
        self.switch_to(runtime, argument)
    }

    /// Leaves for the session `text` names (an id or a name), once it is known to exist.
    pub(crate) fn switch_to(&mut self, runtime: &AgentRuntime, text: &str) -> ControlFlow<()> {
        self.switch_from(&runtime.session.id, text)
    }

    pub(crate) fn switch_from(&mut self, current: &str, text: &str) -> ControlFlow<()> {
        match session::resolve(&self.data_dir, &self.app.primary_root, text) {
            Ok(id) if id == current => {
                self.app
                    .transcript
                    .notice("this is the session you are in already");
                Continue(())
            }
            Ok(id) => {
                // Once the run is torn down a session that cannot open ends the process.
                if let Err(e) = Session::check_openable(&self.data_dir, &id) {
                    self.app.transcript.error(format!("/resume: {}", chain(&e)));
                    return Continue(());
                }
                self.switch = Some(SessionTarget {
                    source: id,
                    record_resume: true,
                });
                Break(())
            }
            Err(e) => {
                self.app.transcript.error(format!("/resume: {e}"));
                Continue(())
            }
        }
    }

    /// `/branch [name]`: copies the session and carries on in the copy; this one stays as it is.
    pub(crate) fn branch(&mut self, runtime: &AgentRuntime, name: &str) -> ControlFlow<()> {
        let agents = runtime.state.agents();
        if agents.any_running() || agents.has_notifications(ROOT_AGENT) {
            self.app.transcript.error(
                "subagents are still running or have reports to deliver; /branch is refused until they finish or are killed",
            );
            return Continue(());
        }
        let name = match name {
            "" => None,
            name => match session::validate_name(name) {
                Ok(name) => Some(name),
                Err(e) => {
                    self.app.transcript.error(format!("/branch: {e}"));
                    return Continue(());
                }
            },
        };
        let source = &runtime.session.id;
        let id = session::new_id();
        let copy = match Session::fork_named(&self.data_dir, source, &id, name.as_deref()) {
            Ok(copy) => copy,
            Err(e) => {
                self.app.transcript.error(format!("/branch: {}", chain(&e)));
                return Continue(());
            }
        };
        let recorded = copy.ledger.append(Record::Resumed {
            source: source.clone(),
            how: "fork (branch)".to_string(),
        });
        if let Err(e) = recorded {
            self.app.transcript.error(format!(
                "/branch: the copy could not be recorded: {}",
                chain(&e)
            ));
            // Best effort: a copy that was never switched to is only clutter in the picker.
            let _ = std::fs::remove_dir_all(&copy.dir);
            return Continue(());
        }
        self.switch = Some(SessionTarget {
            source: id,
            record_resume: false,
        });
        Break(())
    }

    /// `/rename [name]`: names the session; with no name, one is drawn from the first message.
    pub(crate) fn rename(&mut self, runtime: &AgentRuntime, name: &str) {
        let name = match name {
            "" => match runtime.session.suggested_name() {
                Some(name) => name,
                None => {
                    self.app.transcript.error(
                        "/rename: there is no message to draw a name from; give one: /rename <name>",
                    );
                    return;
                }
            },
            name => name.to_string(),
        };
        match runtime.session.rename(&name) {
            Ok(name) => self
                .app
                .transcript
                .notice(format!("session renamed to {name}")),
            Err(e) => self.app.transcript.error(format!("/rename: {}", chain(&e))),
        }
    }

    /// `/export [path]`: writes the conversation as Markdown, to `fontaine-<id>.md` in the launch
    /// directory unless a path is given.
    pub(crate) async fn export(&mut self, runtime: &AgentRuntime, launch_dir: &Path, path: &str) {
        let path = path.trim_matches(|c| c == '"' || c == '\'');
        let path: PathBuf = if path.is_empty() {
            let id = &runtime.session.id;
            launch_dir.join(format!("fontaine-{}.md", id.get(..8).unwrap_or(id)))
        } else {
            launch_dir.join(path)
        };
        match fontaine_core::export::export_session(&runtime.session, &path).await {
            Ok(()) => self
                .app
                .transcript
                .notice(format!("exported the conversation to {}", path.display())),
            Err(e) => self.app.transcript.error(format!("/export: {}", chain(&e))),
        }
    }
}
