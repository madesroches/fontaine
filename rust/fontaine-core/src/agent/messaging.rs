//! The human typing to a subagent from its view: a running agent reads the message with its
//! next tool result, an ended one is resumed with it.

use std::sync::Arc;

use fontaine_agents::root_ledger::RootLedger;
use fontaine_agents::runner::ChildStatus;
use fontaine_agents::spawn::{describe, resume_child};
use fontaine_agents::tree::Refusal;

use super::AgentRuntime;
use crate::child_runner::LedgerWindow;

/// What became of a message typed to a subagent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivered {
    /// The agent is running and reads it at its next tool result.
    Queued,
    /// The agent had ended and started another run with it.
    Resumed,
}

/// Why a message typed to a subagent was not delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRefusal {
    Unknown,
    /// It was killed or ran out of budget; both are final.
    Ended(ChildStatus),
    /// Its budget is spent, whatever its status says.
    BudgetSpent,
    /// Another agent running would break a limit.
    Limit(String),
}

impl MessageRefusal {
    /// The notice for the human who typed to `agent`.
    pub fn notice(&self, agent: &str) -> String {
        match self {
            MessageRefusal::Unknown => format!("there is no agent {agent}"),
            MessageRefusal::Ended(status) => {
                format!("{agent} {}; it cannot be resumed", ended_words(*status))
            }
            MessageRefusal::BudgetSpent => {
                format!("{agent}'s budget is spent; it cannot be resumed")
            }
            MessageRefusal::Limit(why) => format!("{agent} cannot be resumed: {why}"),
        }
    }
}

fn ended_words(status: ChildStatus) -> &'static str {
    match status {
        ChildStatus::Killed => "was killed",
        ChildStatus::BudgetExhausted => "stopped because its budget is spent",
        ChildStatus::Failed => "failed",
        ChildStatus::Done => "is done",
        ChildStatus::Running | ChildStatus::WaitingOnPermission => "is running",
    }
}

impl AgentRuntime {
    /// Delivers what the human typed in `id`'s view. Nothing is written to the ledger here: the
    /// words become intent for that agent only when its model reads them.
    pub fn message_agent(&self, id: &str, text: String) -> Result<Delivered, MessageRefusal> {
        let tree = self.state.agents();
        if !tree.is_child(id) {
            return Err(MessageRefusal::Unknown);
        }
        let mut text = text;
        if tree.is_running(id) {
            self.steering.queue_for(id, text.clone());
            // The tree and the queue are separate locks: the run can end between the check and
            // the queueing, after its end-of-run drain already looked.
            if tree.is_running(id) {
                return Ok(Delivered::Queued);
            }
            let back = self.steering.take_pending_for(id);
            if back.is_empty() {
                // The end-of-run drain took it; it only reached the agent if that resume ran.
                if tree.is_running(id) {
                    return Ok(Delivered::Resumed);
                }
                return match tree.status_of(id) {
                    Some(status) => Err(MessageRefusal::Ended(status)),
                    None => Err(MessageRefusal::Unknown),
                };
            }
            text = back.join("\n\n");
        }
        if tree.exhausted_of(id).is_some() {
            return Err(MessageRefusal::BudgetSpent);
        }
        match tree.status_of(id) {
            Some(ChildStatus::Done | ChildStatus::Failed) => {}
            Some(status) if !status.is_running() => return Err(MessageRefusal::Ended(status)),
            // Resumed by someone else in the meantime: its run reads the message.
            Some(_) => {
                self.steering.queue_for(id, text);
                return Ok(Delivered::Queued);
            }
            None => return Err(MessageRefusal::Unknown),
        }
        let handle = tree.handle_of(id).ok_or(MessageRefusal::Unknown)?;
        let ledger: Arc<dyn RootLedger> = Arc::new(LedgerWindow(self.state.clone()));
        match resume_child(tree, &ledger, id, &handle, text.clone()) {
            Ok(()) => Ok(Delivered::Resumed),
            Err(Refusal::AlreadyRunning) => {
                // Another resume won the race; its run reads the message.
                self.steering.queue_for(id, text);
                Ok(Delivered::Queued)
            }
            Err(refusal) => Err(MessageRefusal::Limit(describe(&refusal))),
        }
    }
}
