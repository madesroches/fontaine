//! Starting MCP servers: a child process over stdio or a streamable-HTTP endpoint.

use std::collections::{BTreeMap, HashMap};
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::error_chain::chain;
use crate::guarded::Guard;
use crate::lock::lock_recovering;
use crate::mcp::auth;
use crate::result_pipeline::ToolOutcome;
use crate::tls::{LoadedNetwork, build_client};
use http::{HeaderName, HeaderValue};
use rig_core::tool::ManagedToolSink;
use rig_rmcp::{McpClientError, McpClientHandler, McpConnection};
use rmcp::ServiceError;
use rmcp::model::{
    ClientCapabilities, ClientConfig, ElicitationCapability, FormElicitationCapability,
    Implementation, ProtocolVersion,
};
use rmcp::service::ClientInitializeError;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, CredentialStore, StateStore,
};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpError,
};
use rmcp::transport::{DynamicTransportError, StreamableHttpClientTransport, TokioChildProcess};
use tokio::sync::Notify;

use super::elicitation::{CallTracker, CancelAll, ElicitationTap, Elicitor};
use super::listings::{Listings, pull_listings};
use super::meta::{MetaTap, ToolMeta};
use super::output::OutputLimits;
use super::register::{GuardedSink, Shared};

const TARGET: &str = "fontaine::telemetry";

/// How long one attempt to start a server and list its tools may take: rmcp sets no deadline of
/// its own.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        /// Names of environment variables passed through. Nothing else of the parent's
        /// environment but a short base set reaches the server.
        env_vars: Vec<String>,
        /// Variables set on the server after the base set and `env_vars`, overriding a copied
        /// value of the same name.
        env: Vec<(OsString, OsString)>,
        cwd: PathBuf,
    },
    Http {
        url: String,
        /// Static headers for the protocol requests; never sent to an authorization server.
        headers: BTreeMap<String, String>,
        /// An executable run at connect time; its `KEY: VALUE` stdout lines become headers,
        /// overriding static headers of the same name.
        headers_helper: Option<String>,
        /// The OAuth stores the transport attaches to its authorization manager; built in
        /// fontaine-core, where the credentials file lives.
        credentials: Arc<dyn CredentialStore>,
        states: Arc<dyn StateStore>,
    },
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::Stdio {
                command,
                args,
                env_vars,
                env,
                cwd,
            } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field("env_vars", env_vars)
                .field("env", env)
                .field("cwd", cwd)
                .finish(),
            Transport::Http {
                url,
                headers,
                headers_helper,
                ..
            } => f
                .debug_struct("Http")
                .field("url", url)
                .field("headers", &headers.keys().collect::<Vec<_>>())
                .field("headers_helper", headers_helper)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
}

impl ServerSpec {
    pub fn kind(&self) -> &'static str {
        match self.transport {
            Transport::Stdio { .. } => "stdio",
            Transport::Http { .. } => "http",
        }
    }
}

/// What a connection needs besides the server it is for.
#[derive(Clone)]
pub struct McpOptions {
    pub guard: Guard,
    /// Whether telemetry may carry the server and tool names.
    pub reveal_names: bool,
    pub limits: Arc<OutputLimits>,
    pub elicitor: Arc<dyn Elicitor>,
    /// Reports a failure the person would care about but the model's call survives.
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

impl McpOptions {
    /// Default limits, no one to answer an elicitation, and warnings to the log.
    pub fn new(guard: Guard, reveal_names: bool) -> Self {
        McpOptions {
            guard,
            reveal_names,
            limits: Arc::new(OutputLimits::default()),
            elicitor: Arc::new(CancelAll),
            warn: Arc::new(|message| tracing::warn!("{message}")),
        }
    }

    fn shown<'a>(&self, name: &'a str) -> &'a str {
        if self.reveal_names {
            name
        } else {
            "third-party"
        }
    }
}

/// Whether trying again could help.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpErrorKind {
    /// The server or the network may come back: a refused or reset connection, a timeout, a
    /// 5xx or 429.
    Transient,
    /// Waiting will not change the outcome: authentication, a protocol failure, a server that
    /// cannot be started.
    Permanent,
}

#[derive(Debug)]
pub struct McpError {
    pub server: String,
    pub kind: McpErrorKind,
    /// What was being done; the cause is the `source`.
    pub message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl McpError {
    pub fn new(
        server: &str,
        kind: McpErrorKind,
        message: impl Into<String>,
        source: Option<Box<dyn StdError + Send + Sync>>,
    ) -> Self {
        McpError {
            server: server.to_string(),
            kind,
            message: message.into(),
            source,
        }
    }

    /// An attempt that ran out of time.
    pub fn timed_out(server: &str, limit: Duration) -> Self {
        McpError::new(
            server,
            McpErrorKind::Transient,
            format!("no answer within {} seconds", limit.as_secs()),
            None,
        )
    }

    fn from_client(server: &str, error: McpClientError) -> Self {
        McpError::new(
            server,
            classify_client_error(&error),
            "could not connect",
            Some(Box::new(error)),
        )
    }

    fn permanent(
        server: &str,
        message: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        McpError::new(
            server,
            McpErrorKind::Permanent,
            message,
            Some(Box::new(source)),
        )
    }

    /// Names the re-login path when the failure is the server's OAuth 401 challenge: waiting
    /// cannot fix that, and the person needs the command that fixes it.
    pub fn with_login_hint_if_auth_required(self) -> Self {
        let auth_required = self.source.as_deref().is_some_and(contains_auth_required);
        if auth_required {
            McpError {
                server: self.server.clone(),
                kind: McpErrorKind::Permanent,
                message: format!(
                    "authentication required; run `fontaine mcp login {}`",
                    self.server
                ),
                source: self.source,
            }
        } else {
            self
        }
    }
}

/// Whether the error chain carries a streamable-HTTP `AuthRequired`: the server answered a
/// protocol request with 401 and no usable stored credentials exist.
fn contains_auth_required(error: &(dyn StdError + Send + Sync + 'static)) -> bool {
    let mut link: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = link {
        if chain_link_has_auth_required(current) {
            return true;
        }
        link = current.source();
    }
    false
}

/// One link of the error chain, looked at directly: the transport errors wrap the typed cause
/// without always marking it `#[source]`, so the known wrappers are opened by hand.
fn chain_link_has_auth_required(current: &(dyn StdError + 'static)) -> bool {
    if let Some(http) = current.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
        return matches!(http, StreamableHttpError::AuthRequired(_));
    }
    if let Some(transport) = current.downcast_ref::<DynamicTransportError>() {
        return boxed_chain_has_auth_required(&*transport.error);
    }
    if let Some(init) = current.downcast_ref::<ClientInitializeError>()
        && let ClientInitializeError::TransportError { error, .. } = init
    {
        return boxed_chain_has_auth_required(&*error.error);
    }
    if let Some(service) = current.downcast_ref::<ServiceError>()
        && let ServiceError::TransportSend(error) = service
    {
        return boxed_chain_has_auth_required(&*error.error);
    }
    false
}

/// Walks the boxed cause of a `DynamicTransportError`, starting at the cause itself.
fn boxed_chain_has_auth_required(first: &(dyn StdError + Send + Sync + 'static)) -> bool {
    let mut link: Option<&(dyn StdError + 'static)> = Some(first);
    while let Some(current) = link {
        if chain_link_has_auth_required(current) {
            return true;
        }
        link = current.source();
    }
    false
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP server `{}`: {}", self.server, self.message)
    }
}

impl StdError for McpError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

pub(crate) fn classify_client_error(error: &McpClientError) -> McpErrorKind {
    match error {
        McpClientError::Connection(init) => classify_init(init),
        McpClientError::Request(service) => classify_service(service),
        McpClientError::Timeout(_) => McpErrorKind::Transient,
    }
}

fn classify_init(error: &ClientInitializeError) -> McpErrorKind {
    match error {
        ClientInitializeError::ConnectionClosed(_)
        | ClientInitializeError::ExpectedInitResponse(None) => McpErrorKind::Transient,
        ClientInitializeError::TransportError { error, .. } => classify_transport(error),
        _ => McpErrorKind::Permanent,
    }
}

pub(crate) fn classify_service(error: &ServiceError) -> McpErrorKind {
    match error {
        ServiceError::TransportSend(error) => classify_transport(error),
        ServiceError::TransportClosed | ServiceError::Timeout { .. } => McpErrorKind::Transient,
        _ => McpErrorKind::Permanent,
    }
}

pub(crate) fn classify_transport(error: &DynamicTransportError) -> McpErrorKind {
    let inner: &(dyn StdError + Send + Sync + 'static) = &*error.error;
    if let Some(http) = inner.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
        classify_http(http)
    } else if inner.is::<std::io::Error>() {
        McpErrorKind::Transient
    } else {
        McpErrorKind::Permanent
    }
}

fn classify_http(error: &StreamableHttpError<reqwest::Error>) -> McpErrorKind {
    match error {
        // Every request error counts: a reset after the TCP connect is neither `is_connect()`
        // nor `is_timeout()`.
        StreamableHttpError::Client(_)
        | StreamableHttpError::Io(_)
        | StreamableHttpError::Sse(_)
        | StreamableHttpError::UnexpectedEndOfStream
        | StreamableHttpError::SessionRecoveryTimeout
        | StreamableHttpError::TransportChannelClosed
        | StreamableHttpError::ControlRequestTimeout => McpErrorKind::Transient,
        StreamableHttpError::HttpStatus(error) => classify_status(error.status),
        StreamableHttpError::UnexpectedServerResponse(text) => classify_status_text(text),
        _ => McpErrorKind::Permanent,
    }
}

fn classify_status(status: u16) -> McpErrorKind {
    if status == 429 || (500..=599).contains(&status) {
        McpErrorKind::Transient
    } else {
        McpErrorKind::Permanent
    }
}

/// Text-form HTTP errors keep no typed status, so the status is read back from the text. Only a
/// 5xx or a 429 is worth another try.
fn classify_status_text(text: &str) -> McpErrorKind {
    if text.starts_with("HTTP 5") || text.starts_with("HTTP 429") {
        McpErrorKind::Transient
    } else {
        McpErrorKind::Permanent
    }
}

/// Whether a failed tool call shows that the server's transport is gone, so the connection has
/// to be replaced: a send that failed for a transient reason, or a closed transport. A timeout
/// does not: a slow tool is not a dead server.
pub(crate) fn signals_transport_failure(outcome: &ToolOutcome, failed: &Notify) {
    let Err(error) = outcome else { return };
    let mut link: Option<&(dyn StdError + 'static)> = StdError::source(error);
    while let Some(cause) = link {
        if let Some(service) = cause.downcast_ref::<ServiceError>() {
            let dead = match service {
                ServiceError::TransportClosed => true,
                ServiceError::TransportSend(error) => {
                    classify_transport(error) == McpErrorKind::Transient
                }
                _ => false,
            };
            if dead {
                // A permit is stored when the watcher is not waiting yet.
                failed.notify_one();
            }
            return;
        }
        link = cause.source();
    }
}

/// A live server. Dropping it closes the connection and retires its tools.
pub struct Connected {
    pub server: String,
    pub connection: McpConnection,
    pub kind: &'static str,
    /// Notified when a tool call shows the transport is dead although the connection has not
    /// closed itself: rmcp's streamable-HTTP worker retries and re-initializes without ever
    /// ending the connection.
    pub transport_failed: Arc<Notify>,
    tool_count: Arc<AtomicUsize>,
    tool_names: Arc<Mutex<Vec<String>>>,
    /// The resources and prompts the server listed, refreshed on a `list_changed`
    /// notification. Advisory: nothing decides on it, so a poisoned lock is read through.
    listings: Arc<Mutex<Listings>>,
    /// Woken by the transport tap when the server announces a resource or prompt list change.
    listings_changed: Arc<Notify>,
    muted: Arc<AtomicBool>,
    tracker: CallTracker,
}

impl Connected {
    /// The registered names of the tools the server has now.
    pub fn tool_names(&self) -> Vec<String> {
        // Only read to remove the tools; a poisoned list still holds the last names written.
        lock_recovering(&self.tool_names).clone()
    }

    /// The server's resource and prompt listings as last pulled.
    pub fn listings(&self) -> Listings {
        // Advisory: a stale listing only misses a mention or command.
        lock_recovering(&self.listings).clone()
    }

    /// Replaces each listing with what a pull answered; a listing the pull could not fetch
    /// keeps the value it already holds.
    pub async fn refresh_listings(&self) {
        let previous = lock_recovering(&self.listings).clone();
        let pulled = pull_listings(&self.connection, &previous).await;
        *lock_recovering(&self.listings) = pulled;
    }

    /// The signal a resource or prompt `list_changed` notification arrives on.
    pub fn listings_changed(&self) -> Arc<Notify> {
        self.listings_changed.clone()
    }

    /// The listings slot itself, for a watcher that re-pulls on [`Self::listings_changed`].
    pub fn listings_slot(&self) -> Arc<Mutex<Listings>> {
        self.listings.clone()
    }

    /// Stops a later `list_changed` refresh from registering the server's tools again. Call it
    /// before removing the tools of a server that is going away.
    pub fn mute(&self) {
        self.muted.store(true, Ordering::SeqCst);
    }

    /// Waits for the calls in flight to finish, up to `bound`. Returns whether they did.
    pub async fn drain(&self, bound: Duration) -> bool {
        tokio::time::timeout(bound, self.tracker.idle())
            .await
            .is_ok()
    }
    /// How many tools the server has registered now: the count follows every `list_changed`
    /// refresh.
    pub fn tool_count(&self) -> usize {
        self.tool_count.load(Ordering::Relaxed)
    }
}

/// Elicitation in form mode only, over the last protocol version whose elicitation arrives as
/// a request: at the newest version it arrives as a result inside the tool call, which rig-rmcp
/// cannot surface.
fn client_config() -> ClientConfig {
    let mut capabilities = ClientCapabilities::default();
    capabilities.elicitation =
        Some(ElicitationCapability::new().with_form(FormElicitationCapability::new()));
    ClientConfig::new(
        capabilities,
        Implementation::new("fontaine", env!("CARGO_PKG_VERSION")),
    )
    .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
}

/// The client rmcp builds for `from_uri` (no idle pooling, same-origin 307/308 redirects only),
/// with `network` applied; rmcp's own cannot take a proxy or extra roots.
fn http_client(network: &LoadedNetwork) -> reqwest::Result<reqwest::Client> {
    build_client(network, || {
        reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(same_origin_redirect_policy())
    })
}

/// Only same-origin 307 and 308 redirects are followed: those keep the method and body, and a
/// cross-origin hop would replay the session id and custom headers to another host.
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let preserves_body = matches!(
            attempt.status(),
            reqwest::StatusCode::TEMPORARY_REDIRECT | reqwest::StatusCode::PERMANENT_REDIRECT
        );
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|original| original.origin() == attempt.url().origin());
        if !preserves_body || !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error(format!("exceeded {MAX_REDIRECTS} redirects"))
        } else {
            attempt.follow()
        }
    })
}

const MAX_REDIRECTS: usize = 10;

fn fail(
    spec: &ServerSpec,
    message: impl Into<String>,
    source: impl StdError + Send + Sync + 'static,
) -> McpError {
    McpError::permanent(&spec.name, message, source)
}

/// A network failure reaching the authorization server may pass; a rejection or a bad
/// configuration will not.
fn classify_auth(error: &AuthError) -> McpErrorKind {
    match error {
        AuthError::HttpError(_)
        | AuthError::DiscoveryRequestFailed { .. }
        | AuthError::TokenRefreshFailed(_) => McpErrorKind::Transient,
        _ => McpErrorKind::Permanent,
    }
}

/// The headers one HTTP server sends on its protocol requests: the static declaration with the
/// helper's output merged over it. A bad name or value, a helper that fails, or a malformed
/// helper line is a permanent failure naming the cause.
async fn declared_headers(
    spec: &ServerSpec,
    headers: &BTreeMap<String, String>,
    headers_helper: Option<&str>,
) -> Result<HashMap<HeaderName, HeaderValue>, McpError> {
    let mut out = HashMap::new();
    for (name, value) in headers {
        insert_header(spec, &mut out, name, value)?;
    }
    if let Some(helper) = headers_helper {
        let output = tokio::process::Command::new(helper)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| fail(spec, format!("cannot run the headers helper `{helper}`"), e))?;
        if !output.status.success() {
            return Err(McpError::new(
                &spec.name,
                McpErrorKind::Permanent,
                format!(
                    "the headers helper `{helper}` exited with {} without outputting headers",
                    output.status
                ),
                None,
            ));
        }
        let text = String::from_utf8(output.stdout).map_err(|e| {
            fail(
                spec,
                format!("the headers helper `{helper}` printed non-UTF-8 output"),
                e,
            )
        })?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(McpError::new(
                    &spec.name,
                    McpErrorKind::Permanent,
                    format!(
                        "the headers helper `{helper}` printed a line without a `KEY: VALUE` pair: {line:?}"
                    ),
                    None,
                ));
            };
            insert_header(spec, &mut out, name.trim(), value.trim())?;
        }
    }
    Ok(out)
}

fn insert_header(
    spec: &ServerSpec,
    out: &mut HashMap<HeaderName, HeaderValue>,
    name: &str,
    value: &str,
) -> Result<(), McpError> {
    let name_parsed = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
        McpError::new(
            &spec.name,
            McpErrorKind::Permanent,
            format!("`{name}` is not a valid HTTP header name"),
            Some(Box::new(e)),
        )
    })?;
    let value_parsed = HeaderValue::from_str(value).map_err(|e| {
        McpError::new(
            &spec.name,
            McpErrorKind::Permanent,
            format!("the value of the `{name}` header is not a valid HTTP header value"),
            Some(Box::new(e)),
        )
    })?;
    out.insert(name_parsed, value_parsed);
    Ok(())
}

/// Starts the server `spec` describes and registers its tools in `sink`, wrapped by the guard
/// in `options`. The tools of a later list-changed notification are wrapped the same way. One
/// attempt gets [`CONNECT_TIMEOUT`].
pub async fn connect<S>(
    spec: &ServerSpec,
    sink: S,
    options: &McpOptions,
    network: &LoadedNetwork,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
{
    let result =
        match tokio::time::timeout(CONNECT_TIMEOUT, start(spec, sink, options, network, env)).await
        {
            Ok(result) => result,
            Err(_) => Err(McpError::timed_out(&spec.name, CONNECT_TIMEOUT)),
        };
    let result = result.map_err(McpError::with_login_hint_if_auth_required);
    tracing::info!(
        target: TARGET,
        server = options.shown(&spec.name),
        transport = spec.kind(),
        status = if result.is_ok() { "connected" } else { "failed" },
        "mcp_server_connection"
    );
    result
}

/// What `serve` wires around one transport.
struct Wiring {
    shared: Shared,
    listings: Arc<Mutex<Listings>>,
    listings_changed: Arc<Notify>,
}

impl Wiring {
    fn new() -> Self {
        Wiring {
            shared: Shared {
                meta: ToolMeta::default(),
                tracker: CallTracker::new(),
                transport_failed: Arc::new(Notify::new()),
                tool_count: Arc::new(AtomicUsize::new(0)),
                tool_names: Arc::default(),
                muted: Arc::new(AtomicBool::new(false)),
            },
            listings: Arc::default(),
            listings_changed: Arc::new(Notify::new()),
        }
    }
}

async fn serve<S, T, E, A>(
    name: &str,
    kind: &'static str,
    transport: T,
    sink: S,
    options: &McpOptions,
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
    T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A>,
    E: StdError + Send + Sync + 'static,
{
    let wiring = Wiring::new();
    let handler = McpClientHandler::new(
        client_config(),
        GuardedSink::new(sink, name, options, &wiring.shared),
    );
    let tapped = ElicitationTap::new(
        MetaTap::new(
            transport.into_transport(),
            wiring.shared.meta.clone(),
            wiring.listings_changed.clone(),
        ),
        name,
        options.shown(name),
        options.elicitor.clone(),
        &wiring.shared.tracker,
    );
    let connection = handler
        .connect(tapped)
        .await
        .map_err(|e| McpError::from_client(name, e))?;
    Ok(Connected {
        server: name.to_string(),
        connection,
        kind,
        transport_failed: wiring.shared.transport_failed,
        tool_count: wiring.shared.tool_count,
        tool_names: wiring.shared.tool_names,
        listings: wiring.listings,
        listings_changed: wiring.listings_changed,
        muted: wiring.shared.muted,
        tracker: wiring.shared.tracker,
    })
}

/// Starts the server behind an already-built transport, such as an in-process pipe, with the
/// same wrapping as [`connect`]. The server's name is `name`.
pub async fn connect_transport<S, T, E, A>(
    name: &str,
    transport: T,
    sink: S,
    options: &McpOptions,
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
    T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A>,
    E: StdError + Send + Sync + 'static,
{
    serve(name, "pipe", transport, sink, options).await
}

async fn start<S>(
    spec: &ServerSpec,
    sink: S,
    options: &McpOptions,
    network: &LoadedNetwork,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
{
    match &spec.transport {
        Transport::Stdio {
            command,
            args,
            env_vars,
            env: set_env,
            cwd,
        } => {
            let mut process = tokio::process::Command::new(command);
            // rmcp's cleanup guard only spawns the kill from Drop, which a runtime that is
            // shutting down (a doctor probe that timed out) never polls.
            process
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .kill_on_drop(true);
            process.envs(crate::child_env::server_env(env, env_vars, set_env));
            let (transport, stderr) = TokioChildProcess::builder(process)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| fail(spec, format!("cannot start `{command}`"), e))?;
            if let Some(stderr) = stderr {
                let name = spec.name.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, BufReader};
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(server = %name, "mcp stderr: {line}");
                    }
                });
            }
            serve(&spec.name, spec.kind(), transport, sink, options).await
        }
        Transport::Http {
            url,
            headers,
            headers_helper,
            credentials,
            states,
        } => {
            let client = http_client(network).map_err(|e| {
                McpError::new(
                    &spec.name,
                    McpErrorKind::Permanent,
                    format!(
                        "could not build the HTTP client: {}",
                        chain(&e.without_url())
                    ),
                    None,
                )
            })?;
            let headers = declared_headers(spec, headers, headers_helper.as_deref()).await?;
            // Every HTTP server gets the OAuth wrapper: rmcp's design is lazy, so an open
            // server pays nothing and a challenging one's 401 drives authorization.
            let oauth = auth::build_oauth_client(network, url)
                .map_err(|e| fail(spec, "could not set up the OAuth HTTP client", e))?;
            let mut manager =
                AuthorizationManager::new_with_oauth_http_client(url.as_str(), Arc::new(oauth))
                    .await
                    .map_err(|e| fail(spec, "could not set up OAuth for the server", e))?;
            manager.set_credential_store(auth::SharedCredentialStore(Arc::clone(credentials)));
            manager.set_state_store(auth::SharedStateStore(Arc::clone(states)));
            // Without this the manager has no OAuth client, so a stored token near expiry
            // cannot be refreshed.
            manager.initialize_from_store().await.map_err(|e| {
                let kind = classify_auth(&e);
                McpError::new(
                    &spec.name,
                    kind,
                    "could not load the stored OAuth credentials",
                    Some(Box::new(e)),
                )
            })?;
            let auth_client = AuthClient::new(client, manager);
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.as_str());
            if !headers.is_empty() {
                config = config.custom_headers(headers);
            }
            let transport = StreamableHttpClientTransport::with_client(auth_client, config);
            serve(&spec.name, spec.kind(), transport, sink, options).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{CallView, GateDecision, ToolGate};
    use crate::result_pipeline::ResultPipeline;
    use futures::FutureExt;
    use futures::future::BoxFuture;
    use rig_agent::tool::server::ToolServer;
    use rig_core::tool::ToolExecutionError;
    use rmcp::transport::streamable_http_client::{AuthRequiredError, HttpStatusError};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Allow;

    impl ToolGate for Allow {
        fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
            let args = call.args.clone();
            Box::pin(async move { GateDecision::Allow { args } })
        }
    }

    fn http_transport(url: &str) -> Transport {
        Transport::Http {
            url: url.to_string(),
            headers: std::collections::BTreeMap::new(),
            headers_helper: None,
            credentials: Arc::new(rmcp::transport::auth::InMemoryCredentialStore::new()),
            states: Arc::new(rmcp::transport::auth::InMemoryStateStore::new()),
        }
    }

    fn options() -> McpOptions {
        McpOptions::new(
            Guard {
                gate: Arc::new(Allow),
                results: Arc::new(ResultPipeline::new()),
                hide_mcp_names: false,
                pre_images: None,
                kills: None,
            },
            true,
        )
    }

    async fn http_error(url: &str) -> McpError {
        let spec = ServerSpec {
            name: "remote".to_string(),
            transport: http_transport(url),
        };
        connect(
            &spec,
            ToolServer::new().run(),
            &options(),
            &LoadedNetwork::default(),
            &|_| None,
        )
        .await
        .err()
        .expect("the connection fails")
    }

    async fn answered_with(template: ResponseTemplate) -> McpError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(template)
            .mount(&server)
            .await;
        http_error(&format!("{}/mcp", server.uri())).await
    }

    #[tokio::test]
    async fn a_refused_connection_is_transient() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = http_error(&format!("http://127.0.0.1:{port}/mcp")).await;
        assert_eq!(error.kind, McpErrorKind::Transient, "{}", chain(&error));
    }

    #[tokio::test]
    async fn a_connection_reset_after_the_tcp_connect_is_transient() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // wiremock cannot drop a connection, so a bare listener accepts and hangs up.
            while let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });
        let error = http_error(&format!("http://127.0.0.1:{port}/mcp")).await;
        assert_eq!(error.kind, McpErrorKind::Transient, "{}", chain(&error));
    }

    #[tokio::test]
    async fn an_end_of_stream_during_initialize_is_transient() {
        let (c2s, sfc) = tokio::io::duplex(1024);
        let (s2c, cfs) = tokio::io::duplex(1024);
        drop((sfc, s2c));
        let error = connect_transport("pipe", (cfs, c2s), ToolServer::new().run(), &options())
            .await
            .err()
            .expect("nobody answers");
        assert_eq!(error.kind, McpErrorKind::Transient, "{}", chain(&error));
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_runs_out_the_connect_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let error = http_error(&format!("http://127.0.0.1:{port}/mcp")).await;
        assert_eq!(error.kind, McpErrorKind::Transient);
        assert!(error.message.contains("30 seconds"), "{}", error.message);
    }

    #[tokio::test]
    async fn a_5xx_and_a_429_are_transient() {
        for status in [500, 503, 429] {
            let error = answered_with(ResponseTemplate::new(status)).await;
            assert_eq!(
                error.kind,
                McpErrorKind::Transient,
                "{status}: {}",
                chain(&error)
            );
        }
    }

    #[tokio::test]
    async fn other_statuses_are_permanent() {
        for status in [400, 403, 404, 405] {
            let error = answered_with(ResponseTemplate::new(status)).await;
            assert_eq!(
                error.kind,
                McpErrorKind::Permanent,
                "{status}: {}",
                chain(&error)
            );
        }
    }

    #[tokio::test]
    async fn a_401_is_permanent_with_and_without_a_www_authenticate_header() {
        let with = answered_with(
            ResponseTemplate::new(401).insert_header("www-authenticate", "Bearer realm=\"mcp\""),
        )
        .await;
        assert_eq!(with.kind, McpErrorKind::Permanent, "{}", chain(&with));
        let without = answered_with(ResponseTemplate::new(401)).await;
        assert_eq!(without.kind, McpErrorKind::Permanent, "{}", chain(&without));
    }

    #[tokio::test]
    async fn a_non_2xx_answer_with_a_json_rpc_error_body_is_permanent() {
        let error = answered_with(ResponseTemplate::new(500).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "error": {"code": -32000, "message": "boom"},
        })))
        .await;
        assert_eq!(error.kind, McpErrorKind::Permanent, "{}", chain(&error));
    }

    #[tokio::test]
    async fn a_server_that_cannot_be_spawned_is_permanent() {
        let spec = ServerSpec {
            name: "ghost".to_string(),
            transport: Transport::Stdio {
                command: "fontaine-no-such-program".to_string(),
                args: Vec::new(),
                env_vars: Vec::new(),
                env: Vec::new(),
                cwd: std::env::temp_dir(),
            },
        };
        let error = connect(
            &spec,
            ToolServer::new().run(),
            &options(),
            &LoadedNetwork::default(),
            &|_| None,
        )
        .await
        .err()
        .expect("it cannot start");
        assert_eq!(error.kind, McpErrorKind::Permanent);
        assert!(
            chain(&error).contains("fontaine-no-such-program"),
            "{}",
            chain(&error)
        );
    }

    fn transport_error(error: impl StdError + Send + Sync + 'static) -> DynamicTransportError {
        DynamicTransportError::from_parts("test", std::any::TypeId::of::<()>(), Box::new(error))
    }

    fn auth_required() -> StreamableHttpError<reqwest::Error> {
        StreamableHttpError::AuthRequired(AuthRequiredError::new("Bearer".to_string()))
    }

    #[test]
    fn client_errors_classify_by_their_typed_cause() {
        use rmcp::model::{ErrorCode, ErrorData};
        let timeout = McpClientError::Timeout(Duration::from_secs(1));
        assert_eq!(classify_client_error(&timeout), McpErrorKind::Transient);

        let mcp_error = ServiceError::McpError(ErrorData::new(ErrorCode(-32000), "no", None));
        assert_eq!(
            classify_client_error(&McpClientError::Request(mcp_error)),
            McpErrorKind::Permanent
        );
        let send_auth = ServiceError::TransportSend(transport_error(auth_required()));
        assert_eq!(
            classify_client_error(&McpClientError::Request(send_auth)),
            McpErrorKind::Permanent
        );
        let send_reset = ServiceError::TransportSend(transport_error(
            StreamableHttpError::<reqwest::Error>::UnexpectedEndOfStream,
        ));
        assert_eq!(
            classify_client_error(&McpClientError::Request(send_reset)),
            McpErrorKind::Transient
        );
        for transient in [
            ServiceError::TransportClosed,
            ServiceError::Timeout {
                timeout: Duration::from_secs(1),
            },
        ] {
            assert_eq!(classify_service(&transient), McpErrorKind::Transient);
        }
        assert_eq!(
            classify_service(&ServiceError::UnexpectedResponse),
            McpErrorKind::Permanent
        );
        let closed = ClientInitializeError::ConnectionClosed("eof".into());
        assert_eq!(
            classify_client_error(&McpClientError::Connection(closed)),
            McpErrorKind::Transient
        );
        let rpc = ClientInitializeError::JsonRpcError(rmcp::model::ErrorData::new(
            rmcp::model::ErrorCode(-32600),
            "bad",
            None,
        ));
        assert_eq!(
            classify_client_error(&McpClientError::Connection(rpc)),
            McpErrorKind::Permanent
        );
    }

    #[test]
    fn typed_http_errors_classify_as_their_variant_says() {
        for transient in [
            StreamableHttpError::<reqwest::Error>::UnexpectedEndOfStream,
            StreamableHttpError::SessionRecoveryTimeout,
            StreamableHttpError::TransportChannelClosed,
            StreamableHttpError::ControlRequestTimeout,
            StreamableHttpError::Io(std::io::Error::other("reset")),
            StreamableHttpError::HttpStatus(HttpStatusError::new(503, None)),
        ] {
            assert_eq!(
                classify_http(&transient),
                McpErrorKind::Transient,
                "{transient}"
            );
        }
        for permanent in [
            StreamableHttpError::<reqwest::Error>::SessionExpired,
            StreamableHttpError::HttpStatus(HttpStatusError::new(400, None)),
            StreamableHttpError::MissingSessionIdInResponse,
            StreamableHttpError::ReinitializationFailed(rmcp::model::ErrorData::new(
                rmcp::model::ErrorCode(-1),
                "no",
                None,
            )),
            auth_required(),
        ] {
            assert_eq!(
                classify_http(&permanent),
                McpErrorKind::Permanent,
                "{permanent}"
            );
        }
        assert_eq!(
            classify_transport(&transport_error(std::io::Error::other("pipe closed"))),
            McpErrorKind::Transient
        );
        assert_eq!(
            classify_transport(&transport_error(fmt_error())),
            McpErrorKind::Permanent
        );
    }

    fn fmt_error() -> std::fmt::Error {
        std::fmt::Error
    }

    #[test]
    fn a_status_text_is_transient_only_for_5xx_and_429() {
        for text in [
            "HTTP 500 Internal Server Error: boom",
            "HTTP 503 Service Unavailable: ",
            "HTTP 429 Too Many Requests: slow down",
        ] {
            assert_eq!(
                classify_status_text(text),
                McpErrorKind::Transient,
                "{text}"
            );
        }
        for text in [
            "HTTP 404 Not Found: gone",
            "HTTP 401 Unauthorized: ",
            "HTTP 400 Bad Request: ",
            "Unexpected JSON-RPC body: {}",
            "500 without a prefix",
            "",
        ] {
            assert_eq!(
                classify_status_text(text),
                McpErrorKind::Permanent,
                "{text}"
            );
        }
    }

    #[test]
    fn a_timed_out_attempt_is_transient_and_says_how_long() {
        let error = McpError::timed_out("srv", Duration::from_secs(30));
        assert_eq!(error.kind, McpErrorKind::Transient);
        assert!(error.to_string().contains("no answer within 30 seconds"));
    }

    #[test]
    fn the_cause_stays_in_the_source_chain_not_the_message() {
        let error = McpError::new(
            "srv",
            McpErrorKind::Permanent,
            "could not connect",
            Some(Box::new(std::io::Error::other("the cause"))),
        );
        assert_eq!(error.to_string(), "MCP server `srv`: could not connect");
        assert_eq!(
            chain(&error),
            "MCP server `srv`: could not connect: the cause"
        );
    }

    fn failed_with(source: ServiceError) -> (ToolOutcome, Notify) {
        let outcome = Err(ToolExecutionError::provider("call failed").with_source(source));
        (outcome, Notify::new())
    }

    fn notified(failed: &Notify) -> bool {
        failed.notified().now_or_never().is_some()
    }

    #[test]
    fn a_transient_send_failure_or_a_closed_transport_signals_the_connection_is_dead() {
        let reset = ServiceError::TransportSend(transport_error(
            StreamableHttpError::<reqwest::Error>::UnexpectedEndOfStream,
        ));
        for source in [reset, ServiceError::TransportClosed] {
            let (outcome, failed) = failed_with(source);
            signals_transport_failure(&outcome, &failed);
            assert!(notified(&failed));
        }
    }

    #[test]
    fn a_timeout_or_a_permanent_send_failure_does_not_signal() {
        let sources = [
            ServiceError::Timeout {
                timeout: Duration::from_secs(1),
            },
            ServiceError::TransportSend(transport_error(auth_required())),
            ServiceError::UnexpectedResponse,
        ];
        for source in sources {
            let (outcome, failed) = failed_with(source);
            signals_transport_failure(&outcome, &failed);
            assert!(!notified(&failed));
        }
        let failed = Notify::new();
        signals_transport_failure(&Ok(rig_core::tool::ToolOutput::text("fine")), &failed);
        let sourceless = Err(ToolExecutionError::other("the tool said no"));
        signals_transport_failure(&sourceless, &failed);
        assert!(!notified(&failed));
    }

    #[test]
    fn the_client_offers_the_last_initialize_version_and_the_form_capability() {
        let config = client_config();
        assert_eq!(
            config.protocol_version,
            ProtocolVersion::LATEST_WITH_INITIALIZE
        );
        let elicitation = config
            .capabilities
            .elicitation
            .expect("elicitation is declared");
        assert!(elicitation.form.is_some());
        assert!(elicitation.url.is_none());
    }

    async fn status_of(client: &reqwest::Client, url: String) -> u16 {
        client.post(url).send().await.unwrap().status().as_u16()
    }

    #[tokio::test]
    async fn a_same_origin_307_is_followed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/b"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = http_client(&LoadedNetwork::default()).unwrap();
        assert_eq!(status_of(&client, format!("{}/a", server.uri())).await, 200);
    }

    #[tokio::test]
    async fn a_302_is_returned_unfollowed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/b"))
            .mount(&server)
            .await;
        let client = http_client(&LoadedNetwork::default()).unwrap();
        assert_eq!(status_of(&client, format!("{}/a", server.uri())).await, 302);
    }

    #[tokio::test]
    async fn a_cross_origin_307_is_returned_unfollowed() {
        let target = MockServer::start().await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/a"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/b", target.uri()).as_str()),
            )
            .mount(&server)
            .await;
        let client = http_client(&LoadedNetwork::default()).unwrap();
        assert_eq!(status_of(&client, format!("{}/a", server.uri())).await, 307);
        assert!(target.received_requests().await.unwrap().is_empty());
    }
}

#[cfg(test)]
mod send_checks {
    use super::*;

    #[test]
    fn connected_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Connected>();
    }

    #[test]
    fn pull_listings_future_is_send() {
        async fn pull(connection: &McpConnection) {
            let _ = super::super::listings::pull_listings(connection, &Default::default()).await;
        }
        fn check_send<F: Send>(_: F) {}
        check_send(pull);
    }
}
