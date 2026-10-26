//! `/goal` on the runtime: setting and clearing, the evaluator call and the prompt of a goal
//! turn. The state and the step logic are in `crate::goal`.

use fontaine_agents::budget::Spend;
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::ROOT_AGENT;
use rig_memory::load_async;

use super::AgentRuntime;
use crate::gate::spend_of;
use crate::goal::{
    Applied, Cleared, Continue, Evaluation, GoalCaps, GoalError, GoalEvaluator, PauseReason,
    Pending, REASON_CHARS, Step, cut, goal_prompt_text, transcript_of,
};
use crate::ledger::{GoalEvent, Record};
use crate::session::{CONVERSATION_LOG, CompactReason, CompactResult};
use crate::turn::TurnOutcome;

impl AgentRuntime {
    /// What the session has spent so far: the root's spend, which holds the main model, the
    /// guardian and the goal evaluator, plus every subagent's. A goal measures itself by the
    /// change in this.
    pub fn tree_spend(&self) -> Spend {
        let tree = self.state.agents();
        let mut spend = tree.spend_of(ROOT_AGENT);
        spend.add(&tree.subagent_spend());
        spend
    }

    /// [`Session::compact_if_needed`](crate::session::Session::compact_if_needed) on the main
    /// model, with the summarizer's usage debited to the root so spend caps count it.
    pub async fn compact_if_needed(&self) -> CompactResult {
        self.compact_with(CompactReason::Threshold).await
    }

    /// [`Session::compact_logged`](crate::session::Session::compact_logged) on the main model,
    /// debited like [`compact_if_needed`](Self::compact_if_needed).
    pub async fn compact_with(&self, reason: CompactReason) -> CompactResult {
        let result = self
            .session
            .compact_logged(
                &self.models.main.completion,
                self.models.main.catalog_billed(),
                reason,
                self.approver(),
            )
            .await;
        if let Some(usage) = result.usage() {
            self.state.agents().debit(ROOT_AGENT, &spend_of(usage));
        }
        result
    }

    fn forward_goal_warning(&self) {
        if let Some(warning) = self.session.goal().take_warning() {
            self.approver.warn(&warning);
        }
    }

    /// `/goal <condition>`: replaces any active goal. The surface then runs the first turn.
    pub fn set_goal(&self, condition: &str) -> Result<(), GoalError> {
        self.session.goal().set(condition, self.tree_spend())?;
        self.state.append(Record::Goal {
            event: GoalEvent::Set,
            evaluations: 0,
        });
        Ok(())
    }

    /// `/goal clear`.
    pub fn clear_goal(&self) -> Option<Cleared> {
        let cleared = self.session.goal().clear();
        if let Some(cleared) = &cleared {
            self.state.append(Record::Goal {
                event: GoalEvent::Cleared,
                evaluations: cleared.evaluations,
            });
        }
        self.forward_goal_warning();
        cleared
    }

    /// A turn ended; `None` is an interrupted one. Returns the pause this caused.
    pub fn goal_turn_ended(&self, end: Option<&TurnOutcome>) -> Option<PauseReason> {
        let goal = self.session.goal();
        let paused = goal.after_turn(end);
        if paused.is_some() {
            self.state.append(Record::Goal {
                event: GoalEvent::Paused,
                evaluations: goal
                    .status(self.tree_spend())
                    .active
                    .map_or(0, |a| a.evaluations),
            });
        }
        paused
    }

    /// The user sent a prompt: a paused goal runs again.
    pub fn goal_user_prompt(&self) {
        self.session.goal().user_prompt(self.tree_spend());
    }

    /// The evaluation due now, if any; none while a background agent still runs.
    pub fn next_goal_step(&self) -> Option<Pending> {
        let running = self.state.agents().any_running();
        self.session.goal().next_step(running)
    }

    /// Asks the evaluator and applies its answer; `None` when the goal was cleared meanwhile.
    pub async fn evaluate_goal(&self, pending: &Pending) -> Option<Applied> {
        let evaluation = self.judge_goal(pending).await;
        let applied = self.session.goal().apply(
            evaluation,
            self.tree_spend(),
            &GoalCaps::from(&self.config.goal),
            self.models.main.priced(),
        );
        if let Some(applied) = &applied {
            let event = match &applied.step {
                Step::Done { event, .. } => Some(*event),
                Step::Pause(_) => Some(GoalEvent::Paused),
                Step::Continue(_) => None,
            };
            if let Some(event) = event {
                self.state.append(Record::Goal {
                    event,
                    evaluations: applied.evaluations,
                });
            }
        }
        self.forward_goal_warning();
        applied
    }

    async fn judge_goal(&self, pending: &Pending) -> Evaluation {
        let evaluator = match GoalEvaluator::on(self.models.guardian.as_ref()) {
            Ok(evaluator) => evaluator,
            Err(why) => return Evaluation::Failed(why),
        };
        let conversation = match load_async(&self.session.store(), CONVERSATION_LOG).await {
            Ok(conversation) => conversation,
            Err(e) => {
                return Evaluation::Failed(format!(
                    "the conversation could not be read: {}",
                    chain(&e)
                ));
            }
        };
        let judged = evaluator
            .evaluate(&pending.condition, &transcript_of(&conversation))
            .await;
        self.state.add_goal_usage(&judged.usage);
        judged.evaluation
    }

    /// The prompt for a goal turn. It is a harness note, not the user speaking. The condition
    /// is the user's own typed text and goes through as written; the evaluator's reason is
    /// model output derived from tool results, so it passes through the result pipeline.
    pub async fn goal_prompt(&self, next: &Continue) -> String {
        // Cut before screening, so a note the scanner adds is never what the cut removes.
        let reason = self
            .screened_text("goal", &cut(&next.reason, REASON_CHARS))
            .await;
        goal_prompt_text(&reason, &next.condition)
    }
}
