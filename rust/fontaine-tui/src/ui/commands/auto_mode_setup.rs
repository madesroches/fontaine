use std::ops::ControlFlow;

use crate::setup_prompt::SetupReviewPrompt;
use crate::{Action, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::now_rfc3339;
use fontaine_core::ledger::git_remotes;
use fontaine_core::models::GuardianModel;
use fontaine_core::project_env::discover::{self, SystemGh};
use fontaine_core::project_env::{
    self, ProjectEnvironment, ScanFacts, ScanStatus, env_scan, sample,
};
use fontaine_core::trust;
use fontaine_tools::error_chain::chain;
use tokio::task::JoinHandle;

/// How a background step ended.
enum Waited<T> {
    Done(T),
    /// The human interrupted it, or the step itself failed to finish.
    Cancelled,
    Quit,
}

/// What the scan step leaves for the record and the report.
struct ScanStep {
    status: ScanStatus,
    accepted: Option<ScanFacts>,
    dropped: Vec<String>,
    usage: Option<String>,
}

impl ScanStep {
    fn without(status: ScanStatus) -> ScanStep {
        ScanStep {
            status,
            accepted: None,
            dropped: Vec::new(),
            usage: None,
        }
    }
}

impl<R: FnMut(&mut crate::App)> Ui<R> {
    /// `/auto-mode-setup`: collects the project's environment facts and, in a trusted
    /// workspace, has the guardian's model propose more from the repository. The user reviews
    /// the proposals; the result is saved as `fontaine auto-mode setup` saves it and applies to
    /// this session at once.
    pub(crate) async fn auto_mode_setup(&mut self, runtime: &mut AgentRuntime) -> ControlFlow<()> {
        let root = runtime.primary_root();
        let remotes = git_remotes(&root);
        self.app.transcript.notice(
            "collecting this project's environment facts (GitHub API through `gh`, repository files); Esc cancels",
        );
        let task = {
            let root = root.clone();
            tokio::spawn(async move {
                discover::discover(&SystemGh, &root, &remotes, now_rfc3339()).await
            })
        };
        let found = match self.wait_for(runtime, task).await {
            Waited::Done(found) => found,
            Waited::Cancelled => {
                self.app.transcript.notice("auto-mode setup cancelled");
                return ControlFlow::Continue(());
            }
            Waited::Quit => return ControlFlow::Break(()),
        };
        let step = match self.scan_step(runtime, &found).await {
            Ok(step) => step,
            Err(flow) => return flow,
        };
        let mut record = found.record;
        record.scan = step.accepted.clone();
        self.report_and_apply(runtime, record, &found.fallbacks, &step);
        ControlFlow::Continue(())
    }

    /// Runs the model scan when the workspace is trusted, shows its proposals for review and
    /// returns what the user accepted. `Err` is the flow to return from the command.
    async fn scan_step(
        &mut self,
        runtime: &AgentRuntime,
        found: &discover::Discovery,
    ) -> Result<ScanStep, ControlFlow<()>> {
        let trusted = trust::is_trusted(&self.data_dir, &runtime.config.workspace);
        if let Err(why) = project_env::scan_gate(false, true, trusted) {
            return Ok(ScanStep::without(ScanStatus::Skipped(why)));
        }
        let handle = match &runtime.models.guardian {
            Some(GuardianModel::Ready(handle)) => handle.clone(),
            Some(GuardianModel::Unavailable { reference, reason }) => {
                return Ok(ScanStep::without(ScanStatus::Failed(format!(
                    "the guardian model {reference} is unavailable: {reason}"
                ))));
            }
            None => {
                return Ok(ScanStep::without(ScanStatus::Failed(
                    "no guardian model is configured".to_string(),
                )));
            }
        };
        let label = handle.reference.to_string();
        self.app.transcript.notice(format!(
            "sending a sample of this repository to {label} to propose sensitive locations, deploy targets and trusted destinations (the account's provider receives it)"
        ));
        let settings = env_scan::Settings {
            timeout: std::time::Duration::from_secs(runtime.config.guardian.timeout_secs),
            additional_params: handle.thinking_off.clone(),
        };
        let (repo_root, project_settings) = (found.repo_root.clone(), found.settings.clone());
        let task = {
            let (model, label) = (handle.completion.clone(), label.clone());
            tokio::spawn(async move {
                let sample = tokio::task::spawn_blocking(move || {
                    sample::gather(&repo_root, &project_settings)
                })
                .await
                .map_err(|e| e.to_string())?;
                project_env::propose(&model, &label, &settings, &sample)
                    .await
                    .map_err(|e| chain(&e))
            })
        };
        let proposals = match self.wait_for(runtime, task).await {
            Waited::Done(Ok(proposals)) => proposals,
            Waited::Done(Err(why)) => {
                return Ok(ScanStep::without(ScanStatus::Failed(why)));
            }
            Waited::Cancelled => {
                return Ok(ScanStep::without(ScanStatus::Skipped(
                    "cancelled".to_string(),
                )));
            }
            Waited::Quit => return Err(ControlFlow::Break(())),
        };
        let tokens = |n: Option<u64>| n.map_or("unknown".to_string(), |n| n.to_string());
        let usage = Some(format!(
            "{label}: {} input tokens, {} output tokens",
            tokens(proposals.usage.input_tokens),
            tokens(proposals.usage.output_tokens)
        ));
        let count = proposals.scan.count();
        let mut step = ScanStep {
            dropped: proposals.dropped,
            usage,
            ..ScanStep::without(ScanStatus::Skipped(
                "the model proposed nothing".to_string(),
            ))
        };
        if count == 0 {
            return Ok(step);
        }
        let review = sample::review_text(&proposals.scan);
        for line in review.lines() {
            self.app.transcript.notice(line.to_string());
        }
        match self.review(runtime, review, count).await {
            Some(true) => {
                step.status = ScanStatus::Accepted(count);
                step.accepted = Some(proposals.scan);
            }
            Some(false) => step.status = ScanStatus::Declined(count),
            None => return Err(ControlFlow::Break(())),
        }
        Ok(step)
    }

    /// The review prompt. `None` when the human quit.
    async fn review(
        &mut self,
        runtime: &AgentRuntime,
        review: String,
        count: usize,
    ) -> Option<bool> {
        self.app
            .begin_setup_review(SetupReviewPrompt::new(review, count));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.setup_review = None;
                return Some(false);
            };
            match self.app.handle_input(input) {
                Action::SetupReview(answer) => return Some(answer),
                Action::Quit => return None,
                _ => {}
            }
        }
    }

    fn report_and_apply(
        &mut self,
        runtime: &AgentRuntime,
        record: ProjectEnvironment,
        fallbacks: &[String],
        step: &ScanStep,
    ) {
        for line in project_env::report(&record, fallbacks) {
            self.app.transcript.notice(line);
        }
        self.app.transcript.notice(step.status.line());
        for dropped in &step.dropped {
            self.app.transcript.notice(format!("  dropped: {dropped}"));
        }
        if let Some(usage) = &step.usage {
            self.app.transcript.notice(format!("scan usage: {usage}"));
        }
        let file = project_env::path(&self.data_dir, &runtime.primary_root());
        match runtime.apply_project_environment(record) {
            Ok(()) => self.app.transcript.notice(format!(
                "saved to {}; applied to this session",
                file.display()
            )),
            Err(e) => self.app.transcript.error(format!(
                "cannot save the project environment: {}",
                chain(&e)
            )),
        }
    }

    /// Waits for a background step while the screen keeps drawing. Esc abandons it.
    async fn wait_for<T: Send + 'static>(
        &mut self,
        runtime: &AgentRuntime,
        mut task: JoinHandle<T>,
    ) -> Waited<T> {
        loop {
            self.draw(runtime);
            tokio::select! {
                joined = &mut task => {
                    return match joined {
                        Ok(value) => Waited::Done(value),
                        Err(_) => Waited::Cancelled,
                    };
                }
                input = self.input.recv() => match input {
                    None => {
                        task.abort();
                        return Waited::Cancelled;
                    }
                    Some(input) => match self.app.handle_input(input) {
                        Action::Quit => {
                            task.abort();
                            return Waited::Quit;
                        }
                        Action::Interrupt => {
                            task.abort();
                            return Waited::Cancelled;
                        }
                        _ => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
            }
        }
    }
}
