//! The `tool_search` tool: loads the definitions of deferred MCP tools. It reads only the
//! in-memory registry and takes no path. Loading a definition lets the model name the tool; the
//! call itself goes through the same gate as every other MCP call.

use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

use crate::gate::{AgentId, ROOT_AGENT};
use crate::mcp::deferral::{Deferral, LoadedSets, TOOL_SEARCH, search};

const TARGET: &str = "fontaine::telemetry";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSearchArgs {
    pub query: String,
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Clone)]
pub struct ToolSearchTool {
    pub deferral: Deferral,
    pub loaded: LoadedSets,
    /// Whether telemetry may carry the names of the tools loaded.
    pub reveal_names: bool,
}

impl Tool for ToolSearchTool {
    const NAME: &'static str = TOOL_SEARCH;
    type Args = ToolSearchArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Load the definitions of deferred MCP tools. Their names are listed in the system \
         prompt, but a deferred tool cannot be called until its definition is loaded here. \
         `query` is keywords matched against tool names and descriptions, or `select:name1,name2` \
         for exact names. The matching tools' descriptions and input schemas come back and the \
         tools are callable from your next turn."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords, or `select:name1,name2` to load tools by exact name.",
                },
                "max_results": {
                    "type": "integer",
                    "description": "At most this many tools are loaded (default 5, at most 25).",
                    "minimum": 1,
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: ToolSearchArgs,
    ) -> Result<String, ToolExecutionError> {
        let agent = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        if !self.deferral.is_active() {
            return Ok(
                "No tools are deferred: every tool's definition is already available.".to_string(),
            );
        }
        let candidates = self
            .deferral
            .definitions(&|name| self.loaded.visible(&agent, name));
        let found = search(&args.query, args.max_results, &candidates);
        tracing::info!(
            target: TARGET,
            deferred = candidates.len(),
            matched = found.len(),
            tools = %if self.reveal_names {
                found.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(",")
            } else {
                String::new()
            },
            "fontaine.tool_search"
        );
        if found.is_empty() {
            return Ok(format!(
                "No deferred tool matches `{}`. {} tools are deferred; try other keywords, or \
                 `select:<name>` with a name from the system prompt.",
                args.query.trim(),
                candidates.len()
            ));
        }
        self.loaded
            .load(&agent, found.iter().map(|tool| tool.name.to_string()));
        let rendered: Vec<serde_json::Value> = found
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name.as_str(),
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect();
        let body = serde_json::to_string_pretty(&rendered)
            .map_err(|e| ToolExecutionError::other(crate::error_chain::chain(&e)))?;
        Ok(format!(
            "Loaded {} tool(s); call them by name from your next turn.\n{body}",
            found.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::ToolDefinition;
    use rig_core::message::ToolName;

    fn def(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition::new(
            ToolName::new(name).expect("a test tool name"),
            description,
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )
    }

    fn tool(mode: crate::mcp::DeferMode) -> ToolSearchTool {
        let deferral = Deferral::default();
        deferral.set_policy(mode, None);
        let c = deferral.begin_connection();
        deferral.record(
            "gh",
            c,
            vec![
                def("mcp__gh__issue", "Open an issue"),
                def("mcp__gh__pr", "Open a pull request"),
            ],
        );
        ToolSearchTool {
            deferral,
            loaded: LoadedSets::default(),
            reveal_names: false,
        }
    }

    #[test]
    fn args_reject_unknown_fields() {
        assert!(serde_json::from_value::<ToolSearchArgs>(json!({"query": "a", "x": 1})).is_err());
        assert!(serde_json::from_value::<ToolSearchArgs>(json!({})).is_err());
    }

    #[tokio::test]
    async fn a_match_is_loaded_for_the_calling_agent_only_and_rendered_with_its_schema() {
        let tool = tool(crate::mcp::DeferMode::On);
        let mut context = ToolContext::default();
        context.insert(AgentId("child-1".into())).unwrap();
        let out = tool
            .call(
                &mut context,
                ToolSearchArgs {
                    query: "issue".into(),
                    max_results: None,
                },
            )
            .await
            .unwrap();
        assert!(
            out.contains("mcp__gh__issue") && out.contains("input_schema"),
            "{out}"
        );
        assert!(!out.contains("mcp__gh__pr"), "{out}");
        assert_eq!(
            tool.loaded.loaded("child-1", &tool.deferral),
            ["mcp__gh__issue"]
        );
        assert!(tool.loaded.loaded(ROOT_AGENT, &tool.deferral).is_empty());
    }

    #[tokio::test]
    async fn a_miss_loads_nothing_and_says_how_many_are_deferred() {
        let tool = tool(crate::mcp::DeferMode::On);
        let out = tool
            .call(
                &mut ToolContext::default(),
                ToolSearchArgs {
                    query: "zebra".into(),
                    max_results: None,
                },
            )
            .await
            .unwrap();
        assert!(out.contains("2 tools are deferred"), "{out}");
        assert!(tool.loaded.loaded(ROOT_AGENT, &tool.deferral).is_empty());
    }

    #[tokio::test]
    async fn with_deferral_off_there_is_nothing_to_load() {
        let tool = tool(crate::mcp::DeferMode::Off);
        let out = tool
            .call(
                &mut ToolContext::default(),
                ToolSearchArgs {
                    query: "issue".into(),
                    max_results: None,
                },
            )
            .await
            .unwrap();
        assert!(out.contains("No tools are deferred"), "{out}");
        assert!(tool.loaded.loaded(ROOT_AGENT, &tool.deferral).is_empty());
    }

    #[tokio::test]
    async fn an_agent_cannot_load_a_tool_outside_its_toolset() {
        let tool = tool(crate::mcp::DeferMode::On);
        tool.loaded
            .enroll("child-1", std::sync::Arc::new(|n| n == "mcp__gh__pr"));
        let mut context = ToolContext::default();
        context.insert(AgentId("child-1".into())).unwrap();
        let out = tool
            .call(
                &mut context,
                ToolSearchArgs {
                    query: "select:mcp__gh__issue,mcp__gh__pr".into(),
                    max_results: None,
                },
            )
            .await
            .unwrap();
        assert!(
            out.contains("mcp__gh__pr") && !out.contains("mcp__gh__issue"),
            "{out}"
        );
    }
}
