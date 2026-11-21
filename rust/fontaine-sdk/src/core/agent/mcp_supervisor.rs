//! Keeping the session's MCP servers alive: a slot per server the session attempted, a watcher per
//! live connection, and the reconnect and startup-retry schedules. A remote server that drops is
//! reconnected with exponential backoff, and in a headless run keeps being retried in the
//! background after a transient failure. An interactive session marks a stdio server that exits
//! failed, since starting its process again is a decision for the person; a headless run has no
//! person to ask, so the exited server's tools stay registered as stand-ins and a call to one
//! starts the server again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::tools::error_chain::chain;
use crate::tools::lock::{lock, read, read_recovering, write, write_recovering};
use crate::tools::mcp::McpConnectionHandle as McpConnection;
use crate::tools::mcp::{
    CONNECT_TIMEOUT, CallScope, Connected, Listings, LiveTool, McpError, McpErrorKind, McpOptions,
    McpResourceQueries, RestartFn, ServerSpec, connect as connect_server, pull_listings,
    stand_in_tools,
};
use crate::tools::tls::LoadedNetwork;
use futures::future::BoxFuture;
use rig_agent::tool::server::ToolServerHandle;
use rmcp::model::{ReadResourceRequestParams, ReadResourceResponse, ResourceContents};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::core::permissions::Approver;

use super::EnvLookup;

/// The first reconnect waits this long, and each later one twice as long as the last.
const RECONNECT_BASE: Duration = Duration::from_secs(1);
/// How many times a lost remote server is started again before it is marked failed.
pub const RECONNECT_ATTEMPTS: u32 = 5;
/// A headless run keeps retrying a remote server after a transient failure, no more than this
/// far apart.
const BACKGROUND_RETRY_CAP: Duration = Duration::from_secs(60);
/// How long after the connection was lost a headless run keeps retrying a remote server.
const BACKGROUND_RETRY_WINDOW: Duration = Duration::from_secs(30 * 60);
/// How many times a headless run starts a stdio server again for a call, per server.
pub const MAX_STDIO_RESTARTS: u32 = 3;
/// How long an unloaded server's in-flight calls may finish before its connection closes. A
/// call is waited for rather than cancelled, since cancelling can leave a tool's side effect
/// half done; one still running after the bound ends with the transport error.
pub const MCP_DRAIN_BOUND: Duration = Duration::from_secs(10);
/// One server's startup attempts share this deadline.
const STARTUP_DEADLINE: Duration = CONNECT_TIMEOUT;
/// The waits before a remote server's startup retries.
const STARTUP_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpServerState {
    /// Running, with `tools` registered now and `resources`/`prompts` listed.
    Connected {
        tools: usize,
        resources: usize,
        prompts: usize,
    },
    /// Lost, and being started again; `attempt` runs from 1 to 5.
    Reconnecting { attempt: u32 },
    /// Lost, and the five attempts failed transiently; a headless run goes on trying in the
    /// background.
    Retrying,
    /// Not running, with the reason as a rendered error chain.
    Failed(String),
}

impl McpServerState {
    /// Running with `tools` registered and nothing listed yet.
    pub(crate) fn connected(tools: usize) -> Self {
        McpServerState::Connected {
            tools,
            resources: 0,
            prompts: 0,
        }
    }
}

/// Which servers `/mcp reconnect` restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconnectTarget {
    One(String),
    AllFailed,
}

/// Connects one server. The production connector starts the real thing; tests script outcomes.
pub(crate) trait McpConnector: Send + Sync {
    fn connect<'a>(&'a self, spec: &'a ServerSpec) -> BoxFuture<'a, Result<Connected, McpError>>;

    /// Keeps the lost server's tools registered as stand-ins that call `restart` first.
    fn register_tombstone(&self, _spec: &ServerSpec, _tools: Vec<LiveTool>, _restart: RestartFn) {}

    /// Takes the stand-ins of `server` out again, leaving any live tool that has replaced one.
    fn unregister_tombstone(&self, _server: &str) {}
}

/// What a tombstone registered, to take it out again.
struct Stood {
    /// The stand-ins' liveness: once false, the registry retires them, and never a live tool that
    /// has since taken their name.
    standing: Arc<AtomicBool>,
    deferral: u64,
    tools: Vec<LiveTool>,
    restart: RestartFn,
}

/// Registers the taken-out stand-ins again when dropped, unless the connect succeeded.
struct RestoreStandIns<'a> {
    connector: &'a ServerConnector,
    spec: &'a ServerSpec,
    stood: Option<Stood>,
}

impl Drop for RestoreStandIns<'_> {
    fn drop(&mut self) {
        // An unregister during the connect (remove, retire, manual reconnect) took the name out
        // of `connecting`; restoring then would resurrect or overwrite what the session moved on from.
        let wanted = lock(&self.connector.connecting, "MCP connecting").remove(&self.spec.name);
        if let Some(stood) = self.stood.take() {
            self.connector
                .options
                .deferral
                .forget(&self.spec.name, stood.deferral);
            if wanted {
                self.connector
                    .register_tombstone(self.spec, stood.tools, stood.restart);
            }
        }
    }
}

pub(crate) struct ServerConnector {
    pub(crate) options: McpOptions,
    pub(crate) tool_server: ToolServerHandle,
    pub(crate) network: Arc<LoadedNetwork>,
    pub(crate) env: EnvLookup,
    tombstones: Mutex<HashMap<String, Stood>>,
    /// Servers whose taken-out stand-ins are held by an in-flight connect and still wanted back.
    connecting: Mutex<HashSet<String>>,
}

impl ServerConnector {
    pub(crate) fn new(
        options: McpOptions,
        tool_server: ToolServerHandle,
        network: Arc<LoadedNetwork>,
        env: EnvLookup,
    ) -> Self {
        ServerConnector {
            options,
            tool_server,
            network,
            env,
            tombstones: Mutex::default(),
            connecting: Mutex::default(),
        }
    }
}

impl McpConnector for ServerConnector {
    fn connect<'a>(&'a self, spec: &'a ServerSpec) -> BoxFuture<'a, Result<Connected, McpError>> {
        Box::pin(async move {
            let env = self.env.clone();
            // The registry will not let a new connection's tools take the place of tools it did
            // not register for that connection, so the stand-ins are retired first and put back
            // if the server does not come up.
            let standing = lock(&self.tombstones, "MCP tombstones").remove(&spec.name);
            if let Some(stood) = &standing {
                stood.standing.store(false, Ordering::SeqCst);
                lock(&self.connecting, "MCP connecting").insert(spec.name.clone());
            }
            // The call that awaits this restart can be cancelled mid-connect, which drops this
            // future; the guard puts the stand-ins back then too.
            let mut restore = RestoreStandIns {
                connector: self,
                spec,
                stood: standing,
            };
            let result = connect_server(
                spec,
                self.tool_server.clone(),
                &self.options,
                &self.network,
                &move |name| env(name),
            )
            .await;
            if result.is_ok() {
                restore.stood = None;
            }
            result
        })
    }

    fn register_tombstone(&self, spec: &ServerSpec, tools: Vec<LiveTool>, restart: RestartFn) {
        self.unregister_tombstone(&spec.name);
        let standing = Arc::new(AtomicBool::new(true));
        let stand_ins = stand_in_tools(&self.options.guard, &tools, &restart, &standing);
        // The lost connection's registry entry went with it; the stand-ins keep the tools
        // listed under a token of their own, which a newer live connection outranks.
        let deferral = self.options.deferral.begin_connection();
        self.options.deferral.record(
            &spec.name,
            deferral,
            tools.iter().map(|tool| tool.definition.clone()).collect(),
        );
        for tool in stand_ins {
            self.tool_server.add_dynamic_tool(tool);
        }
        lock(&self.tombstones, "MCP tombstones").insert(
            spec.name.clone(),
            Stood {
                standing,
                deferral,
                tools,
                restart,
            },
        );
    }

    fn unregister_tombstone(&self, server: &str) {
        lock(&self.connecting, "MCP connecting").remove(server);
        let stood = lock(&self.tombstones, "MCP tombstones").remove(server);
        if let Some(stood) = stood {
            stood.standing.store(false, Ordering::SeqCst);
            self.options.deferral.forget(server, stood.deferral);
        }
    }
}

/// A lost stdio server's stand-in tools, taken out of the registry when it drops.
struct Tombstone {
    connector: Arc<dyn McpConnector>,
    server: String,
}

impl Drop for Tombstone {
    fn drop(&mut self) {
        self.connector.unregister_tombstone(&self.server);
    }
}

/// One server the session attempted, whether it came up or not.
pub struct ServerSlot {
    spec: ServerSpec,
    // Display and advisory only: nothing decides on it, so a poisoned lock is read through.
    state: RwLock<McpServerState>,
    connection: Mutex<Option<Connected>>,
    watcher: Mutex<Option<JoinHandle<()>>>,
    /// Set before Fontaine closes a connection itself, so the close is not taken for a loss.
    closing: AtomicBool,
    /// Bumped by every manual reconnect, so a connection made for an older generation is
    /// discarded instead of installed next to the new one.
    generation: AtomicU64,
    /// Set while a headless stdio server that exited mid-session keeps stand-in tools.
    tombstone: Mutex<Option<Tombstone>>,
    /// The restarts a call has attempted, against [`MAX_STDIO_RESTARTS`].
    restarts: AtomicU32,
    /// Serializes restarts, so concurrent calls to the stand-ins start the server once.
    restarting: tokio::sync::Mutex<()>,
}

impl ServerSlot {
    fn new(spec: ServerSpec, state: McpServerState) -> ServerSlot {
        ServerSlot {
            spec,
            state: RwLock::new(state),
            connection: Mutex::new(None),
            watcher: Mutex::new(None),
            closing: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            tombstone: Mutex::new(None),
            restarts: AtomicU32::new(0),
            restarting: tokio::sync::Mutex::new(()),
        }
    }

    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The live connection and its call scope, for a request that is not a tool call but still
    /// joins the in-flight counting and can signal a dead transport.
    pub(crate) fn call_scope(&self) -> Option<(McpConnection, CallScope)> {
        lock(&self.connection, "MCP connection")
            .as_ref()
            .map(|connected| (connected.connection.clone(), connected.call_scope()))
    }

    /// The server's state; a connected server's counts are read from the connection the slot
    /// holds, so they follow a `list_changed` refresh and a reconnect.
    pub fn state(&self) -> McpServerState {
        let state = read_recovering(&self.state).clone();
        match state {
            McpServerState::Connected { .. } => {
                let guard = lock(&self.connection, "MCP connection");
                match guard.as_ref() {
                    Some(connected) => McpServerState::Connected {
                        tools: connected.tool_count(),
                        resources: connected.listings().resources.len(),
                        prompts: connected.listings().prompts.len(),
                    },
                    None => McpServerState::connected(0),
                }
            }
            other => other,
        }
    }

    fn set_state(&self, state: McpServerState) {
        *write_recovering(&self.state) = state;
    }

    fn is_remote(&self) -> bool {
        self.spec.kind() == "http"
    }

    /// Stops the watcher and starts a new generation, returning it. The bump happens under the
    /// watcher lock, which `install` checks it against.
    fn supersede(&self) -> u64 {
        let mut watcher = lock(&self.watcher, "MCP watcher");
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(handle) = watcher.take() {
            handle.abort();
        }
        generation
    }
}

/// The session's MCP servers, by name. Shared with the resource tools, which read it live on
/// every call, so it sits behind a lock: a poisoned map fed a permission decision and must
/// panic, not be read through.
#[derive(Default)]
pub struct McpServers {
    slots: Arc<RwLock<BTreeMap<String, Arc<ServerSlot>>>>,
    /// Set by the first start; the one that restarts a server by hand.
    pub(crate) supervisor: Option<Arc<Supervisor>>,
    /// Every MCP tool's definition, and whether the session defers them.
    pub(crate) deferral: crate::tools::mcp::Deferral,
    /// What each agent has loaded of the deferred tools.
    pub(crate) loaded: crate::tools::mcp::LoadedSets,
}

impl McpServers {
    /// Applies `[mcp] tool_search` and the main model's window to the deferral. The
    /// `claude-subscription` transport hands the tool list to the CLI, which owns tool search
    /// itself, so Fontaine never defers there.
    pub(crate) fn sync_deferral(
        &self,
        config: &crate::core::config::Config,
        models: &crate::core::models::Models,
    ) {
        use crate::core::config::ToolSearch;
        use crate::tools::mcp::DeferMode;
        let cli_owns_tools = models.main.kind.provider().name()
            == crate::core::providers::claude_subscription::PROVIDER_NAME;
        let mode = match config.mcp.tool_search {
            _ if cli_owns_tools => DeferMode::Off,
            ToolSearch::Auto => DeferMode::Auto,
            ToolSearch::On => DeferMode::On,
            ToolSearch::Off => DeferMode::Off,
        };
        let window = models
            .main
            .completion
            .spec()
            .and_then(|spec| spec.context_window)
            .map(u64::from);
        self.deferral.set_policy(mode, window);
        self.deferral
            .set_native_config(config.mcp.native_tool_search);
    }

    /// The deferred tool names the root's prompt lists, grouped by server; empty while deferral
    /// is off.
    pub(crate) fn deferred_names(&self) -> Vec<(String, Vec<String>)> {
        if self.deferral.is_active() {
            self.deferral.names_by_server(&|_| true)
        } else {
            Vec::new()
        }
    }

    /// The MCP part of the prompt: the resource section and the deferred-tools section.
    pub(crate) fn prompt_section(&self) -> Option<String> {
        crate::core::prompt::mcp_section(&self.listings(), &self.deferred_names())
    }

    pub fn states(&self) -> Vec<(String, McpServerState)> {
        read(&self.slots, "MCP server slots")
            .iter()
            .map(|(name, slot)| (name.clone(), slot.state()))
            .collect()
    }

    pub(crate) fn slot(&self, name: &str) -> Option<Arc<ServerSlot>> {
        read(&self.slots, "MCP server slots").get(name).cloned()
    }

    pub(crate) fn failed(&self) -> Vec<Arc<ServerSlot>> {
        read(&self.slots, "MCP server slots")
            .values()
            .filter(|slot| matches!(slot.state(), McpServerState::Failed(_)))
            .cloned()
            .collect()
    }

    /// The name and listings of every slot that holds a live connection, in name order.
    pub(crate) fn listings(&self) -> Vec<(String, Listings)> {
        read(&self.slots, "MCP server slots")
            .iter()
            .filter_map(|(name, slot)| {
                let guard = lock(&slot.connection, "MCP connection");
                let connected = guard.as_ref()?;
                Some((name.clone(), connected.listings()))
            })
            .collect()
    }

    /// The resource tools' live view of the connected servers.
    pub(crate) fn resource_queries(&self) -> Arc<dyn McpResourceQueries> {
        Arc::new(SlotQueries {
            slots: self.slots.clone(),
        })
    }

    /// The live slot map, shared with anything that must see servers connecting after it was
    /// handed out.
    pub(crate) fn slots_handle(&self) -> Arc<RwLock<BTreeMap<String, Arc<ServerSlot>>>> {
        self.slots.clone()
    }

    pub(crate) fn insert(&self, slot: Arc<ServerSlot>) {
        if let Some(old) =
            write(&self.slots, "MCP server slots").insert(slot.name().to_string(), slot)
        {
            retire(&old);
        }
    }

    /// The first half of the final exit, synchronous: every slot is retired, so no watcher
    /// reconnects a server and none mistakes the close that follows for a loss.
    pub(crate) fn retire_all(&self) {
        for slot in read(&self.slots, "MCP server slots").values() {
            retire(slot);
        }
    }

    /// The second half of the final exit: takes every connection and closes them concurrently,
    /// with no drain of calls in flight and no wait for a slot's tools to leave the registry.
    /// A server gets `graceful` to leave when its stdin closes; then, whether or not it did,
    /// each stdio server's whole process group is signalled synchronously, so a descendant a
    /// wrapper started cannot outlive the session. Dropping this future, as the caller's
    /// deadline does, signals the groups too.
    pub(crate) async fn close_all_final(&self, graceful: Duration) {
        let connections: Vec<Connected> = read(&self.slots, "MCP server slots")
            .values()
            .filter_map(|slot| {
                let connected = lock(&slot.connection, "MCP connection").take();
                if let Some(connected) = &connected {
                    connected.mute();
                }
                connected
            })
            .collect();
        let mut groups = GroupKill {
            groups: connections.iter().map(Connected::group_id).collect(),
            termed: false,
        };
        let closes = connections.iter().map(|connected| async move {
            let _ = tokio::time::timeout(graceful, connected.connection.close()).await;
        });
        futures::future::join_all(closes).await;
        groups.term();
        // A descendant that ignores SIGTERM gets this long before the drop's SIGKILL.
        tokio::time::sleep(GROUP_TERM_GRACE).await;
    }

    /// Unloads the server `name`: its slot is retired, its `list_changed` sink muted (a refresh
    /// during the drain would register the tools again), its tools removed from `tools` so no new
    /// call starts, the calls in flight given up to `bound` to finish, and the connection closed.
    /// Returns `None` for a server with no slot, else whether every call finished in time.
    pub(crate) async fn remove(
        &self,
        name: &str,
        tools: &ToolServerHandle,
        bound: Duration,
    ) -> Option<bool> {
        let slot = write(&self.slots, "MCP server slots").remove(name)?;
        retire(&slot);
        let connected = lock(&slot.connection, "MCP connection").take();
        let Some(connected) = connected else {
            return Some(true);
        };
        connected.mute();
        for tool in connected.tool_names() {
            tools.remove_tool(&tool);
        }
        let drained = connected.drain(bound).await;
        connected.connection.close().await;
        Some(drained)
    }
}

const GROUP_TERM_GRACE: Duration = Duration::from_millis(150);

/// Ends the process groups it holds when it drops, whether the close finished or was cut short:
/// SIGKILL, after a SIGTERM unless [`GroupKill::term`] already sent one.
struct GroupKill {
    groups: Vec<Option<u32>>,
    termed: bool,
}

impl GroupKill {
    fn term(&mut self) {
        for group in &self.groups {
            crate::tools::procgroup::terminate(*group);
        }
        self.termed = true;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        if !self.termed {
            self.term();
        }
        for group in &self.groups {
            crate::tools::procgroup::kill_group(*group);
        }
    }
}

/// Ends a slot's watcher and marks its connection as closed by us.
fn retire(slot: &ServerSlot) {
    slot.closing.store(true, Ordering::SeqCst);
    // A reconnect that finishes after this is handed back by `install` instead of reviving the slot.
    slot.supersede();
    drop(lock(&slot.tombstone, "MCP tombstone").take());
}

/// Commits what a pull answered into `listings`; a listing the pull could not fetch keeps
/// the value it already holds.
async fn refresh_listings(connection: &McpConnection, listings: &Arc<Mutex<Listings>>) {
    let previous = lock(listings, "MCP listings").clone();
    let pulled = pull_listings(connection, &previous).await;
    *lock(listings, "MCP listings") = pulled;
}

/// The `@` mention resource list built from what the slots list now.
pub(crate) fn mention_snapshot(
    slots: &Arc<RwLock<BTreeMap<String, Arc<ServerSlot>>>>,
) -> Vec<super::mcp::McpResourceSummary> {
    read(slots, "MCP server slots")
        .iter()
        .flat_map(|(server, slot)| {
            let guard = lock(&slot.connection, "MCP connection");
            let listings = guard.as_ref().map(|connected| connected.listings());
            listings.into_iter().flat_map(move |listings| {
                listings
                    .resources
                    .into_iter()
                    .map(move |resource| super::mcp::McpResourceSummary {
                        server: server.clone(),
                        uri: resource.uri,
                        name: resource.name,
                        description: resource.description,
                    })
            })
        })
        .collect()
}

/// The resource tools' live view of the connected servers: every call reads the slot map and
/// the listings it holds now, so a server that connects mid-session is visible without a
/// registry rebuild.
struct SlotQueries {
    slots: Arc<RwLock<BTreeMap<String, Arc<ServerSlot>>>>,
}

impl SlotQueries {
    /// The live connection, listings and call scope of `server`, or why they cannot be read.
    fn target(&self, server: &str) -> Result<(McpConnection, Listings, CallScope), String> {
        let slots = read(&self.slots, "MCP server slots");
        let Some(slot) = slots.get(server) else {
            // Failed and reconnecting slots stay in the map for `/mcp reconnect`; only live
            // connections may be labelled connected.
            let connected: Vec<String> = slots
                .iter()
                .filter(|(_, slot)| lock(&slot.connection, "MCP connection").is_some())
                .map(|(name, _)| name.clone())
                .collect();
            return Err(format!(
                "no MCP server named `{server}` is connected{}",
                if connected.is_empty() {
                    String::new()
                } else {
                    format!(" (connected: {})", connected.join(", "))
                }
            ));
        };
        let guard = lock(&slot.connection, "MCP connection");
        let Some(connected) = guard.as_ref() else {
            return Err(format!("MCP server `{server}` is not connected"));
        };
        Ok((
            connected.connection.clone(),
            connected.listings(),
            connected.call_scope(),
        ))
    }
}

impl McpResourceQueries for SlotQueries {
    fn connected(&self) -> Vec<(String, Listings)> {
        read(&self.slots, "MCP server slots")
            .iter()
            .filter_map(|(name, slot)| {
                let guard = lock(&slot.connection, "MCP connection");
                let connected = guard.as_ref()?;
                let listings = connected.listings();
                // Tools-only servers list nothing; skip them rather than answering with an
                // empty listing per server.
                (!listings.is_empty()).then(|| (name.clone(), listings))
            })
            .collect()
    }

    fn read_resource<'a>(
        &'a self,
        root: bool,
        server: &'a str,
        uri: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ResourceContents>, String>> {
        Box::pin(async move {
            let (connection, listings, scope) = self.target(server)?;
            if !listings
                .resources
                .iter()
                .any(|resource| resource.uri == uri)
            {
                // The diagnostic names only the first few URIs: a server listing thousands
                // must not ship its whole catalog into an error.
                const SHOWN: usize = 10;
                let known: Vec<&str> = listings
                    .resources
                    .iter()
                    .map(|resource| resource.uri.as_str())
                    .collect();
                let listed = known.len().min(SHOWN);
                let mut names = known[..listed].join(", ");
                if known.len() > listed {
                    names.push_str(&format!(", … and {} more", known.len() - listed));
                }
                return Err(format!(
                    "no resource `{uri}` on MCP server `{server}`{}",
                    if known.is_empty() {
                        " (it lists no resources)".to_string()
                    } else {
                        format!(" (it lists: {names})")
                    }
                ));
            }
            // The single-round variant: an InputRequired answer is the tool error saying the
            // server asked for input the client does not provide, not a driven MRTR round. The
            // read holds a call guard so a server unload waits it out, and a failed read can
            // mark a dead transport for the watcher, as a failed tool call does.
            let _in_flight = scope.enter(root);
            match connection
                .peer()
                .read_resource_once(ReadResourceRequestParams::new(uri))
                .await
            {
                Ok(ReadResourceResponse::Complete(result)) => Ok(result.contents),
                Ok(ReadResourceResponse::InputRequired(_)) => Err(format!(
                    "MCP server `{server}` asked for input the client does not provide"
                )),
                Ok(other) => Err(format!(
                    "reading `{uri}` from MCP server `{server}` answered unexpectedly: {other:?}"
                )),
                Err(error) => {
                    scope.request_failed(&error);
                    Err(format!(
                        "reading `{uri}` from MCP server `{server}` failed: {}",
                        chain(&error)
                    ))
                }
            }
        })
    }
}

impl Drop for McpServers {
    /// The connections close as the slots drop, and a watcher must not mistake that for a loss.
    fn drop(&mut self) {
        for slot in read(&self.slots, "MCP server slots").values() {
            retire(slot);
        }
    }
}

/// What a watcher needs to replace a lost connection.
pub(crate) struct Supervisor {
    connector: Arc<dyn McpConnector>,
    approver: Arc<dyn Approver>,
    /// Re-derives the toolsets of agents that were built before the server's tools existed.
    refresh: Arc<dyn Fn() + Send + Sync>,
    /// No person is there to run `/mcp reconnect`, so the supervisor recovers servers itself.
    headless: bool,
}

fn disconnected(name: &str) -> String {
    format!("MCP server `{name}` disconnected · run /mcp reconnect {name}")
}

impl Supervisor {
    pub(crate) fn new(
        connector: Arc<dyn McpConnector>,
        approver: Arc<dyn Approver>,
        refresh: Arc<dyn Fn() + Send + Sync>,
        headless: bool,
    ) -> Arc<Supervisor> {
        Arc::new(Supervisor {
            connector,
            approver,
            refresh,
            headless,
        })
    }

    async fn attempt(&self, spec: &ServerSpec, limit: Duration) -> Result<Connected, McpError> {
        match tokio::time::timeout(limit, self.connector.connect(spec)).await {
            Ok(result) => result,
            Err(_) => Err(McpError::timed_out(&spec.name, STARTUP_DEADLINE)),
        }
    }

    /// Starts `spec`. A remote server's transient failure is retried after 1, 2 and 4 seconds, all
    /// inside one 30-second deadline; a stdio server and a permanent error are not retried. The
    /// slot exists either way: a failure leaves it `Failed`, so `/mcp reconnect` can find it.
    pub(crate) async fn start(
        self: &Arc<Self>,
        spec: ServerSpec,
    ) -> (Arc<ServerSlot>, Result<(), McpError>) {
        let deadline = Instant::now() + STARTUP_DEADLINE;
        let remote = spec.kind() == "http";
        let mut delays = STARTUP_RETRY_DELAYS.iter();
        let result = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.attempt(&spec, left).await {
                Ok(connected) => break Ok(connected),
                Err(error) if remote && error.kind == McpErrorKind::Transient => {
                    match delays.next() {
                        // A retry that would only start after the deadline is not started.
                        Some(delay) if Instant::now() + *delay < deadline => {
                            tokio::time::sleep(*delay).await;
                        }
                        _ => break Err(error),
                    }
                }
                Err(error) => break Err(error),
            }
        };
        match result {
            Ok(connected) => {
                let slot = Arc::new(ServerSlot::new(spec, McpServerState::connected(0)));
                // The initial listing pull rides before the install, so the connect reports
                // back with the listings in place; it is bounded by the startup deadline that
                // is left, and a failed or expired pull leaves the empty listings the
                // connection started with.
                let left = deadline.saturating_duration_since(Instant::now());
                let _ = tokio::time::timeout(left, connected.refresh_listings()).await;
                if self.install(&slot, connected, 0).is_err() {
                    unreachable!("a new slot is still on its first generation");
                }
                (slot, Ok(()))
            }
            Err(error) => {
                let slot = Arc::new(ServerSlot::new(spec, McpServerState::Failed(chain(&error))));
                (slot, Err(error))
            }
        }
    }

    /// Takes a new connection into `slot` and starts watching it. A connection made for a
    /// generation a manual reconnect has since replaced is handed back, to be closed.
    // The refused connection is only ever closed, so boxing it would add an allocation to a path
    // that exists to hand it straight back.
    #[allow(clippy::result_large_err)]
    fn install(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
        connected: Connected,
        generation: u64,
    ) -> Result<(), Connected> {
        let mut watcher = lock(&slot.watcher, "MCP watcher");
        if slot.generation.load(Ordering::SeqCst) != generation {
            return Err(connected);
        }
        let connection = connected.connection.clone();
        let failed = connected.transport_failed.clone();
        // The listings the watcher maintains and the signal a list_changed arrives on.
        let listings = connected.listings_slot();
        let listings_changed = connected.listings_changed();
        *lock(&slot.connection, "MCP connection") = Some(connected);
        slot.closing.store(false, Ordering::SeqCst);
        slot.set_state(McpServerState::connected(0));
        // The live tools have replaced the stand-ins in the registry by now.
        drop(lock(&slot.tombstone, "MCP tombstone").take());
        let new = tokio::spawn(self.clone().watch(
            slot.clone(),
            connection,
            failed,
            listings,
            listings_changed,
        ));
        // A replaced watcher is aborted, unless it is the task that is calling us, which ends
        // by itself.
        if let Some(old) = watcher.replace(new)
            && tokio::task::try_id() != Some(old.id())
        {
            old.abort();
        }
        Ok(())
    }

    async fn watch(
        self: Arc<Self>,
        slot: Arc<ServerSlot>,
        connection: McpConnection,
        failed: Arc<Notify>,
        listings: Arc<Mutex<Listings>>,
        listings_changed: Arc<Notify>,
    ) {
        // The initial pull rode install; here the listings are re-pulled on every list_changed
        // notification the transport tap forwards.
        loop {
            tokio::select! {
                _ = connection.closed() => break,
                _ = failed.notified(), if slot.is_remote() => {
                    // rmcp's HTTP worker never ends the connection of a dead server, so it is
                    // closed here, which retires its tools.
                    connection.close().await;
                    break;
                }
                _ = listings_changed.notified() => {
                    // The refresh rides as an in-flight future so a connection loss or a
                    // transport failure stays selectable while a pull runs; losing the
                    // connection cancels the refresh, keeping the listings it already
                    // committed, and each listing's own deadline bounds a silent server.
                    let mut refresh = Box::pin(refresh_listings(&connection, &listings));
                    tokio::select! {
                        _ = connection.closed() => break,
                        _ = failed.notified(), if slot.is_remote() => {
                            // rmcp's HTTP worker never ends the connection of a dead
                            // server, so it is closed here, which retires its tools.
                            connection.close().await;
                            break;
                        }
                        _ = &mut refresh => {}
                    }
                    // The mention snapshot and the counts follow the listings just pulled.
                    (self.refresh)();
                }
            }
        }
        if slot.closing.load(Ordering::SeqCst) {
            return;
        }
        let lost = lock(&slot.connection, "MCP connection").take();
        // Read before the drop, which takes the tools out of the deferral registry.
        let tools = lost.as_ref().map(Connected::live_tools).unwrap_or_default();
        let stand_ins = self.headless && !slot.is_remote() && !tools.is_empty();
        if stand_ins
            && let Some(lost) = &lost
            && slot.restarts.load(Ordering::SeqCst) < MAX_STDIO_RESTARTS
        {
            // The agent's turn pins the tool catalog it started with, so a call it already
            // holds the lost tools for has to reach the restart too.
            lost.set_restart(self.restart_fn(&slot));
        }
        // The drop removes the lost tools by name, so the stand-ins register after it.
        drop(lost);
        if stand_ins {
            self.leave_tombstone(&slot, tools);
        }
        // The mention snapshot still lists the lost server's resources; rebuild it.
        (self.refresh)();
        if slot.is_remote() {
            self.reconnect_with_backoff(&slot).await;
        } else if !stand_ins {
            slot.set_state(McpServerState::Failed("the server exited".to_string()));
            self.approver.warn(&disconnected(slot.name()));
        }
    }

    /// A headless stdio server exited: its tools stay registered as stand-ins, so the next call
    /// to one starts the server again, unless it has already been restarted as often as allowed.
    fn leave_tombstone(self: &Arc<Self>, slot: &Arc<ServerSlot>, tools: Vec<LiveTool>) {
        if slot.restarts.load(Ordering::SeqCst) >= MAX_STDIO_RESTARTS {
            slot.set_state(McpServerState::Failed(format!(
                "exited more than {MAX_STDIO_RESTARTS} times"
            )));
            self.approver.warn(&disconnected(slot.name()));
            return;
        }
        self.connector
            .register_tombstone(&slot.spec, tools, self.restart_fn(slot));
        *lock(&slot.tombstone, "MCP tombstone") = Some(Tombstone {
            connector: self.connector.clone(),
            server: slot.name().to_string(),
        });
        slot.set_state(McpServerState::Failed(
            "the server exited · restarts on next call".to_string(),
        ));
        self.approver.warn(&disconnected(slot.name()));
    }

    fn restart_fn(self: &Arc<Self>, slot: &Arc<ServerSlot>) -> RestartFn {
        // Weak: the registry holds this closure, and the supervisor holds the registry.
        let supervisor = Arc::downgrade(self);
        let weak_slot = Arc::downgrade(slot);
        let name = slot.name().to_string();
        Arc::new(move || {
            let (supervisor, slot, name) = (supervisor.clone(), weak_slot.clone(), name.clone());
            Box::pin(async move {
                match (supervisor.upgrade(), slot.upgrade()) {
                    (Some(supervisor), Some(slot)) => supervisor.restart_for_call(&slot).await,
                    _ => Err(McpError::new(
                        &name,
                        McpErrorKind::Permanent,
                        "the session no longer runs this server",
                        None,
                    )),
                }
            })
        })
    }

    /// Starts the server of a tombstoned `slot` again from its stored spec, for a call to one of
    /// its stand-in tools, and answers with the new connection's tools. Callers wait for one
    /// another, and one that finds the server running again does not start it a second time. A
    /// failure leaves the tombstone and the slot's state as they were.
    async fn restart_for_call(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
    ) -> Result<Vec<LiveTool>, McpError> {
        let _single_flight = slot.restarting.lock().await;
        if let Some(connected) = lock(&slot.connection, "MCP connection").as_ref() {
            return Ok(connected.live_tools());
        }
        if lock(&slot.tombstone, "MCP tombstone").is_none() {
            return Err(McpError::new(
                slot.name(),
                McpErrorKind::Permanent,
                "the server was stopped and is no longer restarted",
                None,
            ));
        }
        let generation = slot.generation.load(Ordering::SeqCst);
        match self.connect_and_install(slot, generation).await? {
            Some(tools) => {
                slot.restarts.fetch_add(1, Ordering::SeqCst);
                self.approver
                    .notice(&format!("MCP server `{}` restarted", slot.name()));
                (self.refresh)();
                Ok(tools)
            }
            None => Err(McpError::new(
                slot.name(),
                McpErrorKind::Permanent,
                "the server was restarted by hand while this call waited",
                None,
            )),
        }
    }

    /// One attempt to connect `slot`'s server and take the connection into it. `None` when the
    /// slot moved to a newer generation meanwhile and the connection was closed instead.
    async fn connect_and_install(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
        generation: u64,
    ) -> Result<Option<Vec<LiveTool>>, McpError> {
        let connected = self.attempt(&slot.spec, CONNECT_TIMEOUT).await?;
        // The listings ride in with the connection, so the notice and the counts report them
        // together; the pull is bounded like the attempt, so an unanswered listing cannot stall
        // the connect.
        let _ = tokio::time::timeout(CONNECT_TIMEOUT, connected.refresh_listings()).await;
        let tools = connected.live_tools();
        match self.install(slot, connected, generation) {
            Ok(()) => Ok(Some(tools)),
            Err(stale) => {
                stale.connection.close().await;
                Ok(None)
            }
        }
    }

    /// Five attempts after 1, 2, 4, 8 and 16 seconds. A headless run goes on after a transient
    /// failure, waiting twice as long each time up to a minute, until the server comes back or
    /// half an hour has passed since it was lost.
    async fn reconnect_with_backoff(self: &Arc<Self>, slot: &Arc<ServerSlot>) {
        let lost_at = Instant::now();
        let generation = slot.generation.load(Ordering::SeqCst);
        let mut attempt = 0u32;
        let last = loop {
            attempt += 1;
            slot.set_state(if attempt <= RECONNECT_ATTEMPTS {
                McpServerState::Reconnecting { attempt }
            } else {
                McpServerState::Retrying
            });
            let doublings = 1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX);
            let wait = RECONNECT_BASE
                .saturating_mul(doublings)
                .min(BACKGROUND_RETRY_CAP);
            tokio::time::sleep(wait).await;
            match self.connect_and_install(slot, generation).await {
                Ok(Some(_)) => {
                    self.approver
                        .notice(&format!("MCP server `{}` reconnected", slot.name()));
                    (self.refresh)();
                    return;
                }
                Ok(None) => return,
                Err(error) => {
                    let background = self.headless
                        && error.kind == McpErrorKind::Transient
                        && lost_at.elapsed() < BACKGROUND_RETRY_WINDOW;
                    if error.kind == McpErrorKind::Permanent
                        || (attempt >= RECONNECT_ATTEMPTS && !background)
                    {
                        break error;
                    }
                }
            }
        };
        let reason = chain(&last);
        slot.set_state(McpServerState::Failed(reason));
        self.approver.warn(&disconnected(slot.name()));
        // The loss already dropped the server's mentions; the callback also covers a caller
        // that only observes the final state.
        (self.refresh)();
    }

    /// Restarts `slot` by hand, with no backoff: its watcher stops, its connection closes, and a
    /// new one takes its place.
    pub(crate) async fn reconnect(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
    ) -> Result<(), McpError> {
        let generation = slot.supersede();
        slot.closing.store(true, Ordering::SeqCst);
        drop(lock(&slot.tombstone, "MCP tombstone").take());
        let old = lock(&slot.connection, "MCP connection").take();
        if let Some(old) = old {
            old.connection.close().await;
        }
        match self.connect_and_install(slot, generation).await {
            Ok(installed) => {
                if installed.is_some() {
                    (self.refresh)();
                }
                Ok(())
            }
            Err(error) => {
                slot.set_state(McpServerState::Failed(chain(&error)));
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    use crate::tools::gate::{AgentId, CallView, GateDecision, ROOT_AGENT, ToolGate};
    use crate::tools::guarded::Guard;
    use crate::tools::mcp::connect_transport;
    use crate::tools::result_pipeline::ResultPipeline;
    use rig_agent::tool::server::ToolServer;
    use rig_core::tool::ToolContext;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    };
    use rmcp::service::RequestContext;
    use rmcp::{RoleServer, ServerHandler, ServiceExt};
    use serde_json::json;

    use crate::core::permissions::DenyAll;

    use super::*;

    struct Allow;

    impl ToolGate for Allow {
        fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
            let args = call.args.clone();
            Box::pin(async move { GateDecision::Allow { args } })
        }
    }

    #[derive(Clone)]
    struct Empty;

    impl ServerHandler for Empty {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }
    }

    /// A server with one tool, `echo`.
    #[derive(Clone)]
    struct Echoing;

    impl ServerHandler for Echoing {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            _: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let schema = Arc::new(serde_json::Map::from_iter([(
                "type".to_string(),
                json!("object"),
            )]));
            Ok(ListToolsResult::with_all_items(vec![Tool::new(
                "echo", "Echo", schema,
            )]))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let text = format!(
                "echo {}",
                serde_json::Value::Object(request.arguments.unwrap_or_default())
            );
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
        }
    }

    /// One live in-process server: the connection to it, and the task that serves it, which a
    /// test aborts to make the server go away.
    struct Live {
        connected: Connected,
        server: JoinHandle<()>,
    }

    async fn live(name: &str, kind: &'static str) -> Live {
        serve_live(name, kind, false).await
    }

    /// A live server that offers the `echo` tool.
    async fn live_echo(name: &str, kind: &'static str) -> Live {
        serve_live(name, kind, true).await
    }

    async fn serve_live(name: &str, kind: &'static str, echoing: bool) -> Live {
        let (c2s, sfc) = tokio::io::duplex(8192);
        let (s2c, cfs) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            if echoing {
                let running = Echoing.serve((sfc, s2c)).await.expect("server start");
                let _ = running.waiting().await;
            } else {
                let running = Empty.serve((sfc, s2c)).await.expect("server start");
                let _ = running.waiting().await;
            }
        });
        let options = McpOptions::new(
            Guard {
                gate: Arc::new(Allow),
                results: Arc::new(ResultPipeline::new()),
                hide_mcp_names: false,
                pre_images: None,
                kills: None,
            },
            true,
        );
        let mut connected = connect_transport(name, (cfs, c2s), ToolServer::new().run(), &options)
            .await
            .expect("connect");
        connected.kind = kind;
        Live { connected, server }
    }

    type Outcome = Result<Live, McpError>;

    /// Answers each connect with the next scripted outcome and counts the attempts. An empty
    /// script never answers.
    struct Script {
        outcomes: Mutex<VecDeque<Outcome>>,
        attempts: AtomicUsize,
        servers: Mutex<Vec<JoinHandle<()>>>,
        times: Mutex<Vec<Instant>>,
        /// What each `register_tombstone` was handed.
        tombstones: Mutex<Vec<(Vec<LiveTool>, RestartFn)>>,
        unregistered: AtomicUsize,
    }

    impl Script {
        fn new(outcomes: Vec<Outcome>) -> Arc<Script> {
            Arc::new(Script {
                outcomes: Mutex::new(outcomes.into()),
                attempts: AtomicUsize::new(0),
                servers: Mutex::new(Vec::new()),
                times: Mutex::new(Vec::new()),
                tombstones: Mutex::new(Vec::new()),
                unregistered: AtomicUsize::new(0),
            })
        }

        fn registered(&self) -> usize {
            self.tombstones.lock().unwrap().len()
        }

        fn unregistered(&self) -> usize {
            self.unregistered.load(Ordering::SeqCst)
        }

        /// The restart the latest tombstone was registered with.
        fn restart(&self) -> RestartFn {
            self.tombstones
                .lock()
                .unwrap()
                .last()
                .expect("a tombstone was registered")
                .1
                .clone()
        }

        /// Ends the server the latest connection is served by.
        fn kill_server(&self) {
            self.servers.lock().unwrap().remove(0).abort();
        }

        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }

        fn gaps(&self) -> Vec<u64> {
            let times = self.times.lock().unwrap();
            times
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).as_secs())
                .collect()
        }
    }

    impl McpConnector for Script {
        fn connect<'a>(
            &'a self,
            _spec: &'a ServerSpec,
        ) -> BoxFuture<'a, Result<Connected, McpError>> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            self.times.lock().unwrap().push(Instant::now());
            let next = self.outcomes.lock().unwrap().pop_front();
            Box::pin(async move {
                match next {
                    Some(Ok(live)) => {
                        self.servers.lock().unwrap().push(live.server);
                        Ok(live.connected)
                    }
                    Some(Err(error)) => Err(error),
                    None => std::future::pending().await,
                }
            })
        }

        fn register_tombstone(&self, _: &ServerSpec, tools: Vec<LiveTool>, restart: RestartFn) {
            self.tombstones.lock().unwrap().push((tools, restart));
        }

        fn unregister_tombstone(&self, _: &str) {
            self.unregistered.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct Said {
        notices: Mutex<Vec<String>>,
        warnings: Mutex<Vec<String>>,
    }

    struct Recording(Arc<Said>);

    impl Approver for Recording {
        fn approve<'a>(
            &'a self,
            request: crate::core::permissions::ApprovalRequest,
        ) -> BoxFuture<'a, crate::core::permissions::Answer> {
            DenyAll.approve(request)
        }

        fn notice(&self, message: &str) {
            self.0.notices.lock().unwrap().push(message.to_string());
        }

        fn warn(&self, message: &str) {
            self.0.warnings.lock().unwrap().push(message.to_string());
        }
    }

    struct Rig {
        script: Arc<Script>,
        said: Arc<Said>,
        refreshed: Arc<AtomicUsize>,
        supervisor: Arc<Supervisor>,
    }

    fn rig(outcomes: Vec<Outcome>) -> Rig {
        rig_with(outcomes, false)
    }

    fn rig_with(outcomes: Vec<Outcome>, headless: bool) -> Rig {
        let script = Script::new(outcomes);
        let said = Arc::new(Said::default());
        let refreshed = Arc::new(AtomicUsize::new(0));
        let counter = refreshed.clone();
        let supervisor = Supervisor::new(
            script.clone(),
            Arc::new(Recording(said.clone())),
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            headless,
        );
        Rig {
            script,
            said,
            refreshed,
            supervisor,
        }
    }

    fn spec(kind: &str) -> ServerSpec {
        use crate::tools::mcp::Transport;
        ServerSpec {
            name: "srv".to_string(),
            transport: match kind {
                "http" => Transport::Http {
                    url: "http://unused.test/mcp".to_string(),
                    headers: Default::default(),
                    headers_helper: None,
                    credentials: Arc::new(rmcp::transport::auth::InMemoryCredentialStore::new()),
                    states: Arc::new(rmcp::transport::auth::InMemoryStateStore::new()),
                },
                _ => Transport::Stdio {
                    command: "unused".to_string(),
                    args: Vec::new(),
                    env_vars: Vec::new(),
                    env: Vec::new(),
                    cwd: std::env::temp_dir(),
                },
            },
        }
    }

    fn error(kind: McpErrorKind) -> McpError {
        McpError::new("srv", kind, "scripted failure", None)
    }

    fn transient() -> Outcome {
        Err(error(McpErrorKind::Transient))
    }

    fn permanent() -> Outcome {
        Err(error(McpErrorKind::Permanent))
    }

    /// Lets spawned tasks run, with time paused.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    async fn wait_for(slot: &ServerSlot, wanted: McpServerState) {
        // Paused time: the background schedule runs for minutes.
        for _ in 0..2000 {
            if slot.state() == wanted {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("the state is {:?}, not {wanted:?}", slot.state());
    }

    #[tokio::test(start_paused = true)]
    async fn the_backoff_schedule_is_1_2_4_8_16_seconds_and_five_failures_end_it() {
        let rig = rig(vec![
            Ok(live("srv", "http").await),
            transient(),
            transient(),
            transient(),
            transient(),
            transient(),
        ]);
        let (slot, result) = rig.supervisor.start(spec("http")).await;
        assert!(result.is_ok());
        let first = rig.script.servers.lock().unwrap().remove(0);
        first.abort();
        wait_for(
            &slot,
            McpServerState::Failed("MCP server `srv`: scripted failure".into()),
        )
        .await;
        assert_eq!(rig.script.attempts(), 6);
        assert_eq!(rig.script.gaps(), [1, 2, 4, 8, 16]);
        let warnings = rig.said.warnings.lock().unwrap().clone();
        assert_eq!(
            warnings,
            ["MCP server `srv` disconnected · run /mcp reconnect srv"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_state_counts_the_attempts_while_reconnecting() {
        let rig = rig(vec![
            Ok(live("srv", "http").await),
            transient(),
            transient(),
        ]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.servers.lock().unwrap().remove(0).abort();
        wait_for(&slot, McpServerState::Reconnecting { attempt: 1 }).await;
        wait_for(&slot, McpServerState::Reconnecting { attempt: 2 }).await;
        wait_for(&slot, McpServerState::Reconnecting { attempt: 3 }).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_http_server_is_reconnected_and_the_toolsets_refreshed() {
        let rig = rig(vec![
            Ok(live("srv", "http").await),
            transient(),
            Ok(live("srv", "http").await),
        ]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 0);
        rig.script.servers.lock().unwrap().remove(0).abort();
        wait_for(&slot, McpServerState::Reconnecting { attempt: 2 }).await;
        wait_for(&slot, McpServerState::connected(0)).await;
        assert_eq!(rig.script.attempts(), 3);
        // The loss rebuilt the mention snapshot once and the reconnect's install once.
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 2);
        let notices = rig.said.notices.lock().unwrap().clone();
        assert_eq!(notices, ["MCP server `srv` reconnected"]);
        // The new connection is watched too: losing it starts another round.
        rig.script.servers.lock().unwrap().remove(0).abort();
        wait_for(&slot, McpServerState::Reconnecting { attempt: 1 }).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_transport_failure_signal_closes_the_connection_and_starts_the_backoff() {
        let first = live("srv", "http").await;
        let failed = first.connected.transport_failed.clone();
        let connection = first.connected.connection.clone();
        let rig = rig(vec![Ok(first), transient()]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        // The server task is still running: only the signal says the transport is dead.
        failed.notify_one();
        wait_for(&slot, McpServerState::Reconnecting { attempt: 1 }).await;
        assert!(connection.is_closed());
    }

    #[tokio::test(start_paused = true)]
    async fn a_list_changed_notification_refreshes_the_listings_and_rebuilds_the_mentions() {
        let first = live("srv", "http").await;
        // Notified before the watcher waits: the permit the notification stores is consumed by
        // its first `notified()`.
        let changed = first.connected.listings_changed();
        let rig = rig(vec![Ok(first)]);
        let (_slot, _) = rig.supervisor.start(spec("http")).await;
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 0);
        changed.notify_one();
        for _ in 0..200 {
            if rig.refreshed.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_connection_rebuilds_the_mentions_and_so_does_the_final_failure() {
        let rig = rig(vec![Ok(live("srv", "http").await), permanent()]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.servers.lock().unwrap().remove(0).abort();
        wait_for(
            &slot,
            McpServerState::Failed("MCP server `srv`: scripted failure".into()),
        )
        .await;
        // Once when the connection is dropped, once when the reconnect gives up.
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanent_error_ends_the_reconnecting_at_once() {
        let rig = rig(vec![Ok(live("srv", "http").await), permanent()]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.servers.lock().unwrap().remove(0).abort();
        wait_for(
            &slot,
            McpServerState::Failed("MCP server `srv`: scripted failure".into()),
        )
        .await;
        assert_eq!(rig.script.attempts(), 2);
        assert_eq!(rig.said.warnings.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stdio_exit_marks_the_server_failed_without_retrying() {
        let rig = rig(vec![Ok(live("srv", "stdio").await)]);
        let (slot, _) = rig.supervisor.start(spec("stdio")).await;
        rig.script.servers.lock().unwrap().remove(0).abort();
        wait_for(&slot, McpServerState::Failed("the server exited".into())).await;
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(rig.script.attempts(), 1);
        assert_eq!(
            rig.said.warnings.lock().unwrap().clone(),
            ["MCP server `srv` disconnected · run /mcp reconnect srv"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_we_close_ourselves_does_not_trigger_a_reconnect() {
        let rig = rig(vec![Ok(live("srv", "http").await)]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        slot.closing.store(true, Ordering::SeqCst);
        let connection = lock(&slot.connection, "test")
            .as_ref()
            .unwrap()
            .connection
            .clone();
        connection.close().await;
        settle().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(rig.script.attempts(), 1);
        assert_eq!(slot.state(), McpServerState::connected(0));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_retries_a_transient_http_failure_three_times_after_1_2_and_4_seconds() {
        let rig = rig(vec![transient(), transient(), transient(), transient()]);
        let (slot, result) = rig.supervisor.start(spec("http")).await;
        assert!(result.is_err());
        assert_eq!(rig.script.attempts(), 4);
        assert_eq!(rig.script.gaps(), [1, 2, 4]);
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_succeeds_on_a_retry() {
        let rig = rig(vec![transient(), Ok(live("srv", "http").await)]);
        let (slot, result) = rig.supervisor.start(spec("http")).await;
        assert!(result.is_ok());
        assert_eq!(slot.state(), McpServerState::connected(0));
        assert_eq!(rig.script.attempts(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn startup_does_not_retry_a_permanent_error_or_a_stdio_server() {
        let rig_permanent = rig(vec![permanent(), Ok(live("srv", "http").await)]);
        let (_, result) = rig_permanent.supervisor.start(spec("http")).await;
        assert!(result.is_err());
        assert_eq!(rig_permanent.script.attempts(), 1);

        let rig_stdio = rig(vec![transient(), Ok(live("srv", "stdio").await)]);
        let (_, result) = rig_stdio.supervisor.start(spec("stdio")).await;
        assert!(result.is_err());
        assert_eq!(rig_stdio.script.attempts(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn startup_gives_up_at_the_deadline_and_never_retries_a_timed_out_attempt() {
        // An empty script never answers.
        let rig = rig(vec![]);
        let started = Instant::now();
        let (slot, result) = rig.supervisor.start(spec("http")).await;
        let error = result.expect_err("nobody answers");
        assert_eq!(error.kind, McpErrorKind::Transient);
        assert_eq!(rig.script.attempts(), 1);
        assert_eq!(started.elapsed(), STARTUP_DEADLINE);
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_whose_backoff_would_pass_the_deadline_is_not_started() {
        // The first attempt takes 28 seconds before it fails; the 1 s wait fits, the 2 s one
        // would end after the deadline.
        struct Slow(AtomicUsize);
        impl McpConnector for Slow {
            fn connect<'a>(
                &'a self,
                _: &'a ServerSpec,
            ) -> BoxFuture<'a, Result<Connected, McpError>> {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if n == 0 {
                        tokio::time::sleep(Duration::from_secs(28)).await;
                    }
                    Err(error(McpErrorKind::Transient))
                })
            }
        }
        let slow = Arc::new(Slow(AtomicUsize::new(0)));
        let supervisor = Supervisor::new(
            slow.clone(),
            Arc::new(Recording(Arc::default())),
            Arc::new(|| {}),
            false,
        );
        let started = Instant::now();
        let (_, result) = supervisor.start(spec("http")).await;
        assert!(result.is_err());
        assert_eq!(slow.0.load(Ordering::SeqCst), 2);
        assert_eq!(started.elapsed(), Duration::from_secs(29));
    }

    #[tokio::test(start_paused = true)]
    async fn a_manual_reconnect_replaces_the_connection_without_backoff() {
        let rig = rig(vec![
            Ok(live("srv", "http").await),
            Ok(live("srv", "http").await),
        ]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        let before = Instant::now();
        let first_connection = lock(&slot.connection, "test")
            .as_ref()
            .unwrap()
            .connection
            .clone();
        rig.supervisor.reconnect(&slot).await.expect("reconnects");
        assert_eq!(Instant::now(), before, "no backoff");
        assert!(first_connection.is_closed());
        assert_eq!(slot.state(), McpServerState::connected(0));
        assert_eq!(rig.script.attempts(), 2);
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 1);
        // Closing the old connection ourselves started no reconnect of its own.
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(rig.script.attempts(), 2);
        assert!(rig.said.warnings.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_manual_reconnect_revives_a_failed_server_and_reports_a_failure() {
        let rig = rig(vec![
            permanent(),
            Ok(live("srv", "stdio").await),
            permanent(),
        ]);
        let (slot, _) = rig.supervisor.start(spec("stdio")).await;
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
        rig.supervisor.reconnect(&slot).await.expect("it comes up");
        assert_eq!(slot.state(), McpServerState::connected(0));
        let error = rig.supervisor.reconnect(&slot).await.unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Permanent);
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_servers_ends_their_watchers_without_a_reconnect() {
        let rig = rig(vec![Ok(live("srv", "http").await)]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        let servers = McpServers::default();
        servers.insert(slot.clone());
        assert_eq!(
            servers.states(),
            [("srv".to_string(), McpServerState::connected(0))]
        );
        drop(servers);
        settle().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(rig.script.attempts(), 1);
        assert!(slot.closing.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn removing_a_server_closes_its_connection_and_a_remote_one_is_not_reconnected() {
        let live = live("srv", "http").await;
        let connection = live.connected.connection.clone();
        let rig = rig(vec![Ok(live)]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        let servers = McpServers::default();
        servers.insert(slot.clone());
        let tools = ToolServer::new().run();

        assert_eq!(
            servers.remove("srv", &tools, MCP_DRAIN_BOUND).await,
            Some(true)
        );
        assert!(connection.is_closed());
        assert!(servers.states().is_empty());
        assert!(slot.closing.load(Ordering::SeqCst));
        assert_eq!(
            servers.remove("srv", &tools, MCP_DRAIN_BOUND).await,
            None,
            "a server with no slot is not removed twice"
        );
        settle().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(rig.script.attempts(), 1, "unloading is not a loss");
        assert!(rig.said.warnings.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_final_close_retires_the_slot_and_closes_its_connection_without_a_reconnect() {
        let live = live("srv", "http").await;
        let connection = live.connected.connection.clone();
        let rig = rig(vec![Ok(live)]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        let servers = McpServers::default();
        servers.insert(slot.clone());

        servers.retire_all();
        assert!(slot.closing.load(Ordering::SeqCst));
        servers.close_all_final(Duration::from_millis(250)).await;

        assert!(connection.is_closed());
        assert!(lock(&slot.connection, "test").is_none());
        settle().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(rig.script.attempts(), 1, "closing for good is not a loss");
        assert!(rig.said.warnings.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn removing_a_failed_server_just_drops_its_slot() {
        let rig = rig(vec![permanent()]);
        let (slot, _) = rig.supervisor.start(spec("stdio")).await;
        let servers = McpServers::default();
        servers.insert(slot);
        let tools = ToolServer::new().run();
        assert_eq!(
            servers.remove("srv", &tools, MCP_DRAIN_BOUND).await,
            Some(true)
        );
        assert!(servers.states().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_finishing_after_the_server_is_removed_is_handed_back() {
        let rig = rig(vec![Ok(live("srv", "http").await)]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        let generation = slot.generation.load(Ordering::SeqCst);
        let servers = McpServers::default();
        servers.insert(slot.clone());
        let tools = ToolServer::new().run();
        servers.remove("srv", &tools, MCP_DRAIN_BOUND).await;

        let late = live("srv", "http").await.connected;
        assert!(rig.supervisor.install(&slot, late, generation).is_err());
        assert!(slot.closing.load(Ordering::SeqCst));
        assert!(lock(&slot.connection, "test").is_none());
    }

    const RESTARTS_ON_CALL: &str = "the server exited · restarts on next call";

    fn failed_with_a_tombstone() -> McpServerState {
        McpServerState::Failed(RESTARTS_ON_CALL.into())
    }

    fn root() -> ToolContext {
        let mut context = ToolContext::new();
        context
            .insert(AgentId(ROOT_AGENT.to_string()))
            .expect("a fresh context takes the agent id");
        context
    }

    /// Runs `tool` of a restarted server's tools by its registered name.
    async fn dispatch(tools: &[LiveTool], name: &str, args: serde_json::Value) -> String {
        let tool = tools
            .iter()
            .find(|tool| tool.definition.name == name)
            .expect("the restarted server offers the tool");
        let output = (*tool.callback)(&mut root(), args)
            .await
            .expect("the call succeeds");
        output.as_text().expect("plain text").to_string()
    }

    async fn headless_stdio(outcomes: Vec<Outcome>) -> (Rig, Arc<ServerSlot>) {
        let rig = rig_with(outcomes, true);
        let (slot, result) = rig.supervisor.start(spec("stdio")).await;
        assert!(result.is_ok());
        (rig, slot)
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_remote_server_keeps_retrying_after_five_transient_failures() {
        let rig = rig_with(
            vec![
                Ok(live("srv", "http").await),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                Ok(live("srv", "http").await),
            ],
            true,
        );
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.kill_server();
        wait_for(&slot, McpServerState::Retrying).await;
        assert_eq!(rig.script.attempts(), 6, "the fifth attempt has failed");
        assert!(rig.said.warnings.lock().unwrap().is_empty());
        wait_for(&slot, McpServerState::connected(0)).await;
        assert_eq!(rig.script.attempts(), 10);
        assert_eq!(rig.script.gaps(), [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(
            rig.said.notices.lock().unwrap().clone(),
            ["MCP server `srv` reconnected"]
        );
        assert!(rig.said.warnings.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_permanent_error_in_the_window_ends_it_as_failed() {
        let rig = rig_with(
            vec![
                Ok(live("srv", "http").await),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                permanent(),
                transient(),
            ],
            true,
        );
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.kill_server();
        wait_for(
            &slot,
            McpServerState::Failed("MCP server `srv`: scripted failure".into()),
        )
        .await;
        assert_eq!(rig.script.attempts(), 8);
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert_eq!(rig.script.attempts(), 8);
        assert_eq!(rig.said.warnings.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_the_thirty_minute_window_ends_the_retrying_with_one_warning() {
        let mut outcomes = vec![Ok(live("srv", "http").await)];
        outcomes.extend((0..100).map(|_| transient()));
        let rig = rig_with(outcomes, true);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.kill_server();
        tokio::time::sleep(Duration::from_secs(25 * 60)).await;
        assert_eq!(slot.state(), McpServerState::Retrying);
        assert!(rig.said.warnings.lock().unwrap().is_empty());
        tokio::time::sleep(Duration::from_secs(6 * 60)).await;
        assert_eq!(
            slot.state(),
            McpServerState::Failed("MCP server `srv`: scripted failure".into())
        );
        let attempts = rig.script.attempts();
        assert!(attempts < 40, "{attempts} attempts in half an hour");
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(rig.script.attempts(), attempts, "it stopped trying");
        assert_eq!(
            rig.said.warnings.lock().unwrap().clone(),
            ["MCP server `srv` disconnected · run /mcp reconnect srv"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_manual_reconnect_in_the_window_supersedes_the_retrying() {
        let rig = rig_with(
            vec![
                Ok(live("srv", "http").await),
                transient(),
                transient(),
                transient(),
                transient(),
                transient(),
                Ok(live("srv", "http").await),
                transient(),
            ],
            true,
        );
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.kill_server();
        wait_for(&slot, McpServerState::Retrying).await;
        rig.supervisor.reconnect(&slot).await.expect("reconnects");
        assert_eq!(slot.state(), McpServerState::connected(0));
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(
            rig.script.attempts(),
            7,
            "the retrying consumed nothing more"
        );
        assert_eq!(slot.state(), McpServerState::connected(0));
    }

    #[tokio::test(start_paused = true)]
    async fn interactive_five_attempts_end_as_failed_with_no_further_attempt() {
        let mut outcomes = vec![Ok(live("srv", "http").await)];
        outcomes.extend((0..10).map(|_| transient()));
        let rig = rig(outcomes);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        rig.script.kill_server();
        wait_for(
            &slot,
            McpServerState::Failed("MCP server `srv`: scripted failure".into()),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        assert_eq!(rig.script.attempts(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_remote_server_with_a_permanent_error_never_retries_in_the_background() {
        let rig = rig_with(vec![permanent()], true);
        let (slot, result) = rig.supervisor.start(spec("http")).await;
        assert!(result.is_err());
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
        assert_eq!(rig.script.attempts(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn interactive_a_stdio_exit_leaves_no_tombstone() {
        let rig = rig(vec![Ok(live_echo("srv", "stdio").await)]);
        let (slot, _) = rig.supervisor.start(spec("stdio")).await;
        rig.script.kill_server();
        wait_for(&slot, McpServerState::Failed("the server exited".into())).await;
        assert_eq!(rig.script.registered(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_stdio_exit_keeps_the_tools_and_a_call_restarts_the_server() {
        let (rig, slot) = headless_stdio(vec![
            Ok(live_echo("srv", "stdio").await),
            Ok(live_echo("srv", "stdio").await),
        ])
        .await;
        rig.script.kill_server();
        wait_for(&slot, failed_with_a_tombstone()).await;
        assert_eq!(rig.script.registered(), 1);
        let stood_in = rig.script.tombstones.lock().unwrap()[0].0.clone();
        assert_eq!(stood_in.len(), 1);
        assert_eq!(stood_in[0].definition.name, "mcp__srv__echo");
        assert_eq!(
            rig.said.warnings.lock().unwrap().clone(),
            ["MCP server `srv` disconnected · run /mcp reconnect srv"]
        );
        // Nothing starts it again until a call asks.
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert_eq!(rig.script.attempts(), 1);

        let restarted = (rig.script.restart())().await.expect("the server restarts");
        assert_eq!(rig.script.attempts(), 2);
        assert_eq!(slot.state(), McpServerState::connected(1));
        assert_eq!(rig.script.unregistered(), 1, "the live tools replaced them");
        assert_eq!(
            dispatch(&restarted, "mcp__srv__echo", json!({"a": 1})).await,
            "echo {\"a\":1}"
        );
        assert_eq!(
            rig.said.notices.lock().unwrap().clone(),
            ["MCP server `srv` restarted"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn headless_concurrent_calls_to_stand_ins_connect_once() {
        let (rig, slot) = headless_stdio(vec![
            Ok(live_echo("srv", "stdio").await),
            Ok(live_echo("srv", "stdio").await),
            Ok(live_echo("srv", "stdio").await),
        ])
        .await;
        rig.script.kill_server();
        wait_for(&slot, failed_with_a_tombstone()).await;
        let restart = rig.script.restart();
        let (a, b, c) = tokio::join!(restart(), restart(), restart());
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        assert_eq!(rig.script.attempts(), 2, "one connect for three calls");
        assert_eq!(slot.restarts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_failed_restart_returns_the_error_and_keeps_the_tombstone() {
        let (rig, slot) = headless_stdio(vec![
            Ok(live_echo("srv", "stdio").await),
            permanent(),
            Ok(live_echo("srv", "stdio").await),
        ])
        .await;
        rig.script.kill_server();
        wait_for(&slot, failed_with_a_tombstone()).await;
        let restart = rig.script.restart();
        let error = restart().await.err().expect("the server does not start");
        assert_eq!(error.kind, McpErrorKind::Permanent);
        assert_eq!(slot.state(), failed_with_a_tombstone());
        assert_eq!(rig.script.unregistered(), 0);
        assert!(lock(&slot.tombstone, "test").is_some());
        assert!(restart().await.is_ok(), "a later call tries again");
        assert_eq!(slot.state(), McpServerState::connected(1));
    }

    #[tokio::test(start_paused = true)]
    async fn headless_the_fourth_loss_removes_the_tombstone_and_fails_the_slot() {
        let mut outcomes = Vec::new();
        for _ in 0..4 {
            outcomes.push(Ok(live_echo("srv", "stdio").await));
        }
        let (rig, slot) = headless_stdio(outcomes).await;
        for _ in 0..MAX_STDIO_RESTARTS {
            rig.script.kill_server();
            wait_for(&slot, failed_with_a_tombstone()).await;
            (rig.script.restart())().await.expect("the server restarts");
        }
        assert_eq!(rig.script.registered(), 3);
        rig.script.kill_server();
        wait_for(
            &slot,
            McpServerState::Failed("exited more than 3 times".into()),
        )
        .await;
        assert_eq!(rig.script.registered(), 3, "no fourth tombstone");
        assert!(lock(&slot.tombstone, "test").is_none());
        assert_eq!(rig.script.attempts(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_first_connection_failure_creates_no_tombstone() {
        let (rig, slot) = {
            let rig = rig_with(vec![permanent()], true);
            let (slot, result) = rig.supervisor.start(spec("stdio")).await;
            assert!(result.is_err());
            (rig, slot)
        };
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
        assert_eq!(rig.script.registered(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_a_manual_reconnect_clears_the_tombstone() {
        let (rig, slot) = headless_stdio(vec![
            Ok(live_echo("srv", "stdio").await),
            Ok(live_echo("srv", "stdio").await),
        ])
        .await;
        rig.script.kill_server();
        wait_for(&slot, failed_with_a_tombstone()).await;
        rig.supervisor.reconnect(&slot).await.expect("reconnects");
        assert_eq!(rig.script.unregistered(), 1);
        assert!(lock(&slot.tombstone, "test").is_none());
        assert_eq!(slot.restarts.load(Ordering::SeqCst), 0);
        // The stale restart no longer starts anything: the server is running.
        let restart = rig.script.restart();
        assert!(restart().await.is_ok());
        assert_eq!(rig.script.attempts(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn headless_removing_a_tombstoned_server_takes_the_stand_ins_out() {
        let (rig, slot) = headless_stdio(vec![Ok(live_echo("srv", "stdio").await)]).await;
        rig.script.kill_server();
        wait_for(&slot, failed_with_a_tombstone()).await;
        let servers = McpServers::default();
        servers.insert(slot.clone());
        let tools = ToolServer::new().run();
        assert_eq!(
            servers.remove("srv", &tools, MCP_DRAIN_BOUND).await,
            Some(true)
        );
        assert_eq!(rig.script.unregistered(), 1);
        let error = (rig.script.restart())()
            .await
            .err()
            .expect("a removed server is not restarted");
        assert!(chain(&error).contains("no longer restarted"));
        assert_eq!(rig.script.attempts(), 1);
    }

    #[cfg(unix)]
    fn as_callback<F: rig_core::serve::adapters::ToolCallback + 'static>(
        callback: F,
    ) -> Arc<dyn rig_core::serve::adapters::ToolCallback> {
        Arc::new(callback)
    }

    // A server process that never answers keeps the connect pending until the call is dropped.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_restart_dropped_mid_connect_puts_the_stand_ins_back() {
        use crate::tools::mcp::Transport;
        use rig_core::completion::ToolDefinition;
        use rig_core::message::ToolName;
        use rig_core::tool::{ToolContext, ToolOutput};

        let options = McpOptions::new(
            Guard {
                gate: Arc::new(Allow),
                results: Arc::new(ResultPipeline::new()),
                hide_mcp_names: false,
                pre_images: None,
                kills: None,
            },
            true,
        );
        let connector = ServerConnector::new(
            options,
            ToolServer::new().run(),
            Arc::new(Default::default()),
            Arc::new(|_| None),
        );
        let mut spec = spec("stdio");
        spec.transport = Transport::Stdio {
            command: "sleep".to_string(),
            args: vec!["30".to_string()],
            env_vars: Vec::new(),
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        let tool = LiveTool {
            definition: ToolDefinition::new(
                ToolName::new("mcp__srv__echo").unwrap(),
                "Echo",
                json!({"type": "object"}),
            ),
            requires_user_interaction: false,
            callback: as_callback(|_: &mut ToolContext, _: serde_json::Value| {
                Box::pin(async move { Ok(ToolOutput::text("live")) })
            }),
        };
        let restart: RestartFn = Arc::new(|| Box::pin(std::future::pending()));
        connector.register_tombstone(&spec, vec![tool], restart);
        let first = lock(&connector.tombstones, "test")
            .get("srv")
            .expect("registered")
            .standing
            .clone();

        let dropped =
            tokio::time::timeout(Duration::from_millis(300), connector.connect(&spec)).await;
        assert!(dropped.is_err(), "the connect never completes");

        assert!(
            !first.load(Ordering::SeqCst),
            "the old stand-ins stay retired"
        );
        let tombstones = lock(&connector.tombstones, "test");
        let stood = tombstones.get("srv").expect("the tombstone is back");
        assert!(stood.standing.load(Ordering::SeqCst));
        assert_eq!(stood.tools.len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unregistering_during_a_connect_keeps_the_stand_ins_gone() {
        use crate::tools::mcp::Transport;
        use rig_core::completion::ToolDefinition;
        use rig_core::message::ToolName;
        use rig_core::tool::ToolOutput;

        let options = McpOptions::new(
            Guard {
                gate: Arc::new(Allow),
                results: Arc::new(ResultPipeline::new()),
                hide_mcp_names: false,
                pre_images: None,
                kills: None,
            },
            true,
        );
        let connector = ServerConnector::new(
            options,
            ToolServer::new().run(),
            Arc::new(Default::default()),
            Arc::new(|_| None),
        );
        let mut spec = spec("stdio");
        spec.transport = Transport::Stdio {
            command: "sleep".to_string(),
            args: vec!["30".to_string()],
            env_vars: Vec::new(),
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        let tool = LiveTool {
            definition: ToolDefinition::new(
                ToolName::new("mcp__srv__echo").unwrap(),
                "Echo",
                json!({"type": "object"}),
            ),
            requires_user_interaction: false,
            callback: as_callback(|_: &mut ToolContext, _: serde_json::Value| {
                Box::pin(async move { Ok(ToolOutput::text("live")) })
            }),
        };
        let restart: RestartFn = Arc::new(|| Box::pin(std::future::pending()));
        connector.register_tombstone(&spec, vec![tool], restart);

        let mut connect = Box::pin(connector.connect(&spec));
        let polled = tokio::time::timeout(Duration::from_millis(100), &mut connect).await;
        assert!(polled.is_err(), "the connect is still in flight");
        connector.unregister_tombstone("srv");
        drop(connect);

        assert!(lock(&connector.tombstones, "test").get("srv").is_none());
    }
}
