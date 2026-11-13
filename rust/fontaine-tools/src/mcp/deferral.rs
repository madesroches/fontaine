//! Deferred MCP tool loading. Every MCP tool is registered and guarded as usual; when the
//! session defers them, only their names reach the model until it asks for the definitions with
//! `tool_search`. Nothing here is a gate: a deferred tool keeps its risk class and its wrapper,
//! and whether a definition is advertised changes what the model can name, not what a call to
//! the name goes through.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rig_core::completion::ToolDefinition;
use rig_memory::HeuristicTokenCounter;

use crate::lock::{lock_recovering, read_recovering, write_recovering};

/// The built-in tool that loads deferred definitions.
pub const TOOL_SEARCH: &str = "tool_search";

/// The share of the context window, in percent, MCP definitions may take before `auto` defers
/// them.
const AUTO_WINDOW_PERCENT: u64 = 10;

/// What `auto` compares against when the model's window is unknown.
pub const AUTO_UNKNOWN_WINDOW_TOKENS: u64 = 10_000;

/// `[mcp] tool_search`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeferMode {
    #[default]
    Auto,
    On,
    Off,
}

/// Whether the definitions of `tokens` estimated tokens are deferred under `mode`, given the
/// model's context window when it is known. Nothing is deferred when there is nothing to defer.
pub fn should_defer(mode: DeferMode, tokens: u64, window: Option<u64>) -> bool {
    if tokens == 0 {
        return false;
    }
    match mode {
        DeferMode::Off => false,
        DeferMode::On => true,
        DeferMode::Auto => {
            let threshold = window.map_or(AUTO_UNKNOWN_WINDOW_TOKENS, |window| {
                window / 100 * AUTO_WINDOW_PERCENT
            });
            tokens > threshold
        }
    }
}

struct Served {
    connection: u64,
    tools: Vec<Arc<ToolDefinition>>,
    tokens: u64,
}

#[derive(Default)]
struct State {
    servers: BTreeMap<String, Served>,
    mode: DeferMode,
    window: Option<u64>,
}

#[derive(Default)]
struct Inner {
    state: RwLock<State>,
    connections: AtomicU64,
}

/// The connection-scoped view of every MCP tool the session registered: its names, descriptions
/// and schemas, and whether they are deferred. It holds no per-agent loaded set.
#[derive(Clone, Default)]
pub struct Deferral(Arc<Inner>);

impl Deferral {
    /// A token for a new connection of a server, so a stale connection cannot forget what a
    /// newer one registered.
    pub fn begin_connection(&self) -> u64 {
        self.0.connections.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Replaces what `server` offers with `tools`, as its connection `connection` last committed.
    /// A connection older than the one already recorded is ignored: its late commit must not
    /// replace a newer connection's tools, which its own drop would then delete.
    pub fn record(&self, server: &str, connection: u64, tools: Vec<ToolDefinition>) {
        let counter = HeuristicTokenCounter::default();
        let tokens = tools.iter().map(|t| counter.count_tool(t) as u64).sum();
        let tools = tools.into_iter().map(Arc::new).collect();
        // Advertisement only: a poisoned registry still holds what was last written, and no
        // permission decision reads it.
        let mut state = write_recovering(&self.0.state);
        if state
            .servers
            .get(server)
            .is_some_and(|served| served.connection > connection)
        {
            return;
        }
        state.servers.insert(
            server.to_string(),
            Served {
                connection,
                tools,
                tokens,
            },
        );
    }

    /// Drops `server`'s tools, unless a newer connection has registered its own since.
    pub fn forget(&self, server: &str, connection: u64) {
        let mut state = write_recovering(&self.0.state);
        if state
            .servers
            .get(server)
            .is_some_and(|served| served.connection == connection)
        {
            state.servers.remove(server);
        }
    }

    /// The setting, and the main model's context window when the catalog knows it.
    pub fn set_policy(&self, mode: DeferMode, window: Option<u64>) {
        let mut state = write_recovering(&self.0.state);
        state.mode = mode;
        state.window = window;
    }

    /// The estimated tokens of every registered MCP definition.
    pub fn tokens(&self) -> u64 {
        read_recovering(&self.0.state)
            .servers
            .values()
            .map(|served| served.tokens)
            .sum()
    }

    /// Whether MCP definitions are deferred right now.
    pub fn is_active(&self) -> bool {
        let state = read_recovering(&self.0.state);
        let tokens = state.servers.values().map(|served| served.tokens).sum();
        should_defer(state.mode, tokens, state.window)
    }

    /// Whether `name` is a registered MCP tool, deferred or not.
    pub fn is_registered(&self, name: &str) -> bool {
        read_recovering(&self.0.state)
            .servers
            .values()
            .any(|served| served.tools.iter().any(|tool| tool.name.as_str() == name))
    }

    /// Every registered MCP definition that `visible` accepts, in server then name order.
    pub fn definitions(&self, visible: &dyn Fn(&str) -> bool) -> Vec<Arc<ToolDefinition>> {
        read_recovering(&self.0.state)
            .servers
            .values()
            .flat_map(|served| served.tools.iter())
            .filter(|tool| visible(tool.name.as_str()))
            .cloned()
            .collect()
    }

    /// The registered tool names `visible` accepts, grouped by server, both sorted.
    pub fn names_by_server(&self, visible: &dyn Fn(&str) -> bool) -> Vec<(String, Vec<String>)> {
        read_recovering(&self.0.state)
            .servers
            .iter()
            .filter_map(|(server, served)| {
                let mut names: Vec<String> = served
                    .tools
                    .iter()
                    .map(|tool| tool.name.to_string())
                    .filter(|name| visible(name))
                    .collect();
                names.sort();
                (!names.is_empty()).then(|| (server.clone(), names))
            })
            .collect()
    }
}

const DEFAULT_RESULTS: usize = 5;
const MAX_RESULTS: usize = 25;

/// Ranks `candidates` against `query`. `select:a,b` names tools exactly, in the order given;
/// anything else is keywords, scored by case-insensitive overlap with the name (weighted) and
/// the description. At most `max` come back, defaulting to five.
pub fn search(
    query: &str,
    max: Option<usize>,
    candidates: &[Arc<ToolDefinition>],
) -> Vec<Arc<ToolDefinition>> {
    let max = max.unwrap_or(DEFAULT_RESULTS).clamp(1, MAX_RESULTS);
    let query = query.trim();
    if let Some(names) = query.strip_prefix("select:") {
        let mut found: Vec<Arc<ToolDefinition>> = Vec::new();
        for wanted in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if let Some(tool) = candidates.iter().find(|tool| tool.name.as_str() == wanted)
                && !found.iter().any(|f| f.name.as_str() == tool.name.as_str())
            {
                found.push(tool.clone());
            }
        }
        found.truncate(max);
        return found;
    }
    let terms: Vec<String> = query
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u32, &Arc<ToolDefinition>)> = candidates
        .iter()
        .filter_map(|tool| {
            let name = tool.name.to_lowercase();
            let description = tool.description.to_lowercase();
            let score: u32 = terms
                .iter()
                .map(|term| {
                    let in_name = if name.contains(term.as_str()) { 3 } else { 0 };
                    let in_description = u32::from(description.contains(term.as_str()));
                    in_name + in_description
                })
                .sum();
            (score > 0).then_some((score, tool))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored
        .into_iter()
        .take(max)
        .map(|(_, tool)| tool.clone())
        .collect()
}

/// Which tool names an agent's own toolset holds.
pub type Visible = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// What an agent may see of the deferred tools, and what it has loaded.
struct AgentLoaded {
    loaded: Vec<String>,
    /// Which tool names the agent's own toolset holds; `None` for the root, which holds them all.
    visible: Option<Visible>,
}

/// Each agent's loaded deferred tools, keyed by agent id: a child's loads never widen its
/// parent's request or the reverse. Lives for the session; a new session starts empty.
#[derive(Clone, Default)]
pub struct LoadedSets(Arc<Mutex<HashMap<String, AgentLoaded>>>);

impl LoadedSets {
    /// Starts `agent` with nothing loaded and `visible` as the names its toolset holds.
    pub fn enroll(&self, agent: &str, visible: Visible) {
        // Advertisement only; a poisoned map holds what was last written.
        lock_recovering(&self.0).insert(
            agent.to_string(),
            AgentLoaded {
                loaded: Vec::new(),
                visible: Some(visible),
            },
        );
    }

    /// Whether `agent` may load or name `tool`.
    pub fn visible(&self, agent: &str, tool: &str) -> bool {
        lock_recovering(&self.0)
            .get(agent)
            .and_then(|entry| entry.visible.clone())
            .is_none_or(|visible| visible(tool))
    }

    /// Marks `names` loaded for `agent`.
    pub fn load(&self, agent: &str, names: impl IntoIterator<Item = String>) {
        let mut sets = lock_recovering(&self.0);
        let entry = sets
            .entry(agent.to_string())
            .or_insert_with(|| AgentLoaded {
                loaded: Vec::new(),
                visible: None,
            });
        for name in names {
            if !entry.loaded.contains(&name) {
                entry.loaded.push(name);
            }
        }
    }

    /// What `agent` has loaded and `deferral` still registers, in load order. A name that
    /// vanished in a `list_changed` refresh is dropped for good.
    pub fn loaded(&self, agent: &str, deferral: &Deferral) -> Vec<String> {
        let mut sets = lock_recovering(&self.0);
        let Some(entry) = sets.get_mut(agent) else {
            return Vec::new();
        };
        entry.loaded.retain(|name| deferral.is_registered(name));
        entry.loaded.clone()
    }

    /// Forgets what `agent` loaded, keeping what it may see.
    pub fn clear(&self, agent: &str) {
        if let Some(entry) = lock_recovering(&self.0).get_mut(agent) {
            entry.loaded.clear();
        }
    }

    /// Forgets what every agent loaded.
    pub fn clear_all(&self) {
        for entry in lock_recovering(&self.0).values_mut() {
            entry.loaded.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::ToolName;
    use serde_json::json;

    fn def(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition::new(
            ToolName::new(name).expect("a test tool name"),
            description,
            json!({"type": "object"}),
        )
    }

    fn arcs(defs: Vec<ToolDefinition>) -> Vec<Arc<ToolDefinition>> {
        defs.into_iter().map(Arc::new).collect()
    }

    fn names(found: &[Arc<ToolDefinition>]) -> Vec<String> {
        found.iter().map(|t| t.name.to_string()).collect()
    }

    fn catalog() -> Vec<Arc<ToolDefinition>> {
        arcs(vec![
            def("mcp__gh__create_issue", "Open an issue in a repository"),
            def(
                "mcp__gh__list_prs",
                "List pull requests; mentions issue links",
            ),
            def("mcp__db__query", "Run a SQL query"),
        ])
    }

    #[test]
    fn a_name_match_outranks_a_description_match() {
        let found = search("issue", None, &catalog());
        assert_eq!(
            names(&found),
            ["mcp__gh__create_issue", "mcp__gh__list_prs"]
        );
    }

    #[test]
    fn matching_ignores_case_and_ties_break_by_name() {
        let found = search("SQL QUERY", None, &catalog());
        assert_eq!(names(&found), ["mcp__db__query"]);
    }

    #[test]
    fn select_names_tools_exactly_in_the_order_given() {
        let found = search(
            "select: mcp__db__query, mcp__gh__create_issue,mcp__db__query,nope",
            None,
            &catalog(),
        );
        assert_eq!(names(&found), ["mcp__db__query", "mcp__gh__create_issue"]);
    }

    #[test]
    fn no_match_is_empty() {
        assert!(search("zebra", None, &catalog()).is_empty());
        assert!(search("   ", None, &catalog()).is_empty());
        assert!(search("select:", None, &catalog()).is_empty());
    }

    #[test]
    fn max_results_caps_and_is_never_zero() {
        assert_eq!(search("mcp", Some(1), &catalog()).len(), 1);
        assert_eq!(search("mcp", Some(0), &catalog()).len(), 1);
        assert_eq!(search("mcp", Some(2), &catalog()).len(), 2);
        assert_eq!(search("mcp", None, &catalog()).len(), 3);
    }

    #[test]
    fn auto_defers_past_a_tenth_of_the_window_or_ten_thousand_tokens_unknown() {
        assert!(!should_defer(DeferMode::Auto, 20_000, Some(200_000)));
        assert!(should_defer(DeferMode::Auto, 20_001, Some(200_000)));
        assert!(!should_defer(DeferMode::Auto, 10_000, None));
        assert!(should_defer(DeferMode::Auto, 10_001, None));
    }

    #[test]
    fn on_and_off_ignore_the_size_and_nothing_defers_when_empty() {
        assert!(should_defer(DeferMode::On, 1, Some(1_000_000)));
        assert!(!should_defer(DeferMode::Off, 1_000_000, None));
        assert!(!should_defer(DeferMode::On, 0, None));
    }

    #[test]
    fn a_refresh_replaces_a_servers_tools_and_a_stale_connection_cannot_forget_the_new_ones() {
        let deferral = Deferral::default();
        let old = deferral.begin_connection();
        deferral.record(
            "gh",
            old,
            vec![def("mcp__gh__a", "a"), def("mcp__gh__b", "b")],
        );
        assert!(deferral.is_registered("mcp__gh__b"));
        deferral.record("gh", old, vec![def("mcp__gh__a", "a")]);
        assert!(!deferral.is_registered("mcp__gh__b"));

        let new = deferral.begin_connection();
        deferral.record("gh", new, vec![def("mcp__gh__c", "c")]);
        deferral.forget("gh", old);
        assert!(deferral.is_registered("mcp__gh__c"));
        deferral.forget("gh", new);
        assert!(!deferral.is_registered("mcp__gh__c"));
    }

    #[test]
    fn an_older_connection_cannot_overwrite_a_newer_ones_tools() {
        let deferral = Deferral::default();
        let old = deferral.begin_connection();
        let new = deferral.begin_connection();
        deferral.record("gh", new, vec![def("mcp__gh__new", "n")]);
        deferral.record("gh", old, vec![def("mcp__gh__old", "o")]);
        assert!(deferral.is_registered("mcp__gh__new"));
        assert!(!deferral.is_registered("mcp__gh__old"));
        deferral.forget("gh", old);
        assert!(deferral.is_registered("mcp__gh__new"));
    }

    #[test]
    fn the_policy_decides_whether_the_registry_is_deferred() {
        let deferral = Deferral::default();
        deferral.set_policy(DeferMode::On, None);
        assert!(!deferral.is_active(), "nothing registered");
        let c = deferral.begin_connection();
        deferral.record("gh", c, vec![def("mcp__gh__a", "a")]);
        assert!(deferral.is_active());
        deferral.set_policy(DeferMode::Off, None);
        assert!(!deferral.is_active());
        deferral.set_policy(DeferMode::Auto, Some(200_000));
        assert!(
            !deferral.is_active(),
            "a handful of tokens is far below a tenth"
        );
    }

    #[test]
    fn names_are_grouped_by_server_and_sorted() {
        let deferral = Deferral::default();
        let c = deferral.begin_connection();
        deferral.record(
            "gh",
            c,
            vec![def("mcp__gh__b", "b"), def("mcp__gh__a", "a")],
        );
        deferral.record("db", c, vec![def("mcp__db__q", "q")]);
        let grouped = deferral.names_by_server(&|_| true);
        assert_eq!(
            grouped,
            vec![
                ("db".to_string(), vec!["mcp__db__q".to_string()]),
                (
                    "gh".to_string(),
                    vec!["mcp__gh__a".to_string(), "mcp__gh__b".to_string()]
                ),
            ]
        );
        let only_gh = deferral.names_by_server(&|n| n.starts_with("mcp__gh"));
        assert_eq!(only_gh.len(), 1);
    }

    #[test]
    fn loaded_names_are_per_agent_and_drop_when_they_vanish() {
        let deferral = Deferral::default();
        let c = deferral.begin_connection();
        deferral.record(
            "gh",
            c,
            vec![def("mcp__gh__a", "a"), def("mcp__gh__b", "b")],
        );
        let sets = LoadedSets::default();
        sets.load("root", ["mcp__gh__a".to_string(), "mcp__gh__b".to_string()]);
        sets.load("child-1", ["mcp__gh__a".to_string()]);
        assert_eq!(sets.loaded("root", &deferral).len(), 2);

        deferral.record("gh", c, vec![def("mcp__gh__a", "a")]);
        assert_eq!(sets.loaded("root", &deferral), ["mcp__gh__a"]);
        deferral.record("gh", c, vec![def("mcp__gh__b", "b")]);
        deferral.record("gh", c, vec![def("mcp__gh__a", "a")]);
        assert_eq!(sets.loaded("root", &deferral), ["mcp__gh__a"]);

        sets.clear("child-1");
        assert!(sets.loaded("child-1", &deferral).is_empty());
        assert_eq!(sets.loaded("root", &deferral), ["mcp__gh__a"]);
        sets.clear_all();
        assert!(sets.loaded("root", &deferral).is_empty());
    }

    #[test]
    fn an_enrolled_agent_sees_only_its_toolset_and_starts_empty() {
        let sets = LoadedSets::default();
        assert!(sets.visible("root", "anything"));
        sets.enroll("child-1", Arc::new(|name| name == "mcp__gh__a"));
        assert!(sets.visible("child-1", "mcp__gh__a"));
        assert!(!sets.visible("child-1", "mcp__gh__b"));
        sets.load("child-1", ["mcp__gh__a".to_string()]);
        sets.enroll("child-1", Arc::new(|_| true));
        assert!(
            sets.loaded("child-1", &Deferral::default()).is_empty(),
            "a revived child starts empty"
        );
    }
}
