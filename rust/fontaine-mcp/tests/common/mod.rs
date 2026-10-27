#![allow(dead_code)]

pub mod server;

use std::sync::{Arc, Mutex};

use fontaine_tools::gate::{AgentId, CallView, GateDecision, ROOT_AGENT, ToolGate};
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::RiskClass;
use futures::future::BoxFuture;
use rig_agent::tool::server::{ToolServer, ToolServerHandle};
use rig_core::tool::ToolContext;
use serde_json::Value;

/// What the gate was asked.
#[derive(Clone, Debug)]
pub struct Seen {
    pub tool: String,
    pub risk: RiskClass,
    pub annotations: Option<String>,
    pub requires_user_interaction: bool,
    pub args: Value,
}

/// A gate that records every call and answers as told.
pub struct Gate {
    pub allow: bool,
    pub seen: Mutex<Vec<Seen>>,
}

impl Gate {
    pub fn new(allow: bool) -> Arc<Gate> {
        Arc::new(Gate {
            allow,
            seen: Mutex::new(Vec::new()),
        })
    }

    pub fn calls(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl ToolGate for Gate {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        self.seen.lock().unwrap().push(Seen {
            tool: call.tool.to_string(),
            risk: call.risk,
            annotations: call.annotations.map(str::to_string),
            requires_user_interaction: call.requires_user_interaction,
            args: call.args.clone(),
        });
        let decision = if self.allow {
            GateDecision::Allow {
                args: call.args.clone(),
            }
        } else {
            GateDecision::Deny {
                message: "denied by the test gate".to_string(),
            }
        };
        Box::pin(async move { decision })
    }
}

pub fn guard(gate: Arc<Gate>) -> Guard {
    Guard {
        gate,
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: false,
        pre_images: None,
    }
}

pub fn tool_server() -> ToolServerHandle {
    ToolServer::new().run()
}

pub fn names(handle: &ToolServerHandle) -> Vec<String> {
    let mut names: Vec<String> = handle.toolset().names().map(str::to_owned).collect();
    names.sort();
    names
}

/// Calls the registered tool `name` as the model would.
pub async fn call(handle: &ToolServerHandle, name: &str, args: &str) -> Result<String, String> {
    let result = handle.toolset().execute(name, args, &mut as_root()).await;
    match result.into_result() {
        Ok(output) => Ok(output.render()),
        Err(e) => Err(e.model_output().render()),
    }
}

fn as_root() -> ToolContext {
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    context
}
