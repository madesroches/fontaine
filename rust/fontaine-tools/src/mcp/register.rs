//! Registers an MCP server's tools into the agent's tool server, wrapped. rig-rmcp registers raw
//! tools with whatever sink it is given, and again on every list-changed refresh, so the sink
//! it gets is this one: nothing an MCP server offers is ever reachable unwrapped.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::Notify;

use super::elicitation::CallTracker;
use super::meta::ToolMeta;
use super::output::{OutputLimits, cap};
use super::servers::signals_transport_failure;
use crate::gate::{AgentId, ROOT_AGENT};
use crate::guarded::{Guard, guard_callback};
use crate::lock::lock_recovering;
use crate::risk::RiskClass;
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
    meta: ToolMeta,
    limits: Arc<OutputLimits>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
    tracker: CallTracker,
    transport_failed: Arc<Notify>,
    tool_count: Arc<AtomicUsize>,
    tool_names: Arc<Mutex<Vec<String>>>,
    muted: Arc<AtomicBool>,
}

/// What a connection's [`GuardedSink`] shares with the rest of the connection.
pub(crate) struct Shared {
    pub(crate) meta: ToolMeta,
    pub(crate) tracker: CallTracker,
    pub(crate) transport_failed: Arc<Notify>,
    /// How many tools the sink's last `add`/`reconcile` committed.
    pub(crate) tool_count: Arc<AtomicUsize>,
    /// The registered names of the tools the sink last committed, for removing them by name.
    pub(crate) tool_names: Arc<Mutex<Vec<String>>>,
    /// Set when the server is being unloaded: a later `list_changed` refresh must not register
    /// the tools again.
    pub(crate) muted: Arc<AtomicBool>,
}

impl<S> GuardedSink<S> {
    pub(crate) fn new(
        inner: S,
        server: &str,
        options: &super::servers::McpOptions,
        shared: &Shared,
    ) -> Self {
        GuardedSink {
            inner,
            server: server.to_string(),
            guard: options.guard.clone(),
            reveal_names: options.reveal_names,
            meta: shared.meta.clone(),
            limits: options.limits.clone(),
            warn: options.warn.clone(),
            tracker: shared.tracker.clone(),
            transport_failed: shared.transport_failed.clone(),
            tool_count: shared.tool_count.clone(),
            tool_names: shared.tool_names.clone(),
            muted: shared.muted.clone(),
        }
    }

    fn record(&self, wrapped: &[DynamicTool]) {
        self.tool_count.store(wrapped.len(), Ordering::Relaxed);
        // The names are not decided on, only used to remove the tools; a poisoned list still
        // holds the last names written.
        *lock_recovering(&self.tool_names) = wrapped
            .iter()
            .map(|tool| tool.definition().name.to_string())
            .collect();
    }

    fn wrap(&self, tool: DynamicTool) -> Option<DynamicTool> {
        let mut definition = tool.definition();
        let original_name = definition.name.to_string();
        let requires_user_interaction = self.meta.requires_user_interaction(&original_name);
        let name = match ToolName::new(mcp_tool_name(&self.server, &original_name)) {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!(error = %e, "skipping an MCP tool with an unusable name");
                return None;
            }
        };
        definition.name = name;
        let registered = definition.name.to_string();
        let original = Arc::new(tool);
        let probe = original.clone();
        let server_tool = original_name.clone();
        let (server, tool_label) = if self.reveal_names {
            (self.server.clone(), original_name)
        } else {
            (HIDDEN.to_string(), HIDDEN.to_string())
        };
        let meta = self.meta.clone();
        let limits = self.limits.clone();
        let warn = self.warn.clone();
        let tracker = self.tracker.clone();
        let transport_failed = self.transport_failed.clone();
        let callback = as_callback(move |context: &mut ToolContext, args: Value| {
            let original = original.clone();
            let (meta, limits, warn) = (meta.clone(), limits.clone(), warn.clone());
            let (tracker, transport_failed) = (tracker.clone(), transport_failed.clone());
            let (server_tool, registered) = (server_tool.clone(), registered.clone());
            // A call that does not say who made it counts as a subagent's, the stricter side.
            let root = context
                .require::<AgentId>()
                .is_ok_and(|agent| agent.0 == ROOT_AGENT);
            let span = tracing::info_span!(
                target: TARGET,
                "fontaine.mcp.call",
                mcp.server = server.as_str(),
                mcp.tool = tool_label.as_str(),
            );
            Box::pin(
                async move {
                    let _in_flight = tracker.enter(root);
                    let outcome = original.run_with(context, args).await;
                    signals_transport_failure(&outcome, &transport_failed);
                    cap(
                        outcome,
                        &registered,
                        meta.char_limit(&server_tool),
                        &limits,
                        &*warn,
                    )
                }
                .instrument(span),
            )
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

    /// Sorted by name: a server's `tools/list` order is its own, and the tool array is the front
    /// of every request, so a reordered list would invalidate the provider's cached prefix.
    fn wrap_all(&self, tools: Vec<DynamicTool>) -> Vec<DynamicTool> {
        let mut wrapped: Vec<DynamicTool> = tools
            .into_iter()
            .filter_map(|tool| self.wrap(tool))
            .collect();
        wrapped.sort_by_cached_key(|tool| tool.definition().name.to_string());
        wrapped
    }
}

impl<S: ManagedToolSink> ManagedToolSink for GuardedSink<S> {
    fn add_managed_tools(&self, tools: Vec<DynamicTool>) -> HashMap<String, ManagedToolToken> {
        let wrapped = self.wrap_all(tools);
        self.record(&wrapped);
        self.inner.add_managed_tools(wrapped)
    }

    fn reconcile_managed_tools(
        &self,
        expected: HashMap<String, ManagedToolToken>,
        tools: Vec<DynamicTool>,
    ) -> HashMap<String, ManagedToolToken> {
        if self.muted.load(Ordering::SeqCst) {
            return expected;
        }
        let wrapped = self.wrap_all(tools);
        self.record(&wrapped);
        self.inner.reconcile_managed_tools(expected, wrapped)
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
