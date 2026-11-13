//! Which MCP tool definitions an agent's requests carry while the session defers them, and what
//! the model is told when it calls one it has not loaded. Neither changes a gate: a deferred
//! tool is registered and wrapped like any other, and a call that reaches it takes the same
//! decision. The hook only narrows the advertised set, which rig-agent also uses as the set
//! of tools it will dispatch.

use fontaine_tools::mcp::{Deferral, LoadedSets, TOOL_SEARCH};
use rig_agent::agent::hook::{
    InvalidToolCallAction, InvalidToolCallContext, InvalidToolCallReason, RequestPatch,
};
use rig_agent::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext};
use rig_agent::tool::server::ToolServerHandle;

/// One agent's view of the deferred tools. Applied to the root agent and to each subagent, over
/// that agent's own tool server.
pub(crate) struct DeferredAdvertiser {
    pub(crate) agent: String,
    pub(crate) tools: ToolServerHandle,
    pub(crate) deferral: Deferral,
    pub(crate) loaded: LoadedSets,
}

impl DeferredAdvertiser {
    fn registered(&self) -> Vec<String> {
        self.tools.snapshot().names().map(str::to_owned).collect()
    }

    /// Whether deferral applies to this agent: the session defers and the agent holds the tool
    /// that loads definitions, so a toolset that dropped `tool_search` still sees its tools.
    fn active(&self, registered: &[String]) -> bool {
        self.deferral.is_active() && registered.iter().any(|name| name == TOOL_SEARCH)
    }

    /// The registered names this agent's request may carry now: everything that is not a
    /// deferred tool or `tool_search`, plus `tool_search` while deferral is on and the deferred
    /// tools this agent loaded. Built from the names registered right now, so a tool that
    /// vanished in a `list_changed` refresh cannot be asked for.
    fn advertised(&self, registered: &[String]) -> Vec<String> {
        let active = self.active(registered);
        if !active {
            self.loaded.clear(&self.agent);
        }
        let loaded = self.loaded.loaded(&self.agent, &self.deferral);
        registered
            .iter()
            .filter(|name| {
                if name.as_str() == TOOL_SEARCH {
                    active
                } else if active && self.deferral.is_registered(name) {
                    loaded.contains(name)
                } else {
                    true
                }
            })
            .cloned()
            .collect()
    }
}

impl AgentHook for DeferredAdvertiser {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        let registered = self.registered();
        let advertised = self.advertised(&registered);
        if advertised.len() == registered.len() {
            return CompletionCallAction::Continue;
        }
        CompletionCallAction::Patch(RequestPatch::default().active_tools(advertised))
    }

    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        if event.reason != InvalidToolCallReason::UnknownTool {
            return None;
        }
        let name = event.tool_name.as_str();
        // Only a tool this agent's own toolset holds and has not loaded: any other unknown name
        // stays on the default path.
        let registered = self.registered();
        let held = registered.iter().any(|registered| registered == name);
        if held && self.active(&registered) && self.deferral.is_registered(name) {
            return Some(InvalidToolCallAction::skip(format!(
                "`{name}` is a deferred tool whose definition is not loaded, so it was not \
                 called. Search first with `tool_search` (for example `select:{name}`), then \
                 call it."
            )));
        }
        None
    }
}

/// The deferred-tools section of a subagent's prompt: the names its own toolset holds, or `None`
/// while deferral is off for it.
pub(crate) fn child_section(deferral: &Deferral, tools: &ToolServerHandle) -> Option<String> {
    let held: Vec<String> = tools.snapshot().names().map(str::to_owned).collect();
    if !deferral.is_active() || !held.iter().any(|name| name == TOOL_SEARCH) {
        return None;
    }
    crate::prompt::deferred_tools_section(
        &deferral.names_by_server(&|name| held.iter().any(|held| held == name)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_tools::mcp::DeferMode;
    use rig_agent::tool::server::ToolServer;
    use rig_core::completion::ToolDefinition;
    use rig_core::message::ToolName;
    use rig_core::tool::{Tool, ToolContext, ToolExecutionError};

    macro_rules! stub {
        ($name:ident, $tool:literal) => {
            struct $name;
            impl Tool for $name {
                const NAME: &'static str = $tool;
                type Args = serde_json::Value;
                type Output = String;
                type Error = ToolExecutionError;
                fn description(&self) -> String {
                    "a stub".to_string()
                }
                fn parameters(&self) -> serde_json::Value {
                    serde_json::json!({"type": "object"})
                }
                async fn call(
                    &self,
                    _: &mut ToolContext,
                    _: serde_json::Value,
                ) -> Result<String, ToolExecutionError> {
                    Ok(String::new())
                }
            }
        };
    }
    stub!(Read, "read");
    stub!(Search, "tool_search");
    stub!(A, "mcp__gh__a");
    stub!(B, "mcp__gh__b");

    fn definition(name: &str) -> ToolDefinition {
        ToolDefinition::new(
            ToolName::new(name).expect("a test tool name"),
            "stub",
            serde_json::json!({"type": "object"}),
        )
    }

    fn advertiser(mode: DeferMode, with_search: bool) -> DeferredAdvertiser {
        let server = ToolServer::new().tool(Read).tool(A).tool(B);
        let tools = if with_search {
            server.tool(Search)
        } else {
            server
        }
        .run();
        let deferral = Deferral::default();
        deferral.set_policy(mode, None);
        let connection = deferral.begin_connection();
        deferral.record(
            "gh",
            connection,
            vec![definition("mcp__gh__a"), definition("mcp__gh__b")],
        );
        DeferredAdvertiser {
            agent: "root".to_string(),
            tools,
            deferral,
            loaded: LoadedSets::default(),
        }
    }

    fn names(hook: &DeferredAdvertiser) -> Vec<String> {
        hook.advertised(&hook.registered())
    }

    #[test]
    fn deferred_tools_are_left_out_until_loaded_and_tool_search_is_in() {
        let hook = advertiser(DeferMode::On, true);
        let mut base = names(&hook);
        base.sort();
        assert_eq!(base, ["read", "tool_search"]);
        hook.loaded.load("root", ["mcp__gh__b".to_string()]);
        let mut loaded = names(&hook);
        loaded.sort();
        assert_eq!(loaded, ["mcp__gh__b", "read", "tool_search"]);
    }

    #[test]
    fn with_deferral_inactive_tool_search_is_left_out_and_everything_else_is_advertised() {
        for mode in [DeferMode::Off, DeferMode::Auto] {
            let hook = advertiser(mode, true);
            let mut all = names(&hook);
            all.sort();
            assert_eq!(all, ["mcp__gh__a", "mcp__gh__b", "read"], "{mode:?}");
        }
    }

    #[test]
    fn a_toolset_without_tool_search_is_never_deferred() {
        let hook = advertiser(DeferMode::On, false);
        let mut all = names(&hook);
        all.sort();
        assert_eq!(all, ["mcp__gh__a", "mcp__gh__b", "read"]);
    }

    #[test]
    fn a_loaded_tool_that_vanished_is_not_asked_for_and_stays_gone() {
        let hook = advertiser(DeferMode::On, true);
        hook.loaded
            .load("root", ["mcp__gh__a".to_string(), "mcp__gh__b".to_string()]);
        let connection = hook.deferral.begin_connection();
        hook.deferral
            .record("gh", connection, vec![definition("mcp__gh__a")]);
        hook.tools.remove_tool("mcp__gh__b");
        let mut now = names(&hook);
        now.sort();
        assert_eq!(now, ["mcp__gh__a", "read", "tool_search"]);
        assert!(
            !hook
                .loaded
                .loaded("root", &hook.deferral)
                .contains(&"mcp__gh__b".to_string())
        );
    }

    #[test]
    fn going_inactive_forgets_what_was_loaded() {
        let hook = advertiser(DeferMode::On, true);
        hook.loaded.load("root", ["mcp__gh__a".to_string()]);
        hook.deferral.set_policy(DeferMode::Off, None);
        names(&hook);
        hook.deferral.set_policy(DeferMode::On, None);
        let mut again = names(&hook);
        again.sort();
        assert_eq!(again, ["read", "tool_search"]);
    }
}
