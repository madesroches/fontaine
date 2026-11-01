use std::ops::ControlFlow;

use crate::setup_prompt::{CompositionPrompt, SetupReviewPrompt};
use crate::{Action, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::now_rfc3339;
use fontaine_core::composition_offer::{self, Candidate, Offer, Posture};
use fontaine_core::project_env::discover::{self, SystemGh};
use fontaine_core::project_env::{self, ProjectEnvironment, ScanStatus, ScanStep, sample};
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

impl<R: FnMut(&mut crate::App)> Ui<R> {
    /// `/auto-mode-setup`: collects the project's environment facts and, in a trusted
    /// workspace, has the guardian's model propose more from the repository. The user reviews
    /// the proposals; the result is saved as `fontaine auto-mode setup` saves it and applies to
    /// this session at once. A last, optional step offers the guardian composition's allow
    /// screen, which is written to the user settings and applies from the next session.
    pub(crate) async fn auto_mode_setup(&mut self, runtime: &mut AgentRuntime) -> ControlFlow<()> {
        let root = runtime.primary_root();
        let remotes = runtime.session_remotes();
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
        let record = step.apply_to(found.record, &self.data_dir, &runtime.primary_root());
        self.report_and_apply(runtime, record, &found.fallbacks, &step);
        self.composition_step(runtime).await
    }

    /// Offers the composition when nothing configured it yet. Declining any prompt writes nothing.
    async fn composition_step(&mut self, runtime: &mut AgentRuntime) -> ControlFlow<()> {
        if composition_offer::existing(&runtime.config)
            || runtime.guardian_profile() == fontaine_guardian::Profile::Minimal
        {
            return ControlFlow::Continue(());
        }
        let Some(file) = self.user_config.clone() else {
            self.app.transcript.notice(
                "guardian composition: skipped (this session has no user settings file to write it to)",
            );
            return ControlFlow::Continue(());
        };
        let mut listings = Vec::new();
        for account in composition_offer::ollama_accounts(&runtime.config) {
            self.draw(runtime);
            if let Ok(found) = runtime.model_listings(Some(&account)).await {
                listings.extend(found);
            }
        }
        let found = composition_offer::detect(&listings);
        if found.is_empty() {
            self.app
                .transcript
                .notice(composition_offer::nothing_to_offer());
            return ControlFlow::Continue(());
        }
        let Some(answer) = self.offer(runtime, found).await else {
            return ControlFlow::Break(());
        };
        let Some(offer) = answer else {
            self.app
                .transcript
                .notice("guardian composition: not set up");
            return ControlFlow::Continue(());
        };
        match composition_offer::write(&file, &runtime.config, &offer) {
            Ok(()) => {
                self.app.transcript.notice(format!(
                    "guardian composition: wrote the {} screen on {} to {}",
                    match offer.posture {
                        Posture::Safe => "safe",
                        Posture::Fast => "fast",
                    },
                    offer.model(),
                    file.display()
                ));
                for line in composition_offer::calibration_note(&offer) {
                    self.app.transcript.notice(line);
                }
            }
            Err(e) => self
                .app
                .transcript
                .error(format!("guardian composition: not written: {}", chain(&e))),
        }
        ControlFlow::Continue(())
    }

    /// The composition prompt. `None` when the human quit; `Some(None)` when they declined.
    async fn offer(
        &mut self,
        runtime: &AgentRuntime,
        candidates: Vec<Candidate>,
    ) -> Option<Option<Offer>> {
        self.app
            .begin_setup_composition(CompositionPrompt::new(candidates));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.setup_composition = None;
                return Some(None);
            };
            match self.app.handle_input(input) {
                Action::SetupComposition(answer) => return Some(answer),
                Action::Quit => return None,
                _ => {}
            }
        }
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
        let handle = match project_env::scan_model(runtime.models.guardian.as_ref()) {
            Ok(handle) => handle.clone(),
            Err(status) => return Ok(ScanStep::without(status)),
        };
        let label = handle.reference.to_string();
        self.app.transcript.notice(project_env::scan_notice(&label));
        let settings = project_env::scan_settings(&handle, runtime.config.guardian.timeout_secs);
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
        let (mut step, proposed) = ScanStep::proposed(proposals, &label, handle.catalog_billed());
        let Some((scan, count)) = proposed else {
            return Ok(step);
        };
        let review = sample::review_text(&scan);
        for line in review.lines() {
            self.app.transcript.notice(line.to_string());
        }
        match self.review(runtime, review, count).await {
            Some(accepted) => step.decide(scan, count, accepted),
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
        for line in project_env::report_lines(
            &record,
            fallbacks,
            &step.status,
            &step.dropped,
            step.usage.as_deref(),
        ) {
            self.app.transcript.notice(line);
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
