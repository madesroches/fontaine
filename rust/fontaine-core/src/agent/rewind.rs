//! `/rewind` on the runtime: the same refusal guard as `/clear`, then the conversation, the
//! files and the ledger record.

use std::fmt;

use fontaine_tools::error_chain::chain;

use super::AgentRuntime;
use crate::checkpoint::{CodeReport, RewindPlan, RewindWhat, TurnSummary};
use crate::ledger::Record;
use crate::session::ClearError;

/// A rewind that changed nothing.
#[derive(Debug)]
pub enum RewindError {
    /// A subagent is running or has a report to deliver.
    SubagentsRunning,
    /// The conversation could not be put back.
    Conversation(ClearError),
}

impl fmt::Display for RewindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RewindError::SubagentsRunning => f.write_str(
                "subagents are still running or have reports to deliver; /rewind is refused until they finish or are killed",
            ),
            RewindError::Conversation(_) => f.write_str("the conversation could not be rewound"),
        }
    }
}

impl std::error::Error for RewindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RewindError::SubagentsRunning => None,
            RewindError::Conversation(e) => Some(e),
        }
    }
}

/// What a rewind did.
#[derive(Debug)]
pub struct RewindOutcome {
    /// The rewound prompt, for the input box.
    pub prompt: String,
    pub conversation_restored: bool,
    /// `None` when the rewind did not include code.
    pub code: Option<CodeReport>,
}

impl AgentRuntime {
    /// The human submitted a prompt: notes the point a later rewind comes back to. `recorded`
    /// says the prompt's ledger message was appended already. A checkpoint that cannot be made
    /// is reported; the turn goes ahead.
    pub fn begin_turn(&self, prompt: &str, recorded: bool) {
        if let Err(e) = self.session.begin_turn(prompt, recorded) {
            self.approver
                .warn(&format!("this prompt cannot be rewound to: {}", chain(&e)));
        }
    }

    /// The prompts of the current `/clear` epoch a rewind may go back to, oldest first.
    pub fn rewind_turns(&self) -> Vec<TurnSummary> {
        self.session.checkpoints().turns()
    }

    /// What going back to `turn` would do to the files.
    pub fn rewind_plan(&self, turn: u64) -> Option<RewindPlan> {
        self.session.checkpoints().plan(turn, &self.state.scope())
    }

    /// `/rewind`. The conversation goes first because it can fail without side effects; the
    /// files follow, best effort, with the failures in the report. Files changed since the
    /// last tool call are overwritten only with `overwrite_changed`.
    pub async fn rewind(
        &self,
        plan: &RewindPlan,
        what: RewindWhat,
        overwrite_changed: bool,
    ) -> Result<RewindOutcome, RewindError> {
        let agents = self.state.agents();
        if agents.any_running() || agents.has_agent_notifications() {
            return Err(RewindError::SubagentsRunning);
        }
        let checkpoints = self.session.checkpoints();
        if what.includes_conversation() {
            self.session
                .rewind_conversation(plan.conv_n)
                .await
                .map_err(RewindError::Conversation)?;
            self.end_jobs().await;
        }
        let code = what
            .includes_code()
            .then(|| checkpoints.restore_code(plan, &self.state.scope(), overwrite_changed));
        let dropped_user_messages = if what.includes_conversation() {
            let held = self.session.ledger.view().user_messages.len();
            held.saturating_sub(plan.user_messages_before)
        } else {
            0
        };
        self.state.append(Record::Rewound {
            turn: plan.turn,
            what,
            dropped_user_messages,
        });
        if what.includes_conversation() {
            let restored = code.as_ref().map(|report| {
                (report.restored.iter().chain(&report.deleted))
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            });
            checkpoints.mark_rewound(plan.turn, what.includes_code(), restored);
            // The goal records no turn, so any conversation rewind clears it, as /clear does.
            self.clear_goal();
        }
        if let Some(report) = &code
            && !report.failed.is_empty()
        {
            for (path, e) in &report.failed {
                self.approver.warn(&format!(
                    "{} was not restored: {}",
                    path.display(),
                    chain(e)
                ));
            }
        }
        Ok(RewindOutcome {
            prompt: plan.prompt.clone(),
            conversation_restored: what.includes_conversation(),
            code,
        })
    }
}
