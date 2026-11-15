//! Runs the guarded agent in process: one host tool, an approver that allows it, and every event
//! printed as the `headless-v1` JSON line `fontaine -p` would write.
//!
//! `cargo run -p fontaine-sdk --example embed -- "what is 6 times 7?"`

use std::sync::Arc;

use fontaine_sdk::permissions::{Answer, ApprovalRequest};
use fontaine_sdk::{
    Approver, BoxFuture, HostTool, PermissionMode, SessionBuilder, ToolExecutionError,
};
use futures::StreamExt;
use serde_json::{Value, json};

struct Multiply;

impl HostTool for Multiply {
    fn name(&self) -> &str {
        "multiply"
    }

    fn description(&self) -> &str {
        "Multiplies two numbers."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}, "required": ["a", "b"]})
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<String, ToolExecutionError>> {
        Box::pin(async move {
            let (a, b) = (args["a"].as_f64(), args["b"].as_f64());
            match (a, b) {
                (Some(a), Some(b)) => Ok((a * b).to_string()),
                _ => Err(ToolExecutionError::refused("a and b must be numbers")),
            }
        })
    }
}

struct AllowMultiply;

impl Approver for AllowMultiply {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        let answer = if request.tool == "multiply" {
            Answer::AllowOnce
        } else {
            Answer::Deny { feedback: None }
        };
        Box::pin(async move { answer })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "what is 6 times 7?".into());
    let mut session = SessionBuilder::new(std::env::current_dir()?)
        .mode(PermissionMode::Manual)
        .approver(Arc::new(AllowMultiply))
        .tool(Multiply)
        .build()
        .await?;
    let mut turn = session.send(prompt);
    while let Some(event) = turn.next().await {
        println!("{}", serde_json::to_string(&event?)?);
    }
    drop(turn);
    session.close().await?;
    Ok(())
}
