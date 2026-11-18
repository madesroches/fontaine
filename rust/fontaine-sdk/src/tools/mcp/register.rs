//! Registers an MCP server's tools into the agent's tool server, wrapped. rig-rmcp registers raw
//! tools with whatever sink it is given, and again on every list-changed refresh, so the sink
//! it gets is this one: nothing an MCP server offers is ever reachable unwrapped.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::future::BoxFuture;
use tokio::sync::Notify;

use super::deferral::Deferral;
use super::elicitation::CallTracker;
use super::meta::ToolMeta;
use super::output::{OutputLimits, cap};
use super::servers::{McpError, signals_transport_failure};
use crate::tools::error_chain::chain;
use crate::tools::gate::{AgentId, ROOT_AGENT};
use crate::tools::guarded::{Guard, guard_callback};
use crate::tools::lock::lock_recovering;
use crate::tools::risk::{RiskClass, ToolSource};
use rig_core::completion::ToolDefinition;
use rig_core::message::ToolName;
use rig_core::serve::adapters::ToolCallback;
use rig_core::tool::{
    DynamicTool, ManagedToolSink, ManagedToolToken, ToolContext, ToolExecutionError, ToolOutput,
};
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

/// A tool of a live connection as the stand-in for it needs it: the definition the model was
/// offered, whether the server marked it as needing a person, and the callback that runs after
/// the gate.
#[derive(Clone)]
pub struct LiveTool {
    pub definition: ToolDefinition,
    pub requires_user_interaction: bool,
    pub callback: Arc<dyn ToolCallback>,
}

/// Starts a stdio server again for a call to one of its stand-in tools and answers with the new
/// connection's tools. It is injected rather than named, so this module never reaches the code
/// that owns the connection.
pub type RestartFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<LiveTool>, McpError>> + Send + Sync>;

/// Where a lost connection's tools find the restart that replaces it.
pub(crate) type RestartCell = Arc<Mutex<Option<RestartFn>>>;

/// Starts the server again and runs the call on the new connection's tool of the same name.
async fn restart_and_dispatch(
    restart: RestartFn,
    name: String,
    context: &mut ToolContext,
    args: Value,
) -> Result<ToolOutput, ToolExecutionError> {
    let live = restart()
        .await
        .map_err(|error| ToolExecutionError::other(chain(&error)))?;
    let Some(tool) = live
        .into_iter()
        .find(|t| t.definition.name == name.as_str())
    else {
        return Err(ToolExecutionError::other(format!(
            "the restarted MCP server no longer offers `{name}`"
        )));
    };
    (*tool.callback)(context, args).await
}

/// One stand-in tool per lost tool, registered under the same name and guarded exactly like the
/// live tool was: the gate runs first, and only an allowed call starts the server again and is
/// then handed to the new connection's tool of that name.
pub fn stand_in_tools(
    guard: &Guard,
    tools: &[LiveTool],
    restart: &RestartFn,
    standing: &Arc<AtomicBool>,
) -> Vec<DynamicTool> {
    tools
        .iter()
        .map(|tool| {
            let name = tool.definition.name.to_string();
            let restart = restart.clone();
            let callback = as_callback(move |context: &mut ToolContext, args: Value| {
                let (restart, name) = (restart.clone(), name.clone());
                Box::pin(restart_and_dispatch(restart, name, context, args))
            });
            let standing = standing.clone();
            guard_callback(
                tool.definition.clone(),
                callback,
                RiskClass::Arbitrary,
                guard.clone(),
                tool.requires_user_interaction,
                ToolSource::Mcp,
            )
            .with_liveness(move || standing.load(Ordering::SeqCst))
        })
        .collect()
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
    listed: Arc<Notify>,
    deferral: Deferral,
    connection: u64,
    live: Arc<Mutex<Vec<LiveTool>>>,
    restart: RestartCell,
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
    /// Woken, with a stored permit, each time the sink commits a tool list, empty ones included;
    /// connect waits on the first.
    pub(crate) listed: Arc<Notify>,
    /// This connection's token in the session's deferral registry.
    pub(crate) connection: u64,
    /// The tools the sink last committed, for a stand-in to take their place.
    pub(crate) live: Arc<Mutex<Vec<LiveTool>>>,
    /// Set when the connection is lost and its tools stand in for a restart: a call the agent
    /// pinned to these tools before the loss still reaches the restarted server.
    pub(crate) restart: RestartCell,
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
            listed: shared.listed.clone(),
            deferral: options.deferral.clone(),
            connection: shared.connection,
            live: shared.live.clone(),
            restart: shared.restart.clone(),
        }
    }

    fn record(&self, wrapped: &[DynamicTool], live: Vec<LiveTool>) {
        // Only read to build stand-ins; a poisoned list still holds the last tools written.
        *lock_recovering(&self.live) = live;
        self.tool_count.store(wrapped.len(), Ordering::Relaxed);
        // The names are not decided on, only used to remove the tools; a poisoned list still
        // holds the last names written.
        *lock_recovering(&self.tool_names) = wrapped
            .iter()
            .map(|tool| tool.definition().name.to_string())
            .collect();
        // Every wrapped tool is registered the same whether or not the session defers it; the
        // registry only decides what is advertised.
        self.deferral.record(
            &self.server,
            self.connection,
            wrapped.iter().map(DynamicTool::definition).collect(),
        );
    }

    fn wrap(&self, tool: DynamicTool) -> Option<(DynamicTool, LiveTool)> {
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
        let kept_definition = definition.clone();
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
        let restart = self.restart.clone();
        let callback = as_callback(move |context: &mut ToolContext, args: Value| {
            let original = original.clone();
            // Read when the call runs, not when it was made: the loss that sets it may come between.
            let restart_to = if original.is_live() {
                None
            } else {
                lock_recovering(&restart).clone()
            };
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
            if let Some(restart) = restart_to {
                return Box::pin(restart_and_dispatch(restart, registered, context, args));
            }
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
        let live = LiveTool {
            definition: kept_definition,
            requires_user_interaction,
            callback: callback.clone(),
        };
        let guarded = guard_callback(
            definition,
            callback,
            RiskClass::Arbitrary,
            self.guard.clone(),
            requires_user_interaction,
            ToolSource::Mcp,
        )
        .with_liveness(move || probe.is_live());
        Some((guarded, live))
    }

    /// Sorted by name: a server's `tools/list` order is its own, and the tool array is the front
    /// of every request, so a reordered list would invalidate the provider's cached prefix.
    fn wrap_all(&self, tools: Vec<DynamicTool>) -> (Vec<DynamicTool>, Vec<LiveTool>) {
        let mut wrapped: Vec<(DynamicTool, LiveTool)> = tools
            .into_iter()
            .filter_map(|tool| self.wrap(tool))
            .collect();
        wrapped.sort_by_cached_key(|(tool, _)| tool.definition().name.to_string());
        wrapped.into_iter().unzip()
    }
}

impl<S: ManagedToolSink> ManagedToolSink for GuardedSink<S> {
    fn add_managed_tools(&self, tools: Vec<DynamicTool>) -> HashMap<String, ManagedToolToken> {
        let (wrapped, live) = self.wrap_all(tools);
        self.record(&wrapped, live);
        let tokens = self.inner.add_managed_tools(wrapped);
        self.listed.notify_one();
        tokens
    }

    fn reconcile_managed_tools(
        &self,
        expected: HashMap<String, ManagedToolToken>,
        tools: Vec<DynamicTool>,
    ) -> HashMap<String, ManagedToolToken> {
        if self.muted.load(Ordering::SeqCst) {
            return expected;
        }
        let (wrapped, live) = self.wrap_all(tools);
        self.record(&wrapped, live);
        let tokens = self.inner.reconcile_managed_tools(expected, wrapped);
        self.listed.notify_one();
        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::gate::{CallView, GateDecision, ToolGate};
    use crate::tools::result_pipeline::ResultPipeline;
    use rig_core::tool::ToolOutput;
    use serde_json::json;

    struct Verdict(bool);

    impl ToolGate for Verdict {
        fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
            let allow = self.0;
            let args = call.args.clone();
            Box::pin(async move {
                if allow {
                    GateDecision::Allow { args }
                } else {
                    GateDecision::Deny {
                        message: "denied".to_string(),
                    }
                }
            })
        }
    }

    fn stand_in(allow: bool, restarts: Arc<AtomicUsize>) -> DynamicTool {
        let guard = Guard {
            gate: Arc::new(Verdict(allow)),
            results: Arc::new(ResultPipeline::new()),
            hide_mcp_names: false,
            pre_images: None,
            kills: None,
        };
        let callback = as_callback(|_: &mut ToolContext, args: Value| {
            Box::pin(async move { Ok(ToolOutput::text(format!("live {args}"))) })
        });
        let live = LiveTool {
            definition: ToolDefinition::new(
                ToolName::new("mcp__srv__echo").unwrap(),
                "Echo",
                json!({"type": "object"}),
            ),
            requires_user_interaction: false,
            callback,
        };
        let again = live.clone();
        let restart: RestartFn = Arc::new(move || {
            restarts.fetch_add(1, Ordering::SeqCst);
            let again = again.clone();
            Box::pin(async move { Ok(vec![again]) })
        });
        stand_in_tools(&guard, &[live], &restart, &Arc::new(AtomicBool::new(true))).remove(0)
    }

    fn as_root() -> ToolContext {
        let mut context = ToolContext::new();
        context.insert(AgentId(ROOT_AGENT.to_string())).unwrap();
        context
    }

    #[tokio::test]
    async fn a_denied_call_to_a_stand_in_does_not_restart_the_server() {
        let restarts = Arc::new(AtomicUsize::new(0));
        let tool = stand_in(false, restarts.clone());
        let outcome = tool.run_with(&mut as_root(), json!({"a": 1})).await;
        assert!(outcome.is_err() || outcome.is_ok_and(|o| o.as_text() != Some("live {\"a\":1}")));
        assert_eq!(restarts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_allowed_call_to_a_stand_in_restarts_once_and_reaches_the_live_tool() {
        let restarts = Arc::new(AtomicUsize::new(0));
        let tool = stand_in(true, restarts.clone());
        let output = tool
            .run_with(&mut as_root(), json!({"a": 1}))
            .await
            .expect("the call runs");
        assert_eq!(output.as_text(), Some("live {\"a\":1}"));
        assert_eq!(restarts.load(Ordering::SeqCst), 1);
    }

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
