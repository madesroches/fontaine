use std::ops::ControlFlow;

use crate::commands::{AfkRequest, parse_afk};
use crate::reports;
use crate::{Action, App, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::goal::PauseReason;

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/afk report [all]` and `/afk review`. Interactive only: the model, a config key and a
    /// scheduled prompt have no way to lift a stop. AFK mode itself is entered and left with the
    /// mode key.
    pub(crate) async fn afk_command(
        &mut self,
        runtime: &mut AgentRuntime,
        argument: &str,
    ) -> ControlFlow<()> {
        match parse_afk(argument) {
            Err(usage) => self.app.transcript.error(usage),
            Ok(AfkRequest::Report { all }) => self.afk_show(runtime, all),
            Ok(AfkRequest::Review) => return self.afk_review(runtime).await,
        }
        ControlFlow::Continue(())
    }

    /// The part of a temperature stop that follows the dropped turn: the agents and jobs are
    /// killed, the goal pauses, the stretch is reviewed, and the user is told.
    pub(crate) async fn afk_stopped(&mut self, runtime: &AgentRuntime) {
        self.app.transcript.notice(
            "AFK: the temperature ceiling was reached. Stopping every agent and job, then reviewing the stretch…",
        );
        self.draw(runtime);
        if let Some(report) = runtime.handle_afk_stop().await {
            self.app
                .transcript
                .notice(reports::afk_outcome_notice(&report));
        }
        self.show_goal_pause(runtime);
        self.app
            .transcript
            .notice("No tool call runs and no scheduled prompt fires until you run /afk review.");
    }

    /// Announces, once, that repeated denials have blocked an unattended session.
    pub(crate) async fn announce_blocked(&mut self, runtime: &AgentRuntime) {
        if !runtime.block_unannounced() {
            return;
        }
        self.app
            .transcript
            .notice("AFK: repeated denials blocked the session. Reviewing the stretch…");
        self.draw(runtime);
        if let Some(report) = runtime.note_blocked().await {
            self.app
                .transcript
                .notice(reports::afk_outcome_notice(&report));
            self.show_goal_pause(runtime);
        }
    }

    /// Says why the goal waits, when AFK is what paused it.
    fn show_goal_pause(&mut self, runtime: &AgentRuntime) {
        if let Some(active) = runtime.session.goal().status(runtime.tree_spend()).active
            && let Some(reason @ (PauseReason::AfkStopped | PauseReason::Blocked(_))) =
                active.paused
        {
            self.app.transcript.notice(reason.notice());
        }
    }

    pub(crate) fn afk_show(&mut self, runtime: &AgentRuntime, all: bool) {
        match runtime.afk_reports(all) {
            Ok(reports) if reports.is_empty() => self
                .app
                .transcript
                .notice("no AFK stretch has run in this session"),
            Ok(reports) => {
                let text = reports
                    .iter()
                    .map(reports::afk_report)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                self.app.transcript.notice(text);
            }
            Err(e) => self
                .app
                .transcript
                .error(fontaine_tools::error_chain::chain(&e)),
        }
    }

    /// `/afk review`: shows the report and, after a confirmation, lets a stopped session run
    /// again, in `manual` if it was in AFK mode. Nothing but the stop is cleared.
    async fn afk_review(&mut self, runtime: &mut AgentRuntime) -> ControlFlow<()> {
        if runtime.state.afk().stopped.is_none() {
            self.app
                .transcript
                .notice("no AFK stop is waiting for a review");
            return ControlFlow::Continue(());
        }
        let summary = match runtime.afk_reports(false) {
            Ok(mut reports) => reports.pop(),
            Err(e) => {
                self.app
                    .transcript
                    .error(fontaine_tools::error_chain::chain(&e));
                None
            }
        };
        let headline = match &summary {
            Some(report) => {
                self.app.transcript.notice(reports::afk_report(report));
                reports::afk_outcome_notice(report)
            }
            None => "The session was stopped by the AFK temperature ceiling.".to_string(),
        };
        self.app
            .begin_afk_review(crate::guardian_prompt::afk_review(headline));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.afk_review_prompt = None;
                return ControlFlow::Continue(());
            };
            match self.app.handle_input(input) {
                Action::AfkReview(true) => {
                    let was_afk = runtime.state.afk_on();
                    if runtime.review_afk_stop() {
                        self.app.mode = runtime.state.mode();
                        self.app.afk = runtime.state.afk_on();
                        self.app.transcript.notice(if was_afk {
                            "reviewed: the session can run again, now in manual mode"
                        } else {
                            "reviewed: the session can run again"
                        });
                    }
                    return ControlFlow::Continue(());
                }
                Action::AfkReview(false) => {
                    self.app.transcript.notice("the session stays stopped");
                    return ControlFlow::Continue(());
                }
                Action::Quit => return ControlFlow::Break(()),
                _ => {}
            }
        }
    }
}
