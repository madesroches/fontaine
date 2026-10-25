//! Registers an MCP server's tools into the agent's tool server, wrapped. rig-rmcp registers raw
//! tools with whatever sink it is given, and again on every list-changed refresh, so the sink
//! it gets is this one: nothing an MCP server offers is ever reachable unwrapped.

use std::collections::HashMap;
use std::sync::Arc;

use crate::interaction::Marked;
use fontaine_tools::guarded::{Guard, guard_callback};
use fontaine_tools::risk::RiskClass;
use rig_core::message::ToolName;
use rig_core::serve::adapters::ToolCallback;
use rig_core::tool::{DynamicTool, ManagedToolSink, ManagedToolToken, ToolContext};
use serde_json::Value;
use tracing::Instrument;

const TARGET: &str = "fontaine::telemetry";

/// What exporters see in place of a server or tool name when names are not opted in.
const HIDDEN: &str = "third-party";

fn clean(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `mcp__<server>__<tool>`, with characters the permission grammar cannot name replaced.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", clean(server), clean(tool))
}

fn as_callback<F: ToolCallback + 'static>(callback: F) -> Arc<dyn ToolCallback> {
    Arc::new(callback)
}

/// A [`ManagedToolSink`] that wraps every tool it is handed before passing it on.
pub struct GuardedSink<S> {
    pub inner: S,
    pub server: String,
    pub guard: Guard,
    /// Whether telemetry may carry the server and tool names.
    pub reveal_names: bool,
    marked: Marked,
}

impl<S> GuardedSink<S> {
    pub(crate) fn new(
        inner: S,
        server: &str,
        guard: Guard,
        reveal_names: bool,
        marked: Marked,
    ) -> Self {
        GuardedSink {
            inner,
            server: server.to_string(),
            guard,
            reveal_names,
            marked,
        }
    }

    fn wrap(&self, tool: DynamicTool) -> Option<DynamicTool> {
        let mut definition = tool.definition();
        let original_name = definition.name.to_string();
        let requires_user_interaction = self.marked.contains(&original_name);
        let name = match ToolName::new(mcp_tool_name(&self.server, &original_name)) {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!(error = %e, "skipping an MCP tool with an unusable name");
                return None;
            }
        };
        definition.name = name;
        let original = Arc::new(tool);
        let probe = original.clone();
        let (server, tool_label) = if self.reveal_names {
            (self.server.clone(), original_name)
        } else {
            (HIDDEN.to_string(), HIDDEN.to_string())
        };
        let callback = as_callback(move |context: &mut ToolContext, args: Value| {
            let original = original.clone();
            let span = tracing::info_span!(
                target: TARGET,
                "fontaine.mcp.call",
                mcp.server = server.as_str(),
                mcp.tool = tool_label.as_str(),
            );
            Box::pin(async move { original.run_with(context, args).await }.instrument(span))
        });
        // Declared hints travel with the definition as text for the guardian; the class is
        // fixed here, whatever the server says. The interaction marker is the one declared
        // value that is more than text, and it can only add a prompt.
        Some(
            guard_callback(
                definition,
                callback,
                RiskClass::Arbitrary,
                self.guard.clone(),
                requires_user_interaction,
            )
            .with_liveness(move || probe.is_live()),
        )
    }

    fn wrap_all(&self, tools: Vec<DynamicTool>) -> Vec<DynamicTool> {
        tools
            .into_iter()
            .filter_map(|tool| self.wrap(tool))
            .collect()
    }
}

impl<S: ManagedToolSink> ManagedToolSink for GuardedSink<S> {
    fn add_managed_tools(&self, tools: Vec<DynamicTool>) -> HashMap<String, ManagedToolToken> {
        self.inner.add_managed_tools(self.wrap_all(tools))
    }

    fn reconcile_managed_tools(
        &self,
        expected: HashMap<String, ManagedToolToken>,
        tools: Vec<DynamicTool>,
    ) -> HashMap<String, ManagedToolToken> {
        self.inner
            .reconcile_managed_tools(expected, self.wrap_all(tools))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_claude_code_and_stay_inside_the_rule_grammar() {
        assert_eq!(
            mcp_tool_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(mcp_tool_name("gh", "list.repos"), "mcp__gh__list_repos");
        assert_eq!(mcp_tool_name("a-b", "x y"), "mcp__a-b__x_y");
    }
}
