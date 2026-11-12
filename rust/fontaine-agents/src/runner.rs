//! The seam between the agent tools and whatever runs a child. This crate defines it and
//! `fontaine-core` implements it, so the spawn logic, registry and budgets work and test with no
//! dependency on the core.

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::budget::{Budget, Spend};
use crate::mode::Ladder;
use crate::registry::{AgentType, ModelSpec};
use crate::root_ledger::RootLedger;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildStatus {
    Running,
    /// Blocked on a human's answer to one of its calls.
    WaitingOnPermission,
    Done,
    Killed,
    BudgetExhausted,
    Failed,
}

impl ChildStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildStatus::Running => "running",
            ChildStatus::WaitingOnPermission => "waiting on permission",
            ChildStatus::Done => "done",
            ChildStatus::Killed => "killed",
            ChildStatus::BudgetExhausted => "budget exhausted",
            ChildStatus::Failed => "failed",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(
            self,
            ChildStatus::Running | ChildStatus::WaitingOnPermission
        )
    }
}

/// Everything a runner needs to start one child.
pub struct SpawnRequest {
    pub id: String,
    pub parent: String,
    pub agent_type: AgentType,
    pub prompt: String,
    /// The model after precedence: the call's, else the type's, else the session's.
    pub model: ModelSpec,
    /// The tools the child gets, already cut down to the parent's.
    pub tools: Vec<String>,
    /// The mode the child starts with; the runner holds the child to the tighter of this and
    /// its parent's mode on every call.
    pub mode: Ladder,
    pub budget: Budget,
    /// Layers below the main conversation.
    pub depth: usize,
    pub ledger: Arc<dyn RootLedger>,
}

/// Everything a runner needs to rebuild the conversation of an agent a resumed session brought
/// back: a [`SpawnRequest`] without a prompt.
pub struct RevivalRequest {
    pub id: String,
    pub parent: String,
    pub agent_type: AgentType,
    pub model: ModelSpec,
    pub tools: Vec<String>,
    pub mode: Ladder,
    pub budget: Budget,
    pub depth: usize,
    pub ledger: Arc<dyn RootLedger>,
}

/// How a child ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildReport {
    pub status: ChildStatus,
    /// What the child said last. Partial when `status` is not `Done`.
    pub text: String,
    /// Why it stopped early, or how it failed.
    pub reason: Option<String>,
}

pub trait ChildHandle: Send + Sync {
    fn status(&self) -> ChildStatus;

    fn spend(&self) -> Spend;

    /// Stops the child. Its pending approvals are answered as denied.
    fn kill(&self);

    /// Ends the child's tasks at once, for the session's final exit: no rig hook boundary is
    /// awaited and no report is produced. A handle with no task of its own falls back to
    /// [`kill`](Self::kill).
    fn abort(&self) {
        self.kill();
    }

    /// Resolves when the child has ended, with how it ended. After a [`resume`](Self::resume)
    /// it resolves with the report of the run that is current when it is called.
    fn report(&self) -> BoxFuture<'_, ChildReport>;

    /// Starts another run on the same conversation with `prompt`, the human's words. The caller
    /// has already moved the node back to running in the tree.
    fn resume(&self, _prompt: String) {}

    /// Starts another run like [`resume`](Self::resume), on `text` that is not the human's: a
    /// peer's message, a hand-off note or a hook's reason. It must never become intent for the
    /// guardian, so an implementation does not record it as a human message.
    fn resume_with_data(&self, text: String) {
        self.resume(text);
    }

    /// Takes the messages the human typed to this child that its last run ended without
    /// reading.
    fn take_missed(&self) -> Vec<String> {
        Vec::new()
    }

    /// Tells the human that `text` could not be delivered because an automatic resume was
    /// refused.
    fn report_undelivered(&self, _refusal: &str, _text: &str) {}
}

pub trait ChildRunner: Send + Sync {
    fn spawn<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>>;

    /// Builds the handle of an agent whose conversation an earlier process left on disk, without
    /// starting a run. The caller resumes it.
    fn revive<'a>(
        &'a self,
        _request: RevivalRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>> {
        Box::pin(async { Err("this runner cannot revive an agent".to_string()) })
    }

    /// Drops what the human typed to `id` while its revival was pending, which no run will read.
    fn discard_unread(&self, _id: &str) {}
}
