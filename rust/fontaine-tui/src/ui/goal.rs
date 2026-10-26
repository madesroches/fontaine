//! `/goal` and the turns it leads to: the evaluation between turns, its verdict notices and
//! the goal turn that follows a verdict of not met.

use std::ops::ControlFlow::{self, Break, Continue};

use fontaine_core::agent::AgentRuntime;
use fontaine_core::goal::{Applied, GoalArg, Judgment, Step, first_turn_prompt, parse_goal_arg};
use fontaine_core::ledger::{GoalEvent, Record};
use fontaine_core::turn::TurnOutcome;
use fontaine_tools::error_chain::chain;

use crate::ui::turn::{EndOfTurn, Serve, TurnEnd, end_of_turn};
use crate::{App, Ui};

impl<R: FnMut(&mut App)> Ui<R> {
    pub(super) async fn goal_command(
        &mut self,
        runtime: &mut AgentRuntime,
        typed: &str,
        argument: &str,
    ) -> ControlFlow<()> {
        match parse_goal_arg(argument) {
            GoalArg::Status => {
                let status = runtime.session.goal().status(runtime.tree_spend());
                self.app.transcript.notice(status.describe());
            }
            GoalArg::Clear => match runtime.clear_goal() {
                Some(cleared) => self
                    .app
                    .transcript
                    .notice(format!("Goal cleared: {}", cleared.condition)),
                None => self.app.transcript.notice("No goal set"),
            },
            GoalArg::Set(condition) => {
                if let Err(e) = runtime.set_goal(&condition) {
                    self.app.transcript.error(chain(&e));
                    return Continue(());
                }
                self.app
                    .transcript
                    .notice(format!("Goal set: {}", condition.trim()));
                // The first turn is the user's: the line they typed is the intent the
                // guardian reads, and later goal turns are harness notes.
                runtime.append(Record::UserMessage {
                    text: typed.to_string(),
                });
                return self
                    .converse(
                        runtime,
                        typed.to_string(),
                        Some(first_turn_prompt(&condition)),
                    )
                    .await;
            }
        }
        Continue(())
    }

    /// Shows how a turn ended for the goal: a pause the turn caused.
    pub(super) fn goal_after_turn(&mut self, runtime: &AgentRuntime, end: &TurnEnd) {
        let paused = match end {
            TurnEnd::Finished(outcome) => runtime.goal_turn_ended(Some(outcome)),
            TurnEnd::Interrupted => runtime.goal_turn_ended(None),
            TurnEnd::InputClosed => None,
        };
        if let Some(reason) = paused {
            self.app.transcript.notice(reason.notice());
        }
    }

    /// Evaluates the goal when a turn has ended with one due, then runs what the verdict leads
    /// to. `None` when nothing was due.
    pub(super) async fn goal_step(
        &mut self,
        runtime: &mut AgentRuntime,
    ) -> Option<ControlFlow<()>> {
        let pending = runtime.next_goal_step()?;
        self.app.running = true;
        let mut evaluation = Box::pin(runtime.evaluate_goal(&pending));
        let result = self
            .serve_while(runtime, &mut evaluation, Serve::Queueing)
            .await;
        drop(evaluation);
        self.app.running = false;
        let applied = match result {
            Ok(applied) => applied,
            Err(TurnEnd::InputClosed) => return Some(Break(())),
            Err(end) => {
                self.app.deny_pending();
                self.drain_prompts();
                self.goal_after_turn(runtime, &end);
                self.app.transcript.notice("interrupted");
                if let EndOfTurn::RestoreToInput(back) =
                    end_of_turn(&end, runtime.steering.retract())
                {
                    self.app.restore_to_input(&back);
                }
                return Some(Continue(()));
            }
        };
        let step = applied.as_ref().map(|applied| self.show_verdict(applied));
        // What the human typed during the evaluation is the next turn, ahead of any goal turn.
        let finished = TurnEnd::Finished(TurnOutcome::Answer(String::new()));
        if let EndOfTurn::Submit(next) = end_of_turn(&finished, runtime.steering.retract()) {
            return Some(self.converse(runtime, next, None).await);
        }
        match step.flatten() {
            Some(next) => Some(self.goal_turn(runtime, next).await),
            None => Some(Continue(())),
        }
    }

    /// Shows a verdict and hands back the turn it asks for, if any.
    fn show_verdict(&mut self, applied: &Applied) -> Option<fontaine_core::goal::Continue> {
        let transcript = &mut self.app.transcript;
        match &applied.step {
            Step::Continue(next) => {
                transcript.notice(format!("goal: not met — {}", next.reason));
                return Some(next.clone());
            }
            Step::Done { event, summary } => {
                transcript.notice(match event {
                    GoalEvent::Met => summary.achieved(),
                    _ => format!("Goal impossible: {}", applied.reason),
                });
            }
            Step::Pause(reason) => {
                if applied.judgment == Some(Judgment::NotMet) {
                    transcript.notice(format!("goal: not met — {}", applied.reason));
                }
                transcript.notice(reason.notice());
            }
        }
        None
    }
}
