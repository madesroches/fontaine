//! Keeping the session's MCP servers alive: a slot per server the session attempted, a watcher per
//! live connection, and the reconnect and startup-retry schedules. A remote server that drops is
//! reconnected with exponential backoff; a stdio server that exits is marked failed, since
//! starting its process again is a decision for the person.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use fontaine_tools::error_chain::chain;
use fontaine_tools::lock::{lock, read_recovering, write_recovering};
use fontaine_tools::mcp::McpConnectionHandle as McpConnection;
use fontaine_tools::mcp::{
    CONNECT_TIMEOUT, Connected, McpError, McpErrorKind, McpOptions, ServerSpec,
    connect as connect_server,
};
use fontaine_tools::tls::LoadedNetwork;
use futures::future::BoxFuture;
use rig_agent::tool::server::ToolServerHandle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::permissions::Approver;

use super::EnvLookup;

/// The first reconnect waits this long, and each later one twice as long as the last.
const RECONNECT_BASE: Duration = Duration::from_secs(1);
/// How many times a lost remote server is started again before it is marked failed.
pub const RECONNECT_ATTEMPTS: u32 = 5;
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
    /// Running, with `tools` registered now.
    Connected { tools: usize },
    /// Lost, and being started again; `attempt` runs from 1 to 5.
    Reconnecting { attempt: u32 },
    /// Not running, with the reason as a rendered error chain.
    Failed(String),
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
}

pub(crate) struct ServerConnector {
    pub(crate) options: McpOptions,
    pub(crate) tool_server: ToolServerHandle,
    pub(crate) network: Arc<LoadedNetwork>,
    pub(crate) env: EnvLookup,
}

impl McpConnector for ServerConnector {
    fn connect<'a>(&'a self, spec: &'a ServerSpec) -> BoxFuture<'a, Result<Connected, McpError>> {
        Box::pin(async move {
            let env = self.env.clone();
            connect_server(
                spec,
                self.tool_server.clone(),
                &self.options,
                &self.network,
                &move |name| env(name),
            )
            .await
        })
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
        }
    }

    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The server's state; a connected server's tool count is read from the connection the slot
    /// holds, so it follows a `list_changed` refresh and a reconnect.
    pub fn state(&self) -> McpServerState {
        let state = read_recovering(&self.state).clone();
        match state {
            McpServerState::Connected { .. } => McpServerState::Connected {
                tools: lock(&self.connection, "MCP connection")
                    .as_ref()
                    .map_or(0, Connected::tool_count),
            },
            other => other,
        }
    }

    fn set_state(&self, state: McpServerState) {
        *write_recovering(&self.state) = state;
    }

    fn is_remote(&self) -> bool {
        self.spec.kind() == "http"
    }

    fn stop_watcher(&self) {
        if let Some(handle) = lock(&self.watcher, "MCP watcher").take() {
            handle.abort();
        }
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

/// The session's MCP servers, by name.
#[derive(Default)]
pub struct McpServers {
    slots: BTreeMap<String, Arc<ServerSlot>>,
    /// Set by the first start; the one that restarts a server by hand.
    pub(crate) supervisor: Option<Arc<Supervisor>>,
}

impl McpServers {
    pub fn states(&self) -> Vec<(String, McpServerState)> {
        self.slots
            .iter()
            .map(|(name, slot)| (name.clone(), slot.state()))
            .collect()
    }

    pub(crate) fn slot(&self, name: &str) -> Option<Arc<ServerSlot>> {
        self.slots.get(name).cloned()
    }

    pub(crate) fn failed(&self) -> Vec<Arc<ServerSlot>> {
        self.slots
            .values()
            .filter(|slot| matches!(slot.state(), McpServerState::Failed(_)))
            .cloned()
            .collect()
    }

    pub(crate) fn insert(&mut self, slot: Arc<ServerSlot>) {
        if let Some(old) = self.slots.insert(slot.name().to_string(), slot) {
            retire(&old);
        }
    }

    /// Unloads the server `name`: its slot is retired, its `list_changed` sink muted (a refresh
    /// during the drain would register the tools again), its tools removed from `tools` so no new
    /// call starts, the calls in flight given up to `bound` to finish, and the connection closed.
    /// Returns `None` for a server with no slot, else whether every call finished in time.
    pub(crate) async fn remove(
        &mut self,
        name: &str,
        tools: &ToolServerHandle,
        bound: Duration,
    ) -> Option<bool> {
        let slot = self.slots.remove(name)?;
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

/// Ends a slot's watcher and marks its connection as closed by us.
fn retire(slot: &ServerSlot) {
    slot.closing.store(true, Ordering::SeqCst);
    slot.stop_watcher();
}

impl Drop for McpServers {
    /// The connections close as the slots drop, and a watcher must not mistake that for a loss.
    fn drop(&mut self) {
        for slot in self.slots.values() {
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
}

fn disconnected(name: &str) -> String {
    format!("MCP server `{name}` disconnected · run /mcp reconnect {name}")
}

impl Supervisor {
    pub(crate) fn new(
        connector: Arc<dyn McpConnector>,
        approver: Arc<dyn Approver>,
        refresh: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Supervisor> {
        Arc::new(Supervisor {
            connector,
            approver,
            refresh,
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
                let slot = Arc::new(ServerSlot::new(
                    spec,
                    McpServerState::Connected { tools: 0 },
                ));
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
        *lock(&slot.connection, "MCP connection") = Some(connected);
        slot.closing.store(false, Ordering::SeqCst);
        slot.set_state(McpServerState::Connected { tools: 0 });
        let new = tokio::spawn(self.clone().watch(slot.clone(), connection, failed));
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
    ) {
        if slot.is_remote() {
            tokio::select! {
                _ = connection.closed() => {}
                _ = failed.notified() => {
                    // rmcp's HTTP worker never ends the connection of a dead server, so it is
                    // closed here, which retires its tools.
                    connection.close().await;
                }
            }
        } else {
            connection.closed().await;
        }
        if slot.closing.load(Ordering::SeqCst) {
            return;
        }
        drop(lock(&slot.connection, "MCP connection").take());
        if slot.is_remote() {
            self.reconnect_with_backoff(&slot).await;
        } else {
            slot.set_state(McpServerState::Failed("the server exited".to_string()));
            self.approver.warn(&disconnected(slot.name()));
        }
    }

    async fn reconnect_with_backoff(self: &Arc<Self>, slot: &Arc<ServerSlot>) {
        let mut last = None;
        let generation = slot.generation.load(Ordering::SeqCst);
        for attempt in 1..=RECONNECT_ATTEMPTS {
            slot.set_state(McpServerState::Reconnecting { attempt });
            tokio::time::sleep(RECONNECT_BASE * (1 << (attempt - 1))).await;
            match self.attempt(&slot.spec, CONNECT_TIMEOUT).await {
                Ok(connected) => {
                    if let Err(stale) = self.install(slot, connected, generation) {
                        stale.connection.close().await;
                        return;
                    }
                    self.approver
                        .notice(&format!("MCP server `{}` reconnected", slot.name()));
                    (self.refresh)();
                    return;
                }
                Err(error) if error.kind == McpErrorKind::Permanent => {
                    last = Some(error);
                    break;
                }
                Err(error) => last = Some(error),
            }
        }
        let reason = last.map_or_else(|| "no reason".to_string(), |error| chain(&error));
        slot.set_state(McpServerState::Failed(reason));
        self.approver.warn(&disconnected(slot.name()));
    }

    /// Restarts `slot` by hand, with no backoff: its watcher stops, its connection closes, and a
    /// new one takes its place.
    pub(crate) async fn reconnect(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
    ) -> Result<(), McpError> {
        let generation = slot.supersede();
        slot.closing.store(true, Ordering::SeqCst);
        let old = lock(&slot.connection, "MCP connection").take();
        if let Some(old) = old {
            old.connection.close().await;
        }
        match self.attempt(&slot.spec, CONNECT_TIMEOUT).await {
            Ok(connected) => {
                match self.install(slot, connected, generation) {
                    Ok(()) => (self.refresh)(),
                    Err(stale) => stale.connection.close().await,
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

    use fontaine_tools::gate::{CallView, GateDecision, ToolGate};
    use fontaine_tools::guarded::Guard;
    use fontaine_tools::mcp::connect_transport;
    use fontaine_tools::result_pipeline::ResultPipeline;
    use rig_agent::tool::server::ToolServer;
    use rmcp::model::{ServerCapabilities, ServerConfig};
    use rmcp::{ServerHandler, ServiceExt};

    use crate::permissions::DenyAll;

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

    /// One live in-process server: the connection to it, and the task that serves it, which a
    /// test aborts to make the server go away.
    struct Live {
        connected: Connected,
        server: JoinHandle<()>,
    }

    async fn live(name: &str, kind: &'static str) -> Live {
        let (c2s, sfc) = tokio::io::duplex(8192);
        let (s2c, cfs) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let running = Empty.serve((sfc, s2c)).await.expect("server start");
            let _ = running.waiting().await;
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
    }

    impl Script {
        fn new(outcomes: Vec<Outcome>) -> Arc<Script> {
            Arc::new(Script {
                outcomes: Mutex::new(outcomes.into()),
                attempts: AtomicUsize::new(0),
                servers: Mutex::new(Vec::new()),
                times: Mutex::new(Vec::new()),
            })
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
            request: crate::permissions::ApprovalRequest,
        ) -> BoxFuture<'a, crate::permissions::Answer> {
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
        );
        Rig {
            script,
            said,
            refreshed,
            supervisor,
        }
    }

    fn spec(kind: &str) -> ServerSpec {
        use fontaine_tools::mcp::Transport;
        ServerSpec {
            name: "srv".to_string(),
            transport: match kind {
                "http" => Transport::Http {
                    url: "http://unused.test/mcp".to_string(),
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
        for _ in 0..200 {
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
        wait_for(&slot, McpServerState::Connected { tools: 0 }).await;
        assert_eq!(rig.script.attempts(), 3);
        assert_eq!(rig.refreshed.load(Ordering::SeqCst), 1);
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
        assert_eq!(slot.state(), McpServerState::Connected { tools: 0 });
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
        assert_eq!(slot.state(), McpServerState::Connected { tools: 0 });
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
        assert_eq!(slot.state(), McpServerState::Connected { tools: 0 });
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
        assert_eq!(slot.state(), McpServerState::Connected { tools: 0 });
        let error = rig.supervisor.reconnect(&slot).await.unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Permanent);
        assert!(matches!(slot.state(), McpServerState::Failed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_servers_ends_their_watchers_without_a_reconnect() {
        let rig = rig(vec![Ok(live("srv", "http").await)]);
        let (slot, _) = rig.supervisor.start(spec("http")).await;
        let mut servers = McpServers::default();
        servers.insert(slot.clone());
        assert_eq!(
            servers.states(),
            [("srv".to_string(), McpServerState::Connected { tools: 0 })]
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
        let mut servers = McpServers::default();
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
    async fn removing_a_failed_server_just_drops_its_slot() {
        let rig = rig(vec![permanent()]);
        let (slot, _) = rig.supervisor.start(spec("stdio")).await;
        let mut servers = McpServers::default();
        servers.insert(slot);
        let tools = ToolServer::new().run();
        assert_eq!(
            servers.remove("srv", &tools, MCP_DRAIN_BOUND).await,
            Some(true)
        );
        assert!(servers.states().is_empty());
    }
}
