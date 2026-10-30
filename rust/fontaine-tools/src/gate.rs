//! The seam between the tool wrappers and whatever decides whether a call may run. This crate
//! only defines it; the implementation lives in `fontaine-core`, so `fontaine-tools` never
//! depends on it.

use futures::future::BoxFuture;
use rig_core::tool::ContextValue;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::risk::RiskClass;

/// The agent making a call, carried in rig's per-call context so audit records can be keyed per
/// agent. A call that carries none is the root agent's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl ContextValue for AgentId {
    const KEY: &'static str = "fontaine.agent_id";
}

pub const ROOT_AGENT: &str = "root";

/// One tool call as the gate sees it.
#[derive(Clone, Copy, Debug)]
pub struct CallView<'a> {
    pub tool: &'a str,
    pub risk: RiskClass,
    pub args: &'a Value,
    pub agent: &'a str,
    /// What an MCP server declared about the tool, as untrusted text for the guardian. Never
    /// a gate: it changes no decision.
    pub annotations: Option<&'a str>,
    /// The MCP server marked this tool as needing a person's approval on every call. It can
    /// only add a prompt; it never allows.
    pub requires_user_interaction: bool,
    /// No model call made this: it is text screened for a new user-role turn (a notification
    /// prompt), so a stage that keeps per-agent state for the model's tool results skips it.
    pub synthetic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateDecision {
    /// Run the tool with these arguments, which may differ from the model's.
    Allow { args: Value },
    /// Do not run it; the message goes back to the model as the tool result.
    Deny { message: String },
}

pub trait ToolGate: Send + Sync {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision>;
}
