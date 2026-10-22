//! The `skill` tool: loads a skill's instructions into the conversation. Its only argument is a
//! skill name, resolved against the index built at startup, so it can read nothing else; what
//! the skill's body then asks for goes through the permission pipeline like any other call.

use std::sync::Arc;

use futures::future::BoxFuture;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

use crate::gate::{AgentId, ROOT_AGENT};

/// Where skill text comes from. `fontaine-core` implements it over the loaded extension files.
pub trait SkillSource: Send + Sync {
    /// The text of the skill `name` as `agent` loads it, or why it cannot be loaded.
    fn load<'a>(&'a self, name: &'a str, agent: &'a str) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillArgs {
    pub name: String,
}

#[derive(Clone)]
pub struct SkillTool {
    pub source: Arc<dyn SkillSource>,
}

impl Tool for SkillTool {
    const NAME: &'static str = "skill";
    type Args = SkillArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Load a skill: instructions for a kind of task, named in the skill list of the system \
         prompt. Takes the skill's `name` and returns its text, which you then follow."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill's name, from the skill list." },
            },
            "required": ["name"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: SkillArgs,
    ) -> Result<String, ToolExecutionError> {
        let agent = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        self.source
            .load(&args.name, &agent)
            .await
            .map_err(ToolExecutionError::invalid_args)
    }
}
