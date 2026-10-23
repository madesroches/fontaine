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
    pub background: bool,
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

    /// Resolves when the child has ended, with how it ended.
    fn report(&self) -> BoxFuture<'_, ChildReport>;
}

pub trait ChildRunner: Send + Sync {
    fn spawn<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>>;
}
