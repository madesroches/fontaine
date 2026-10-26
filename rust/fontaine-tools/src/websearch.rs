//! The `websearch` tool: one query to a configured backend (Tavily, or a SearXNG instance that
//! Fontaine may start itself), returned as a numbered list of titles, URLs and snippets.
//! Provider-native search does not pass through here.

use std::ffi::OsString;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use url::Url;

use crate::error_chain::chain;
use crate::lock::lock_recovering;

pub const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

/// Results returned when the configuration does not say.
pub const DEFAULT_MAX_RESULTS: usize = 10;

/// How long a launched SearXNG instance has to answer, when the configuration does not say.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_INTERVAL: Duration = Duration::from_millis(100);
const STDERR_TAIL_BYTES: usize = 2048;

/// A backend API key. Not `Debug`-derived: the value is a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        ApiKey(key.into())
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(..)")
    }
}

/// How Fontaine starts a SearXNG instance: a fixed argv, no shell, a cleared environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub startup_timeout: Duration,
    pub env: Vec<(OsString, OsString)>,
}

/// What a managed start runs, decided when the first search needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedPlan {
    pub argv: Vec<String>,
    pub base_url: Url,
    pub env: Vec<(OsString, OsString)>,
}

type Prepare = dyn Fn() -> std::io::Result<ManagedPlan> + Send + Sync;

/// A SearXNG instance Fontaine decides how to run itself, not the user. The plan is made on the
/// first search, not at configuration time, because it writes a file and picks a free port.
#[derive(Clone)]
pub struct Managed {
    prepare: Arc<Prepare>,
    pub startup_timeout: Duration,
}

impl Managed {
    pub fn new(
        startup_timeout: Duration,
        prepare: impl Fn() -> std::io::Result<ManagedPlan> + Send + Sync + 'static,
    ) -> Self {
        Managed {
            prepare: Arc::new(prepare),
            startup_timeout,
        }
    }
}

impl std::fmt::Debug for Managed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Managed")
            .field("startup_timeout", &self.startup_timeout)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Managed {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.prepare, &other.prepare) && self.startup_timeout == other.startup_timeout
    }
}

impl Eq for Managed {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendConfig {
    Tavily {
        api_key: ApiKey,
        endpoint: Url,
    },
    Searxng {
        base_url: Url,
        launch: Option<Launch>,
    },
    Managed(Managed),
}

impl BackendConfig {
    pub fn tavily(api_key: ApiKey) -> Self {
        BackendConfig::Tavily {
            api_key,
            endpoint: Url::parse(TAVILY_ENDPOINT).expect("the Tavily endpoint is a valid URL"),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            BackendConfig::Tavily { .. } => "Tavily",
            BackendConfig::Searxng { .. } | BackendConfig::Managed(_) => "SearXNG",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebsearchConfig {
    pub backend: BackendConfig,
    pub max_results: usize,
    pub timeout: Duration,
    /// Characters of formatted results returned.
    pub output_cap_chars: usize,
}

impl WebsearchConfig {
    pub fn new(backend: BackendConfig) -> Self {
        WebsearchConfig {
            backend,
            max_results: DEFAULT_MAX_RESULTS,
            timeout: Duration::from_secs(30),
            output_cap_chars: 100_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebsearchArgs {
    pub query: String,
    pub allowed_domains: Option<Vec<String>>,
    pub blocked_domains: Option<Vec<String>>,
}

/// How long a stopped SearXNG command has to exit after SIGTERM before it is killed. A
/// `docker run` needs the time to stop its container, which `--rm` then removes.
const STOP_GRACE: Duration = Duration::from_secs(10);
const STOP_POLL: Duration = Duration::from_millis(50);

#[derive(Debug)]
struct Running {
    // Taken by `drop`, which hands the child to a detached task.
    child: Option<Child>,
    // Recorded at spawn: tokio's `Child::id` is None once the leader has been reaped, but a
    // wrapper that backgrounded the server leaves the group alive.
    pid: Option<u32>,
    grace: Duration,
}

impl Running {
    fn new(child: Child, grace: Duration) -> Self {
        Running {
            pid: child.id(),
            child: Some(child),
            grace,
        }
    }

    fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("the child is only taken by drop")
    }
}

impl Drop for Running {
    /// SIGTERM goes out now; waiting out the grace and the SIGKILL run detached, because the
    /// task dropping this (the TUI's, on Esc) must not block.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let (pid, grace) = (self.pid, self.grace);
        terminate(pid);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                settle(&mut child, pid, grace).await;
            });
        } else {
            std::thread::spawn(move || {
                let deadline = Instant::now() + grace;
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                    std::thread::sleep(STOP_POLL);
                }
                force_kill(&mut child, pid);
                for _ in 0..20 {
                    if !matches!(child.try_wait(), Ok(None)) {
                        break;
                    }
                    std::thread::sleep(STOP_POLL);
                }
            });
        }
    }
}

fn command(
    program: &str,
    args: &[String],
    env: &[(OsString, OsString)],
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own group, so the signals reach what a wrapper script started too.
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// SIGTERM to the process group: a `docker run` CLI forwards it to the container, which SIGKILL
/// to the CLI alone could not.
fn terminate(pid: Option<u32>) {
    #[cfg(unix)]
    signal_group(pid, "TERM");
    #[cfg(not(unix))]
    let _ = pid;
}

/// Kills the group, even when the leader already exited (a wrapper may have left the server
/// running), and the child itself where there is no group to signal.
fn force_kill(child: &mut Child, pid: Option<u32>) {
    #[cfg(unix)]
    signal_group(pid, "KILL");
    #[cfg(not(unix))]
    let _ = pid;
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.start_kill();
    }
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: &str) {
    let Some(pid) = pid else { return };
    // Best effort: the group may already be gone.
    let _ = std::process::Command::new("kill")
        .args(["-s", signal, "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

async fn stop(child: &mut Child, pid: Option<u32>, grace: Duration) {
    terminate(pid);
    settle(child, pid, grace).await;
}

/// Waits for the exit that a SIGTERM already asked for, then kills what is left.
async fn settle(child: &mut Child, pid: Option<u32>, grace: Duration) {
    let deadline = Instant::now() + grace;
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        tokio::time::sleep(STOP_POLL).await;
    }
    force_kill(child, pid);
    let _ = child.wait().await;
}

/// The SearXNG instance Fontaine started, shared by every `Websearch` a session builds so a
/// rebuilt registry reuses it. Dropping the last handle kills the process.
#[derive(Debug, Default)]
pub struct SearxngChild {
    // The address is kept with the process: a managed start picks its port per start.
    slot: tokio::sync::Mutex<Option<(Running, Url)>>,
}

impl SearxngChild {
    /// Whether a launched process is alive now.
    pub async fn is_running(&self) -> bool {
        let mut slot = self.slot.lock().await;
        slot.as_mut()
            .is_some_and(|(running, _)| matches!(running.child().try_wait(), Ok(None)))
    }
}

#[derive(Clone, Debug)]
pub struct Websearch {
    client: reqwest::Client,
    config: WebsearchConfig,
    child: Arc<SearxngChild>,
}

#[derive(Debug, PartialEq, Eq)]
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct SearxngResponse {
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: Option<String>,
    url: String,
    #[serde(default)]
    content: Option<String>,
}

fn parse_tavily(body: &[u8]) -> Result<Vec<Hit>, ToolExecutionError> {
    let parsed: TavilyResponse = serde_json::from_slice(body).map_err(|e| {
        ToolExecutionError::other(format!("Tavily answered with unexpected JSON: {e}"))
    })?;
    Ok(parsed
        .results
        .into_iter()
        .map(|r| Hit {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect())
}

fn parse_searxng(body: &[u8]) -> Result<Vec<Hit>, ToolExecutionError> {
    let parsed: SearxngResponse = serde_json::from_slice(body).map_err(|e| {
        ToolExecutionError::other(format!("SearXNG answered with unexpected JSON: {e}"))
    })?;
    Ok(parsed
        .results
        .into_iter()
        .map(|r| Hit {
            title: r.title.unwrap_or_default(),
            url: r.url,
            snippet: r.content.unwrap_or_default(),
        })
        .collect())
}

fn domain_matches(host: &str, domain: &str) -> bool {
    let domain = domain
        .trim()
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !domain.is_empty()
        && (host == domain
            || host
                .strip_suffix(&domain)
                .is_some_and(|rest| rest.ends_with('.')))
}

fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}

fn apply_domain_filters(hits: Vec<Hit>, allowed: &[String], blocked: &[String]) -> Vec<Hit> {
    hits.into_iter()
        .filter(|hit| {
            let host = host_of(&hit.url);
            let on = |list: &[String]| {
                host.as_deref()
                    .is_some_and(|host| list.iter().any(|d| domain_matches(host, d)))
            };
            (allowed.is_empty() || on(allowed)) && !on(blocked)
        })
        .collect()
}

fn format_hits(hits: &[Hit], max_results: usize, cap_chars: usize) -> String {
    if hits.is_empty() {
        return "No results were found for that query.".to_string();
    }
    let mut text = String::new();
    for (i, hit) in hits.iter().take(max_results).enumerate() {
        if i > 0 {
            text.push('\n');
        }
        let title = if hit.title.trim().is_empty() {
            hit.url.as_str()
        } else {
            hit.title.trim()
        };
        text.push_str(&format!("{}. {title}\n   {}\n", i + 1, hit.url));
        let snippet = hit.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
        if !snippet.is_empty() {
            text.push_str(&format!("   {snippet}\n"));
        }
    }
    let mut notes = Vec::new();
    if hits.len() > max_results {
        notes.push(format!(
            "showing the first {max_results} of {} results",
            hits.len()
        ));
    }
    if let Some((end, _)) = text.char_indices().nth(cap_chars) {
        text.truncate(end);
        notes.push(format!(
            "the output was truncated at {cap_chars} characters"
        ));
    }
    let mut text = text.trim_end().to_string();
    if !notes.is_empty() {
        text.push_str(&format!("\n\n[{}]", notes.join("; ")));
    }
    text
}

fn status_error(backend: &str, status: StatusCode, json_hint: bool) -> ToolExecutionError {
    ToolExecutionError::other(match status.as_u16() {
        401 => format!("{backend} rejected the credentials (HTTP 401); check the API key"),
        403 if json_hint => format!(
            "{backend} answered HTTP 403; the instance probably does not have the `json` \
             format enabled (add `json` to `search.formats` in its settings.yml)"
        ),
        403 => format!("{backend} refused the request (HTTP 403); the key may lack access"),
        429 => format!("{backend} is rate limiting searches (HTTP 429); try again later"),
        _ => format!("{backend} answered with HTTP status {status}"),
    })
}

/// The last bytes of `text`, cut at a character boundary.
fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].trim()
}

impl Websearch {
    pub fn new(
        config: WebsearchConfig,
        network: &crate::tls::LoadedNetwork,
        child: Arc<SearxngChild>,
    ) -> Result<Self, reqwest::Error> {
        let client = crate::tls::build_client(network, || {
            reqwest::Client::builder()
                .user_agent(crate::FONTAINE_USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(config.timeout)
        })?;
        Ok(Websearch {
            client,
            config,
            child,
        })
    }

    pub fn config(&self) -> &WebsearchConfig {
        &self.config
    }

    async fn search(&self, query: &str) -> Result<Vec<Hit>, ToolExecutionError> {
        let backend = self.config.backend.name();
        match &self.config.backend {
            BackendConfig::Tavily { api_key, endpoint } => {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .bearer_auth(&api_key.0)
                    .json(&json!({ "query": query, "max_results": self.config.max_results }));
                parse_tavily(&self.send(request, backend, false).await?)
            }
            BackendConfig::Searxng { .. } | BackendConfig::Managed(_) => {
                let base_url = match &self.config.backend {
                    BackendConfig::Searxng {
                        base_url,
                        launch: None,
                    } => base_url.clone(),
                    BackendConfig::Searxng {
                        base_url,
                        launch: Some(launch),
                    } => {
                        self.ensure_started(launch.startup_timeout, || {
                            Ok(ManagedPlan {
                                argv: launch.argv.clone(),
                                base_url: base_url.clone(),
                                env: launch.env.clone(),
                            })
                        })
                        .await?
                    }
                    BackendConfig::Managed(managed) => {
                        self.ensure_started(managed.startup_timeout, || (managed.prepare)())
                            .await?
                    }
                    BackendConfig::Tavily { .. } => unreachable!("matched by the outer arm"),
                };
                let mut url = base_url;
                url.path_segments_mut()
                    .map_err(|()| {
                        ToolExecutionError::other("the SearXNG base_url cannot hold a path")
                    })?
                    .pop_if_empty()
                    .push("search");
                url.query_pairs_mut()
                    .append_pair("q", query)
                    .append_pair("format", "json");
                parse_searxng(&self.send(self.client.get(url), backend, true).await?)
            }
        }
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        backend: &str,
        json_hint: bool,
    ) -> Result<Vec<u8>, ToolExecutionError> {
        let response = request.send().await.map_err(|e| self.request_error(e))?;
        let status = response.status();
        if status.is_redirection() {
            return Err(ToolExecutionError::other(format!(
                "{backend} answered with a redirect ({status}). Redirects are not followed, since \
                 they would send the query to another host; check the configured address"
            )));
        }
        if !status.is_success() {
            return Err(status_error(backend, status, json_hint));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|e| self.request_error(e))
    }

    fn request_error(&self, error: reqwest::Error) -> ToolExecutionError {
        if error.is_timeout() {
            return ToolExecutionError::timeout(format!(
                "the search timed out after {} seconds",
                self.config.timeout.as_secs_f64().max(0.001)
            ));
        }
        ToolExecutionError::network(format!(
            "the search request failed: {}",
            chain(&error.without_url())
        ))
    }

    /// Starts the instance on the first search and waits until `base_url` answers. A process
    /// that is still alive is reused. The lock is held throughout, so searches that arrive
    /// meanwhile wait for the same start.
    async fn ensure_started(
        &self,
        startup_timeout: Duration,
        plan: impl FnOnce() -> std::io::Result<ManagedPlan>,
    ) -> Result<Url, ToolExecutionError> {
        let mut slot = self.child.slot.lock().await;
        if let Some((running, url)) = slot.as_mut() {
            if matches!(running.child().try_wait(), Ok(None)) {
                return Ok(url.clone());
            }
            *slot = None;
        }
        let ManagedPlan {
            argv,
            base_url,
            env,
        } = plan().map_err(|e| {
            ToolExecutionError::other(format!("cannot prepare SearXNG: {}", chain(&e)))
        })?;
        let base_url = &base_url;
        let (program, args) = argv
            .split_first()
            .expect("a launch command is validated as non-empty");
        let child = command(program, args, &env).spawn().map_err(|e| {
            ToolExecutionError::other(format!(
                "cannot start the SearXNG command `{program}`: {}",
                chain(&e)
            ))
        })?;
        // Guarded from the spawn on, so a dropped future or an early return still stops the
        // process group; `kill_on_drop` alone would only kill a `docker run` CLI, not its container.
        let mut running = Running::new(child, STOP_GRACE);
        let stderr = Arc::new(Mutex::new(String::new()));
        let drain = running.child().stderr.take().map(|pipe| {
            let tail_buffer = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Shown only inside a start-failure message, so a poisoned lock is harmless.
                    let mut buffer = lock_recovering(&tail_buffer);
                    buffer.push_str(&line);
                    buffer.push('\n');
                    if buffer.len() > 4 * STDERR_TAIL_BYTES {
                        let keep = tail(&buffer, STDERR_TAIL_BYTES).to_string();
                        *buffer = keep;
                    }
                }
            })
        });
        let started = Instant::now();
        loop {
            let exited = running.child().try_wait().map_err(|e| {
                ToolExecutionError::other(format!(
                    "cannot check the SearXNG process: {}",
                    chain(&e)
                ))
            })?;
            if let Some(status) = exited {
                if let Some(drain) = drain {
                    let _ = tokio::time::timeout(Duration::from_millis(500), drain).await;
                }
                return Err(start_failure(
                    format!(
                        "the SearXNG command `{program}` exited ({status}) before {base_url} answered"
                    ),
                    &stderr,
                ));
            }
            if self.probe(base_url).await {
                break;
            }
            if started.elapsed() >= startup_timeout {
                let pid = running.pid;
                stop(running.child(), pid, STOP_GRACE).await;
                if let Some(drain) = drain {
                    let _ = tokio::time::timeout(Duration::from_millis(500), drain).await;
                }
                return Err(start_failure(
                    format!(
                        "{base_url} did not answer within {} seconds of starting `{program}`",
                        startup_timeout.as_secs_f64()
                    ),
                    &stderr,
                ));
            }
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
        *slot = Some((running, base_url.clone()));
        Ok(base_url.clone())
    }

    /// Any non-5xx answer counts: the instance is up even if it has no page at its root, but a
    /// proxy's 502/503 while SearXNG is still booting is not.
    async fn probe(&self, base_url: &Url) -> bool {
        self.client
            .get(base_url.clone())
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .is_ok_and(|response| !response.status().is_server_error())
    }
}

fn start_failure(message: String, stderr: &Mutex<String>) -> ToolExecutionError {
    let buffer = lock_recovering(stderr);
    let output = tail(&buffer, STDERR_TAIL_BYTES);
    if output.is_empty() {
        ToolExecutionError::other(message)
    } else {
        ToolExecutionError::other(format!("{message}. Its stderr ended with:\n{output}"))
    }
}

impl Tool for Websearch {
    const NAME: &'static str = "websearch";
    type Args = WebsearchArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Search the web and return a numbered list of results, each with a title, a URL and a \
             snippet (at most {}). Takes a `query`, and optionally `allowed_domains` or \
             `blocked_domains` (not both) to keep or drop results by site, subdomains included. \
             Use it for current information the conversation does not have; fetch a result's page \
             with `webfetch` for more than its snippet. The query is sent to the configured \
             search backend ({}). Times out after {} seconds.",
            self.config.max_results,
            self.config.backend.name(),
            self.config.timeout.as_secs()
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query." },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only return results from these domains.",
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Never return results from these domains.",
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        })
    }

    fn annotations(&self) -> rig_core::completion::ToolAnnotations {
        rig_core::completion::ToolAnnotations::default().with_title("Web search")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: WebsearchArgs,
    ) -> Result<String, ToolExecutionError> {
        if args.query.trim().is_empty() {
            return Err(ToolExecutionError::invalid_args(
                "`query` must not be empty",
            ));
        }
        let allowed = args.allowed_domains.unwrap_or_default();
        let blocked = args.blocked_domains.unwrap_or_default();
        if !allowed.is_empty() && !blocked.is_empty() {
            return Err(ToolExecutionError::invalid_args(
                "give `allowed_domains` or `blocked_domains`, not both",
            ));
        }
        let hits = self.search(&args.query).await?;
        let hits = apply_domain_filters(hits, &allowed, &blocked);
        Ok(format_hits(
            &hits,
            self.config.max_results,
            self.config.output_cap_chars,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn spawn_script(script: &str) -> Child {
        let env = vec![(OsString::from("PATH"), std::env::var_os("PATH").unwrap())];
        command("sh", &["-c".to_string(), script.to_string()], &env)
            .spawn()
            .unwrap()
    }

    /// Waits until the script has installed its trap, so the signal is not sent before it.
    #[cfg(unix)]
    async fn until_ready(marker: &std::path::Path) {
        for _ in 0..100 {
            if marker.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the fake child never started");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let script = format!(
            "trap '' TERM; echo x > {}; while :; do sleep 0.05; done",
            ready.display()
        );
        let mut child = spawn_script(&script);
        let pid = child.id();
        until_ready(&ready).await;
        let started = Instant::now();
        stop(&mut child, pid, Duration::from_millis(300)).await;
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(matches!(child.try_wait(), Ok(Some(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_returns_at_once_and_the_group_is_killed_after_the_grace() {
        let dir = tempfile::tempdir().unwrap();
        let (ready, pidfile) = (dir.path().join("ready"), dir.path().join("pid"));
        // The inner shell is the grandchild a wrapper would leave behind.
        let script = format!(
            "sh -c 'trap \"\" TERM; echo $$ > {}; while :; do sleep 0.05; done' & trap '' TERM; echo x > {}; wait",
            pidfile.display(),
            ready.display()
        );
        let running = Running::new(spawn_script(&script), Duration::from_millis(600));
        until_ready(&ready).await;
        until_ready(&pidfile).await;
        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // A killed process nobody reaps lingers as a zombie, and `/proc` is Linux-only.
        let alive = || {
            let out = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &grandchild.to_string()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&out.stdout);
            let state = state.trim();
            !state.is_empty() && !state.starts_with('Z')
        };
        let started = Instant::now();
        drop(running);
        assert!(started.elapsed() < Duration::from_secs(5), "drop blocked");
        assert!(alive(), "SIGTERM is ignored, so it lives through the grace");
        for _ in 0..100 {
            if !alive() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the process group was not killed after the grace");
    }

    #[test]
    fn a_domain_matches_itself_and_its_subdomains_only() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(domain_matches("docs.example.com", "Example.com"));
        assert!(domain_matches("docs.example.com", "*.example.com"));
        assert!(!domain_matches("notexample.com", "example.com"));
        assert!(!domain_matches("example.com", "docs.example.com"));
        assert!(!domain_matches("example.com", ""));
    }

    #[test]
    fn the_key_is_redacted_in_debug_output() {
        let config = WebsearchConfig::new(BackendConfig::tavily(ApiKey::new("tvly-secret")));
        assert!(!format!("{config:?}").contains("tvly-secret"));
    }
}
