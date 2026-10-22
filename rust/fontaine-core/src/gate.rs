//! The permission decision for a tool call. This build ships a deny-by-default stub: it allows
//! only a read-only call whose paths all lie inside the workspace roots, and only when no
//! `deny` or `ask` rule names the tool. It matches no specifiers, so it over-denies rather than
//! guess.

use std::sync::Arc;

use fontaine_tools::gate::{CallView, GateDecision, ToolGate};
use fontaine_tools::risk::RiskClass;
use fontaine_tools::scope::Scope;
use futures::future::BoxFuture;

use crate::config::PermissionMode;
use crate::ledger::{Ledger, Record};
use crate::permission_rules::Rule;

/// What the gate needs to know about the session, shared by every wrapped tool.
pub struct SessionState {
    pub mode: PermissionMode,
    pub scope: Arc<Scope>,
    pub deny: Vec<Rule>,
    pub ask: Vec<Rule>,
    pub allow: Vec<Rule>,
    pub ledger: Arc<Ledger>,
}

pub struct StubGate {
    state: Arc<SessionState>,
}

impl StubGate {
    pub fn new(state: Arc<SessionState>) -> StubGate {
        StubGate { state }
    }

    fn named_by_a_rule(&self, tool: &str) -> bool {
        self.state
            .deny
            .iter()
            .chain(&self.state.ask)
            .any(|rule| rule.tool == tool)
    }

    fn decide(&self, call: &CallView<'_>) -> GateDecision {
        let tool = call.tool;
        if self.named_by_a_rule(tool) {
            return deny(format!(
                "`{tool}` was denied: a permission rule in the settings names it. Do not retry \
                 it; tell the user what you could not do."
            ));
        }
        if call.risk != RiskClass::ReadOnly {
            return deny(format!(
                "`{tool}` was denied: only read-only tools inside the workspace roots can run \
                 right now. Do not retry it or look for another route to the same effect; tell \
                 the user what you could not do."
            ));
        }
        match self.state.scope.locate(tool, call.args) {
            Ok(_) => GateDecision::Allow {
                args: call.args.clone(),
            },
            Err(e) => deny(format!(
                "`{tool}` was denied: {e}. Read-only tools work inside the workspace roots only."
            )),
        }
    }
}

fn deny(message: String) -> GateDecision {
    GateDecision::Deny { message }
}

impl ToolGate for StubGate {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        Box::pin(async move {
            let decision = self.decide(&call);
            let record = match &decision {
                GateDecision::Allow { .. } => Record::Action {
                    tool: call.tool.to_string(),
                    agent: call.agent.to_string(),
                },
                GateDecision::Deny { message } => Record::Denial {
                    tool: call.tool.to_string(),
                    agent: call.agent.to_string(),
                    reason: message.clone(),
                },
            };
            if let Err(e) = self.state.ledger.append(record) {
                tracing::warn!(error = %e, "the session ledger could not be written");
            }
            decision
        })
    }
}
