//! A side-effect-free tool for exercising the model round trip. It exists only so headless
//! tests can drive a tool call; the shipped binary registers it solely under a hidden
//! environment variable.

use std::convert::Infallible;

use rig_agent::tool::Tool;
use rig_core::tool::ToolContext;
use serde::Deserialize;
use serde_json::json;

pub const ENABLE_VAR: &str = "FONTAINE_SPIKE_ECHO_TOOL";

pub struct Echo;

#[derive(Deserialize)]
pub struct EchoArgs {
    text: String,
}

impl Tool for Echo {
    const NAME: &'static str = "echo";
    type Args = EchoArgs;
    type Output = String;
    type Error = Infallible;

    fn description(&self) -> String {
        "Returns the given text unchanged.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "text": { "type": "string", "description": "The text to return." } },
            "required": ["text"],
        })
    }

    async fn call(&self, _context: &mut ToolContext, args: EchoArgs) -> Result<String, Infallible> {
        Ok(args.text)
    }
}
