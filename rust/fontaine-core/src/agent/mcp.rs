//! Starting the MCP servers a session is configured with, and the approval repository-defined
//! ones need first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::{CallView, GateDecision, ToolGate};
use fontaine_tools::guarded::Guard;
use fontaine_tools::lock::write_recovering;
use fontaine_tools::mcp::output::{DEFAULT_MAX_TOKENS, OutputLimits, Spill};
use fontaine_tools::mcp::{
    ElicitForm, ElicitReply, ElicitUrl, Elicitor, McpOptions, PromptSummary, ServerSpec, Transport,
    connect as connect_server, mcp_tool_name,
};
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::tls::LoadedNetwork;
use futures::future::BoxFuture;
use rig_core::tool::{DynamicTool, ManagedToolSink, ManagedToolToken};
use rmcp::model::{ContentBlock, GetPromptRequestParams, GetPromptResponse, PromptMessage, Role};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::mcp_supervisor::{
    MCP_DRAIN_BOUND, McpServerState, ReconnectTarget, ServerConnector, ServerSlot, Supervisor,
};
use super::{AgentRuntime, EnvLookup};
use crate::config::{Config, McpServer, Origin, Slot};
use crate::gate::SessionState;
use crate::mcp_auth::McpAuth;
use crate::mcp_json::{StartContext, expand_at_start};
use crate::permissions::{Approver, ElicitationEvent, ElicitationHook, run_elicitation_hooks};
use crate::runtime_error::RuntimeError;
use crate::tools;
use crate::trust;
use crate::user_hooks::LivePlugins;

/// An MCP server the session may start, and whether the user has to say so first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerPlan {
    pub name: String,
    /// The command line or URL, as the approval prompt shows it.
    pub description: String,
    /// The repository defines it.
    pub project: bool,
    pub needs_approval: bool,
    /// Identifies this definition in the approval record.
    pub digest: String,
}

/// What starting an MCP server came to.
#[derive(Debug)]
pub struct McpOutcome {
    pub server: String,
    pub error: Option<String>,
}

/// An MCP resource the `@` completion can offer, with the server it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpResourceSummary {
    pub server: String,
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
}

/// Rebuilds the `@` mention resource list from what `slots` list now and bumps the listing
/// revision, so a surface caching MCP prompt rows never sees refreshed listings without the
/// revision moving. The completion list is advisory: a stale list only misses a suggestion.
fn refresh_mentions(
    revision: &AtomicU64,
    mentions: &Arc<RwLock<Arc<Vec<McpResourceSummary>>>>,
    slots: &Arc<RwLock<BTreeMap<String, Arc<ServerSlot>>>>,
) {
    revision.fetch_add(1, Ordering::Relaxed);
    *write_recovering(mentions) = Arc::new(super::mcp_supervisor::mention_snapshot(slots));
}

/// Flattens a `prompts/get` answer into one user turn: only when every message is user-role
/// text, joined in order with a blank line. Anything else refuses the command rather than
/// dropping a block or folding assistant text into the user's turn.
pub(crate) fn flatten_prompt_messages(messages: &[PromptMessage]) -> Result<String, String> {
    let mut texts = Vec::new();
    for message in messages {
        match (&message.role, &message.content) {
            (Role::User, ContentBlock::Text(text)) => texts.push(text.text.clone()),
            (role, _) => {
                return Err(format!(
                    "the server's answer carried a {role:?} message or non-text content; \
                     only user-role text can become your turn, so the command did nothing"
                ));
            }
        }
    }
    Ok(texts.join("\n\n"))
}

/// Parses `key=value` pairs out of a prompt command's argument and checks them against what
/// the prompt declares. `Err` names the declared arguments and an example invocation.
pub(crate) fn parse_prompt_arguments(
    name: &str,
    argument: &str,
    prompt: &PromptSummary,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    fn usage(name: &str, prompt: &PromptSummary, why: String) -> String {
        let declared: Vec<String> = prompt
            .arguments
            .iter()
            .map(|argument| {
                if argument.required {
                    format!("{} (required)", argument.name)
                } else {
                    argument.name.clone()
                }
            })
            .collect();
        let example: Vec<String> = prompt
            .arguments
            .iter()
            .map(|argument| format!("{}=<value>", argument.name))
            .collect();
        let spacing = if example.is_empty() { "" } else { " " };
        let declared = if declared.is_empty() {
            "no arguments".to_string()
        } else {
            declared.join(", ")
        };
        format!(
            "{why}; it declares: {declared}. For example: `/{name}{spacing}{}`",
            example.join(" ")
        )
    }
    let declared: std::collections::BTreeMap<&str, bool> = prompt
        .arguments
        .iter()
        .map(|argument| (argument.name.as_str(), argument.required))
        .collect();
    let mut given = std::collections::BTreeMap::new();
    for pair in argument.split_whitespace() {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(usage(
                name,
                prompt,
                format!("`{pair}` is not a `key=value` pair"),
            ));
        };
        if !declared.contains_key(key) {
            return Err(usage(
                name,
                prompt,
                format!("`{key}` is not an argument it declares"),
            ));
        }
        given.insert(key.to_string(), value.to_string());
    }
    let missing: Vec<&str> = declared
        .iter()
        .filter(|(_, required)| **required)
        .map(|(name, _)| *name)
        .filter(|name| !given.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        return Err(usage(
            name,
            prompt,
            format!("missing: {}", missing.join(", ")),
        ));
    }
    Ok(given)
}

pub fn server_digest(name: &str, server: &McpServer) -> String {
    let mut canonical = json!({
        "name": name,
        "command": server.command,
        "args": server.args,
        "env_vars": server.env_vars,
        "env": server.env,
        "url": server.url,
    });
    // Only when set, so digests of definitions that never expand stay as approved; an expanding
    // definition needs its own approval because `${VAR}` values reach the server. Headers alter
    // what is sent to the remote just as much, so they join the digest the same way.
    if server.expand {
        canonical["expand"] = json!(true);
    }
    if let Some(headers) = &server.headers {
        canonical["headers"] = json!(headers);
    }
    if let Some(helper) = &server.headers_helper {
        canonical["headers_helper"] = json!(helper);
    }
    let bytes =
        serde_jcs::to_vec(&canonical).unwrap_or_else(|_| canonical.to_string().into_bytes());
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The transport's literal env map: `[env]` entries with the server's own `.mcp.json` `env`
/// laid over them.
pub(crate) fn merged_server_env(
    base: Vec<(OsString, OsString)>,
    extra: &Option<BTreeMap<String, String>>,
) -> Vec<(OsString, OsString)> {
    let mut out = base;
    for (name, value) in extra.iter().flatten() {
        match out.iter_mut().find(|(key, _)| env_key_matches(key, name)) {
            Some(slot) => slot.1 = value.clone().into(),
            None => out.push((name.clone().into(), value.clone().into())),
        }
    }
    out
}

/// Environment variable names are case-insensitive on Windows and case-sensitive elsewhere.
fn env_key_matches(key: &OsString, name: &str) -> bool {
    if cfg!(windows) {
        key.to_string_lossy().eq_ignore_ascii_case(name)
    } else {
        key.as_os_str() == name
    }
}

fn server_description(server: &McpServer) -> String {
    let mut variables: Vec<String> = server.env_vars.clone().unwrap_or_default();
    if server.expand {
        let texts = server
            .command
            .iter()
            .chain(server.args.iter().flatten())
            .chain(server.env.iter().flat_map(|env| env.values()))
            .chain(server.url.iter())
            .chain(server.headers.iter().flat_map(|headers| headers.values()));
        for text in texts {
            for name in crate::mcp_json::references(text) {
                // Taken from the session root, never read from the environment.
                if name != "CLAUDE_PROJECT_DIR" && !variables.contains(&name) {
                    variables.push(name);
                }
            }
        }
    }
    let mut line = server.command_line();
    if !variables.is_empty() && !line.is_empty() {
        line.push_str(&format!("\nenvironment: {}", variables.join(", ")));
    }
    if let Some(helper) = &server.headers_helper {
        line.push_str(&format!("\nheaders helper: {helper}"));
    }
    line
}

/// Reads the state of each MCP server, for a task that does not own the runtime.
pub type McpStatesReader = Arc<dyn Fn() -> Vec<(String, McpServerState)> + Send + Sync>;

/// The MCP servers a session at `root` would start, and which of them need the user's say-so.
/// `project_executables` is whether the repository's own servers may load at all, and `confirm`
/// whether the repository's servers wait for the user's approval.
pub fn mcp_plan(
    config: &Config,
    root: &Path,
    data_dir: &Path,
    project_executables: bool,
    confirm: bool,
) -> Vec<McpServerPlan> {
    config
        .mcp_servers
        .iter()
        .filter_map(|(name, server)| {
            let origin = config.mcp_origins.get(name).copied().unwrap_or_default();
            let project = origin.is_project();
            if project && !project_executables {
                return None;
            }
            let digest = server_digest(name, server);
            // A repository's server needs the user's approval; one in the user's own
            // settings, or in an untracked local file of a trusted root, does not.
            let repository_defined =
                origin.slot == crate::config::Slot::Project || (project && !origin.user_trust);
            let needs_approval = repository_defined
                && confirm
                && !trust::server_approved(data_dir, root, name, &digest);
            Some(McpServerPlan {
                name: name.clone(),
                description: server_description(server),
                project,
                needs_approval,
                digest,
            })
        })
        .collect()
}

/// Whether `name` holds a credential Fontaine knows of, which a repository's definition never
/// expands: what the tool subprocesses are scrubbed of, the web-search key, and the provider
/// variables the Claude CLI refuses.
fn credential_check<'a>(
    config: &'a Config,
    scrubbed: &'a [OsString],
) -> impl Fn(&str) -> bool + 'a {
    move |name: &str| {
        let matches = |other: &str| {
            if cfg!(windows) {
                other.eq_ignore_ascii_case(name)
            } else {
                other == name
            }
        };
        scrubbed.iter().any(|n| matches(&n.to_string_lossy()))
            || config.websearch.api_key_env.as_deref().is_some_and(matches)
            || crate::providers::claude_subscription::CONFLICTING_ENV
                .iter()
                .any(|n| matches(n))
    }
}

/// A repository's definition with its `${VAR}` references expanded.
fn expanded(
    server: &McpServer,
    root: &Path,
    env: &EnvLookup,
    is_credential: &dyn Fn(&str) -> bool,
) -> Result<McpServer, String> {
    let env: &dyn Fn(&str) -> Option<OsString> = &|name| env(name);
    let cx = StartContext {
        project_dir: root,
        env,
        is_credential,
    };
    let one = |text: &String| expand_at_start(text, &cx);
    let mut out = server.clone();
    out.command = server.command.as_ref().map(one).transpose()?;
    out.args = server
        .args
        .as_ref()
        .map(|args| args.iter().map(one).collect::<Result<Vec<_>, _>>())
        .transpose()?;
    out.env = server
        .env
        .as_ref()
        .map(|env| {
            env.iter()
                .map(|(k, v)| Ok((k.clone(), one(v)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()
        })
        .transpose()?;
    out.url = server.url.as_ref().map(one).transpose()?;
    out.headers = server
        .headers
        .as_ref()
        .map(|headers| {
            headers
                .iter()
                .map(|(k, v)| Ok((k.clone(), one(v)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()
        })
        .transpose()?;
    Ok(out)
}

/// Why a `headers_helper` cannot run for this server, if it cannot. The helper is an executable
/// the declaration names, so it runs only for scopes the user controls: refused for the project
/// scope or a tracked project-local file, where a repository Fontaine merely opened could
/// otherwise make Fontaine run a script.
pub fn headers_helper_refusal(name: &str, server: &McpServer, origin: Origin) -> Option<String> {
    let repository_defined =
        origin.slot == Slot::Project || (origin.is_project() && !origin.user_trust);
    if server.headers_helper.is_some() && repository_defined {
        Some(format!(
            "MCP server `{name}` sets `headers_helper` in the {} scope, which Fontaine refuses to run: a repository must not make Fontaine execute a script; move the server to a user-trust scope or drop `headers_helper`",
            origin.slot.label()
        ))
    } else {
        None
    }
}

/// The configured server `name` as it will be started: the `headers_helper` refusal applied and,
/// for a definition that expands, its `${VAR}` references resolved. `Ok(None)` when unknown.
pub fn resolved_server(
    config: &Config,
    name: &str,
    root: &Path,
    env: &EnvLookup,
    scrubbed: &[OsString],
) -> Result<Option<McpServer>, String> {
    let Some(defined) = config.mcp_servers.get(name) else {
        return Ok(None);
    };
    let origin = config.mcp_origins.get(name).copied().unwrap_or_default();
    if let Some(refusal) = headers_helper_refusal(name, defined, origin) {
        return Err(refusal);
    }
    if defined.expand {
        let is_credential = credential_check(config, scrubbed);
        expanded(defined, root, env, &is_credential)
            .map(Some)
            .map_err(|e| format!("MCP server `{name}`: {e}"))
    } else {
        Ok(Some(defined.clone()))
    }
}

/// How to start the configured server `name` with its working directory at `root`; `Ok(None)`
/// when it is unknown or names neither a command nor a URL. A repository's definition has its
/// `${VAR}` references expanded here, through `env`; `scrubbed` are the credential variables it
/// may not expand. A reference that cannot be expanded is an error naming it.
pub fn server_spec(
    config: &Config,
    name: &str,
    root: &Path,
    data_dir: &Path,
    env: &EnvLookup,
    scrubbed: &[OsString],
) -> Result<Option<ServerSpec>, String> {
    let Some(server) = resolved_server(config, name, root, env, scrubbed)? else {
        return Ok(None);
    };
    let transport = match (&server.command, &server.url) {
        (Some(command), _) => Transport::Stdio {
            command: command.clone(),
            args: server.args.clone().unwrap_or_default(),
            env_vars: server.env_vars.clone().unwrap_or_default(),
            env: merged_server_env(tools::subprocess_env(config), &server.env),
            cwd: root.to_path_buf(),
        },
        (None, Some(url)) => {
            // Stored tokens are bound to the URL they were issued for; a declaration that does
            // not match fails closed rather than receive a token issued to another server.
            match crate::mcp_auth::stored_resource(data_dir, name) {
                Ok(Some(resource)) if resource != *url => {
                    return Err(format!(
                        "MCP server `{name}` is declared at `{url}` but its stored credentials were issued for `{resource}`; run `fontaine mcp login {name}` for this URL"
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(format!(
                        "MCP server `{name}`: cannot read the stored credentials: {}",
                        chain(&e)
                    ));
                }
            }
            Transport::Http {
                url: url.clone(),
                headers: server.headers.clone().unwrap_or_default(),
                headers_helper: server.headers_helper.clone(),
                credentials: Arc::new(crate::mcp_auth::FileCredentialStore::new(
                    data_dir, name, url,
                )),
                states: Arc::new(crate::mcp_auth::FileStateStore::new(data_dir, name)),
            }
        }
        (None, None) => return Ok(None),
    };
    Ok(Some(ServerSpec {
        name: name.to_string(),
        transport,
    }))
}

/// A gate that refuses every call, for a connection made only to see what a server offers.
struct DenyEveryCall;

impl ToolGate for DenyEveryCall {
    fn check<'a>(&'a self, _call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        Box::pin(async {
            GateDecision::Deny {
                message: "a probe never calls tools".to_string(),
            }
        })
    }
}

/// Counts the tools a server registers instead of keeping them.
#[derive(Clone, Default)]
struct CountingSink(Arc<AtomicUsize>);

impl ManagedToolSink for CountingSink {
    fn add_managed_tools(&self, tools: Vec<DynamicTool>) -> HashMap<String, ManagedToolToken> {
        self.0.store(tools.len(), Ordering::Relaxed);
        HashMap::new()
    }

    fn reconcile_managed_tools(
        &self,
        _expected: HashMap<String, ManagedToolToken>,
        tools: Vec<DynamicTool>,
    ) -> HashMap<String, ManagedToolToken> {
        self.0.store(tools.len(), Ordering::Relaxed);
        HashMap::new()
    }
}

/// Shows an MCP server's request for input through the session's approver, so the terminal UI's
/// dialog answers it and every other surface cancels.
pub(crate) struct ApproverElicitor(pub(crate) Arc<dyn Approver>);

impl Elicitor for ApproverElicitor {
    fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply> {
        self.0.elicit(form)
    }

    fn elicit_url<'a>(&'a self, request: ElicitUrl) -> BoxFuture<'a, ElicitReply> {
        self.0.elicit_url(request)
    }
}

/// Lets `elicitation` hooks answer an MCP server's request for input before any dialog. A
/// request nobody answers goes on to the approver, so a session with no hook behaves as before.
pub struct HookElicitor {
    hooks: Vec<Arc<dyn ElicitationHook>>,
    plugin_hooks: LivePlugins,
    inner: ApproverElicitor,
    approver: Arc<dyn Approver>,
    state: Arc<SessionState>,
}

impl HookElicitor {
    pub fn new(
        hooks: Vec<Arc<dyn ElicitationHook>>,
        plugin_hooks: LivePlugins,
        approver: Arc<dyn Approver>,
        state: Arc<SessionState>,
    ) -> HookElicitor {
        HookElicitor {
            hooks,
            plugin_hooks,
            inner: ApproverElicitor(approver.clone()),
            approver,
            state,
        }
    }

    async fn ask_hooks(
        &self,
        server: &str,
        id: &str,
        message: &str,
        url: Option<&str>,
        schema: Option<&serde_json::Value>,
    ) -> Option<ElicitReply> {
        let layer = self.plugin_hooks.get();
        if self.hooks.is_empty() && layer.elicitation.is_empty() {
            return None;
        }
        let scope = self.state.scope();
        let roots: Vec<String> = scope
            .roots()
            .iter()
            .map(|root| root.display().to_string())
            .collect();
        let mode = self.state.mode().rules();
        run_elicitation_hooks(
            &self.hooks,
            &self.plugin_hooks,
            ElicitationEvent {
                server,
                mode: if url.is_some() { "url" } else { "form" },
                message,
                url,
                elicitation_id: id,
                requested_schema: schema,
                session_id: &self.state.session_id,
                cwd: scope.primary(),
                permission_mode: mode.as_str(),
                workspace_roots: &roots,
            },
            &*self.approver,
        )
        .await
    }
}

impl Elicitor for HookElicitor {
    fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply> {
        Box::pin(async move {
            let schema = serde_json::to_value(&form.schema).ok();
            match self
                .ask_hooks(&form.server, &form.id, &form.message, None, schema.as_ref())
                .await
            {
                Some(reply) => reply,
                None => self.inner.elicit(form).await,
            }
        })
    }

    fn elicit_url<'a>(&'a self, request: ElicitUrl) -> BoxFuture<'a, ElicitReply> {
        Box::pin(async move {
            match self
                .ask_hooks(
                    &request.server,
                    &request.id,
                    &request.message,
                    Some(&request.url),
                    None,
                )
                .await
            {
                Some(reply @ (ElicitReply::Decline | ElicitReply::Cancel)) => reply,
                // A link opens only after the person confirms, so a hook never accepts one.
                Some(ElicitReply::Accept(_)) => {
                    self.approver.warn(
                        "an elicitation hook accepted a link request; links open only after you confirm, so the request goes to the dialog",
                    );
                    self.inner.elicit_url(request).await
                }
                None => self.inner.elicit_url(request).await,
            }
        })
    }
}

/// Why `name`, which has no slot, was never started. A configured server the plan leaves out is
/// the repository's, withheld because the workspace is not trusted; one the plan holds back for
/// approval was left off.
fn not_attempted(name: &str, configured: bool, plan: &[McpServerPlan]) -> String {
    if configured && !plan.iter().any(|p| p.name == name) {
        format!(
            "MCP server `{name}` is defined by the repository and the workspace is not trusted, so it is not started"
        )
    } else if plan.iter().any(|p| p.name == name && p.needs_approval) {
        format!("MCP server `{name}` was left off at startup; restart Fontaine to approve it")
    } else {
        format!("no MCP server named `{name}` was started this session")
    }
}

const MAX_OUTPUT_TOKENS_VAR: &str = "MAX_MCP_OUTPUT_TOKENS";

/// The output limits a session applies to MCP results: the token limit from the environment and
/// a spill directory in the session's scratch root. An unusable value is warned about and the
/// default used.
pub(crate) fn output_limits(
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
    scratch: Option<&Path>,
    warn: &dyn Fn(&str),
) -> OutputLimits {
    let max_tokens = match env(MAX_OUTPUT_TOKENS_VAR) {
        None => DEFAULT_MAX_TOKENS,
        Some(value) => match value
            .to_str()
            .and_then(|text| text.trim().parse::<usize>().ok())
            .filter(|tokens| *tokens > 0)
        {
            Some(tokens) => tokens,
            None => {
                warn(&format!(
                    "{MAX_OUTPUT_TOKENS_VAR} is not a positive whole number ({}); using {DEFAULT_MAX_TOKENS}",
                    value.to_string_lossy()
                ));
                DEFAULT_MAX_TOKENS
            }
        },
    };
    OutputLimits::new(
        max_tokens,
        scratch.map(|dir| Spill(dir.join("mcp-results"))),
    )
}

/// How long a probe waits for a server to start and list its tools.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts the server `spec` describes, counts the tools it registers, and closes the
/// connection. The count is of tools a session would register: names the guard cannot use are
/// already dropped.
pub async fn probe_server(
    spec: &ServerSpec,
    network: &LoadedNetwork,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<usize, String> {
    let sink = CountingSink::default();
    let guard = Guard {
        gate: Arc::new(DenyEveryCall),
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: true,
        pre_images: None,
        kills: None,
    };
    // One attempt, no retry; the default options cancel any request for input and keep nothing.
    let options = McpOptions::new(guard, false);
    let attempt = async {
        let connected = connect_server(spec, sink.clone(), &options, network, env)
            .await
            .map_err(|e| chain(&e))?;
        let count = sink.0.load(Ordering::Relaxed);
        // Waits for the shutdown, so a server process is gone before doctor exits.
        let _ = connected.connection.close().await;
        Ok(count)
    };
    match tokio::time::timeout(PROBE_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "no answer within {} seconds",
            PROBE_TIMEOUT.as_secs()
        )),
    }
}

impl AgentRuntime {
    /// The MCP servers this session would start, and which of them need the user's say-so.
    pub fn mcp_plan(&self) -> Vec<McpServerPlan> {
        mcp_plan(
            &self.config,
            &self.primary_root(),
            &self.setup.data_dir,
            self.setup.project_executables,
            self.setup.confirm_project_servers,
        )
    }

    /// The supervisor that starts and watches this session's MCP servers, made on first use.
    fn mcp_supervisor(&mut self) -> Arc<Supervisor> {
        if let Some(supervisor) = &self.mcp.supervisor {
            return supervisor.clone();
        }
        let telemetry = &self.config.telemetry;
        let reveal = telemetry.enabled && telemetry.log_tool_details;
        let guard = Guard {
            gate: self.pipeline.clone(),
            results: self.results.clone(),
            hide_mcp_names: !reveal,
            pre_images: None,
            // MCP tools join outside the registry, so the session's switches reach them here.
            kills: Some(self.kills.clone()),
        };
        let env = self.setup.env.clone();
        let approver = self.approver.clone();
        let limits = {
            let env = env.clone();
            let approver = approver.clone();
            output_limits(
                &move |name| env(name),
                self.state.scratch.as_deref(),
                &move |m| approver.warn(m),
            )
        };
        let mut options = McpOptions::new(guard, reveal);
        options.deferral = self.mcp.deferral.clone();
        options.limits = Arc::new(limits);
        options.elicitor = Arc::new(HookElicitor::new(
            self.setup.elicitation_hooks.clone(),
            self.setup.plugin_hooks.clone(),
            approver.clone(),
            self.state.clone(),
        ));
        let warner = approver.clone();
        options.warn = Arc::new(move |message| warner.warn(message));
        let connector = ServerConnector {
            options,
            tool_server: self.tool_server.clone(),
            network: self.setup.network.clone(),
            env,
        };
        let state = self.state.clone();
        let types = self.agent_types.clone();
        let slots = self.mcp.slots_handle();
        let mentions = self.mcp_mentions.clone();
        let revision = self.mcp_listing_revision.clone();
        let supervisor = Supervisor::new(
            Arc::new(connector),
            approver,
            // Watchers run on tokio worker threads, concurrent with draws that sample the
            // revision: the bump and the snapshot ride one helper so no path refreshes the
            // listings without moving the revision cached prompt rows key on.
            Arc::new(move || {
                state.agents().refresh_rebuilt_toolsets(&types.get());
                refresh_mentions(&revision, &mentions, &slots);
            }),
        );
        self.mcp.supervisor = Some(supervisor.clone());
        supervisor
    }

    /// Rebuilds the `@` mention resource list from what the connected servers list now. Every
    /// listing change (connect, disconnect, reconnect, a `list_changed` refresh) rides through
    /// here, so the bump doubles as the listing revision cached prompt rows key on.
    pub(crate) fn refresh_mcp_mentions(&self) {
        refresh_mentions(
            &self.mcp_listing_revision,
            &self.mcp_mentions,
            &self.mcp.slots_handle(),
        );
    }

    /// The MCP listing revision: bumped on every connect, disconnect, reconnect and
    /// `list_changed` refresh. A caller caching `slash_prompts` rows refreshes them only when
    /// this moves.
    pub fn slash_prompts_revision(&self) -> u64 {
        self.mcp_listing_revision.load(Ordering::Relaxed)
    }

    /// Starts the one configured server `name` that the plan lets start without asking, which is
    /// every plugin server: a plugin's install is its approval. The server's old slot, if any,
    /// is retired first. A failure is the outcome's `error` and leaves a `Failed` slot, as at
    /// startup.
    pub async fn start_mcp_server(&mut self, name: &str) -> McpOutcome {
        let outcome = |error: Option<String>| McpOutcome {
            server: name.to_string(),
            error,
        };
        let planned = self
            .mcp_plan()
            .into_iter()
            .any(|plan| plan.name == name && !plan.needs_approval);
        if !planned {
            return outcome(Some(format!(
                "MCP server `{name}` is not one this session starts without approval"
            )));
        }
        let spec = match server_spec(
            &self.config,
            name,
            &self.primary_root(),
            &self.setup.data_dir,
            &self.setup.env,
            &self.setup.scrub_env,
        ) {
            Ok(Some(spec)) => spec,
            Ok(None) => return outcome(None),
            Err(error) => return outcome(Some(error)),
        };
        let supervisor = self.mcp_supervisor();
        let (slot, result) = supervisor.start(spec).await;
        self.mcp.insert(slot);
        self.refresh_mcp_mentions();
        self.state
            .agents()
            .refresh_rebuilt_toolsets(&self.agent_types.get());
        outcome(result.err().map(|e| chain(&e)))
    }

    /// Unloads the MCP server `name`: no new call reaches it, calls in flight get
    /// a ten-second bound to finish, then its connection closes. `None` when the session never
    /// attempted it, else whether every call finished in time.
    pub async fn stop_mcp_server(&mut self, name: &str) -> Option<bool> {
        let removed = self
            .mcp
            .remove(name, &self.tool_server, MCP_DRAIN_BOUND)
            .await;
        if removed.is_some() {
            self.refresh_mcp_mentions();
        }
        removed
    }

    /// Starts the MCP servers the plan allows: those that need no approval and those named in
    /// `approved`, all at once. A server that fails to start is reported and skipped, and keeps a
    /// slot so `/mcp reconnect` can try it again; the session goes on.
    pub async fn connect_mcp(&mut self, approved: &[String]) -> Vec<McpOutcome> {
        let supervisor = self.mcp_supervisor();
        let approver = self.approver.clone();

        let (attempts, mut outcomes) = self.specs_to_start(approved);
        for outcome in &outcomes {
            if let Some(error) = &outcome.error {
                approver.warn(error);
            }
        }
        let started = futures::future::join_all(attempts.into_iter().map(|(name, spec)| {
            let supervisor = supervisor.clone();
            async move { (name, supervisor.start(spec).await) }
        }))
        .await;
        for (name, (slot, result)) in started {
            self.mcp.insert(slot);
            let error = result.err().map(|e| chain(&e));
            if let Some(rendered) = &error {
                approver.warn(rendered);
            }
            outcomes.push(McpOutcome {
                server: name,
                error,
            });
        }
        self.refresh_mcp_mentions();
        // The rebuild at startup derived each rebuilt agent's toolset before any server
        // connected; now that the tool server holds every MCP tool, the handle-less rebuilt
        // toolsets are re-derived, parents before children, so a later revival carries them.
        self.state
            .agents()
            .refresh_rebuilt_toolsets(&self.agent_types.get());
        outcomes
    }

    /// The specs of the servers to start now, and an outcome for each whose definition could not
    /// be built, which gets no slot.
    fn specs_to_start(&self, approved: &[String]) -> (Vec<(String, ServerSpec)>, Vec<McpOutcome>) {
        let root = self.primary_root();
        let mut attempts = Vec::new();
        let mut failed = Vec::new();
        for plan in self
            .mcp_plan()
            .into_iter()
            .filter(|plan| !plan.needs_approval || approved.contains(&plan.name))
        {
            match server_spec(
                &self.config,
                &plan.name,
                &root,
                &self.setup.data_dir,
                &self.setup.env,
                &self.setup.scrub_env,
            ) {
                Ok(Some(spec)) => attempts.push((plan.name, spec)),
                Ok(None) => {}
                Err(error) => failed.push(McpOutcome {
                    server: plan.name,
                    error: Some(error),
                }),
            }
        }
        if let Some(host) = &self.setup.host_mcp {
            attempts.extend(host.servers.iter().map(|name| {
                let spec = ServerSpec {
                    name: name.clone(),
                    transport: Transport::Sdk {
                        factory: host.factory.clone(),
                    },
                };
                (name.clone(), spec)
            }));
        }
        (attempts, failed)
    }

    /// The state of each MCP server the session attempted, by name.
    pub fn mcp_states(&self) -> Vec<(String, McpServerState)> {
        self.mcp.states()
    }

    /// [`mcp_states`](Self::mcp_states) as a handle another task can read while a turn runs.
    pub fn mcp_states_reader(&self) -> McpStatesReader {
        let slots = self.mcp.slots_handle();
        Arc::new(move || {
            fontaine_tools::lock::read(&slots, "MCP server slots")
                .iter()
                .map(|(name, slot)| (name.clone(), slot.state()))
                .collect()
        })
    }

    /// The auth status of each configured server, for the `/mcp` listing's auth column: `n/a`
    /// for a stdio server, `ok` when usable credentials are stored, else `required`.
    pub fn mcp_auth(&self) -> Vec<(String, McpAuth)> {
        let root = self.primary_root();
        self.config
            .mcp_servers
            .iter()
            .map(|(name, server)| {
                let auth = if server.url.is_none() {
                    McpAuth::Na
                } else if crate::mcp_auth::has_credentials(&self.setup.data_dir, name) {
                    // Credentials issued for another URL are refused at connect time, so they
                    // are not a usable login for this declaration.
                    let url = resolved_server(
                        &self.config,
                        name,
                        &root,
                        &self.setup.env,
                        &self.setup.scrub_env,
                    )
                    .ok()
                    .flatten()
                    .and_then(|s| s.url);
                    match crate::mcp_auth::stored_resource(&self.setup.data_dir, name) {
                        Ok(Some(resource)) if url.as_deref().is_some_and(|u| u != resource) => {
                            McpAuth::Required
                        }
                        Ok(_) => McpAuth::Ok,
                        Err(e) => {
                            self.approver.warn(&format!(
                                "MCP server `{name}`: cannot read the stored credentials: {}",
                                chain(&e)
                            ));
                            McpAuth::Required
                        }
                    }
                } else {
                    McpAuth::Required
                };
                (name.clone(), auth)
            })
            .collect()
    }

    /// Restarts MCP servers by hand, with no backoff. Only a server the session attempted has a
    /// slot, so this never starts one that was left off or that workspace trust withholds.
    pub async fn reconnect_mcp(&self, target: ReconnectTarget) -> Vec<McpOutcome> {
        let slots: Vec<Arc<ServerSlot>> = match &target {
            ReconnectTarget::One(name) => match self.mcp.slot(name) {
                Some(slot) => vec![slot],
                None => {
                    return vec![McpOutcome {
                        server: name.clone(),
                        error: Some(self.not_attempted(name)),
                    }];
                }
            },
            ReconnectTarget::AllFailed => self.mcp.failed(),
        };
        let Some(supervisor) = self.mcp.supervisor.clone() else {
            return Vec::new();
        };
        let outcomes = futures::future::join_all(slots.into_iter().map(|slot| {
            let supervisor = supervisor.clone();
            async move {
                McpOutcome {
                    server: slot.name().to_string(),
                    error: supervisor.reconnect(&slot).await.err().map(|e| chain(&e)),
                }
            }
        }))
        .await;
        self.refresh_mcp_mentions();
        outcomes
    }

    /// The MCP prompts a user can run as `/mcp__<server>__<prompt>`: the full command name,
    /// a hint of the declared argument names, and the server-supplied description. Two prompts
    /// whose names clean to the same command collide; the first stays listed and the rest are
    /// left out, since dispatch refuses the ambiguity.
    pub fn slash_prompts(&self) -> Vec<(String, Option<String>, String)> {
        let mut rows = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (server, listings) in self.mcp.listings() {
            for prompt in listings.prompts {
                let name = mcp_tool_name(&server, &prompt.name);
                if !seen.insert(name.clone()) {
                    continue;
                }
                let hint = (!prompt.arguments.is_empty()).then(|| {
                    prompt
                        .arguments
                        .iter()
                        .map(|argument| {
                            if argument.required {
                                format!("{}=<value>", argument.name)
                            } else {
                                format!("[{}=<value>]", argument.name)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                rows.push((
                    name,
                    hint,
                    prompt.description.unwrap_or_else(|| prompt.name.clone()),
                ));
            }
        }
        rows
    }

    /// `/mcp__<server>__<prompt> arguments`: fetches the prompt and flattens its answer into
    /// one user turn's text. The command name is the cleaned `mcp__<server>__<prompt>` the
    /// menu lists; a missing prompt, bad arguments, a fetch failure or an answer that is not
    /// user-role text is an error naming what is connected.
    pub async fn run_mcp_prompt(&self, name: &str, argument: &str) -> Result<String, RuntimeError> {
        let matches: Vec<(String, PromptSummary)> = self
            .mcp
            .listings()
            .into_iter()
            .flat_map(|(server, listings)| {
                listings
                    .prompts
                    .into_iter()
                    .filter(|prompt| mcp_tool_name(&server, &prompt.name) == name)
                    .map(|prompt| (server.clone(), prompt))
                    .collect::<Vec<_>>()
            })
            .collect();
        let (server, prompt) = match matches.as_slice() {
            [] => {
                return Err(RuntimeError::new(format!(
                    "no MCP prompt named `{name}` is connected"
                )));
            }
            [only] => only.clone(),
            // Distinct prompt names can clean to the same command (`review.now` and
            // `review/now` both become `review_now`); picking one would invoke the wrong
            // prompt, so the collision is refused.
            many => {
                return Err(RuntimeError::new(format!(
                    "`/{name}` names more than one prompt: {}; rename one on its server",
                    many.iter()
                        .map(|(server, prompt)| format!("`{}` on `{server}`", prompt.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        };
        let arguments =
            parse_prompt_arguments(name, argument, &prompt).map_err(RuntimeError::new)?;
        let Some((connection, scope)) = self.mcp.slot(&server).and_then(|slot| slot.call_scope())
        else {
            return Err(RuntimeError::new(format!(
                "MCP server `{server}` is not connected"
            )));
        };
        let mut params = GetPromptRequestParams::new(prompt.name.clone());
        if !arguments.is_empty() {
            params.arguments = Some(serde_json::Map::from_iter(
                arguments
                    .into_iter()
                    .map(|(key, value)| (key, serde_json::Value::String(value))),
            ));
        }
        // The single-round variant: server-requested input is an error, not a driven round.
        // The fetch holds a root call guard so a server unload waits it out, and a failed
        // fetch can mark a dead transport for the watcher, as a failed tool call does.
        let _in_flight = scope.enter(true);
        let response = connection.peer().get_prompt_once(params).await;
        if let Err(error) = &response {
            scope.request_failed(error);
        }
        let response =
            response.map_err(|e| RuntimeError::caused(format!("fetching `/{name}` failed"), e))?;
        let result = match response {
            GetPromptResponse::Complete(result) => result,
            GetPromptResponse::InputRequired(_) => {
                return Err(RuntimeError::new(
                    "the server asked for input the client does not provide".to_string(),
                ));
            }
            other => {
                return Err(RuntimeError::new(format!(
                    "the server answered unexpectedly: {other:?}"
                )));
            }
        };
        flatten_prompt_messages(&result.messages).map_err(RuntimeError::new)
    }

    /// Why a server with no slot cannot be restarted.
    fn not_attempted(&self, name: &str) -> String {
        let plan = self.mcp_plan();
        let planned_and_cleared = plan.iter().any(|p| p.name == name && !p.needs_approval);
        if planned_and_cleared
            && let Err(error) = server_spec(
                &self.config,
                name,
                &self.primary_root(),
                &self.setup.data_dir,
                &self.setup.env,
                &self.setup.scrub_env,
            )
        {
            return error;
        }
        not_attempted(name, self.config.mcp_servers.contains_key(name), &plan)
    }

    /// Records the user's approval of a repository-defined MCP server in the trust store.
    pub fn approve_mcp_server(&self, plan: &McpServerPlan) -> Result<(), RuntimeError> {
        trust::approve_server(
            &self.setup.data_dir,
            &self.primary_root(),
            &plan.name,
            &plan.digest,
        )
        .map_err(|e| RuntimeError::caused("could not record the approval", e))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        McpServerPlan, env_key_matches, flatten_prompt_messages, headers_helper_refusal,
        not_attempted, output_limits, parse_prompt_arguments, server_digest,
    };
    use fontaine_tools::mcp::{PromptArgument, PromptSummary};
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::Path;

    fn greet() -> PromptSummary {
        PromptSummary {
            name: "greet".to_string(),
            description: Some("Greet someone".to_string()),
            arguments: vec![
                PromptArgument {
                    name: "name".to_string(),
                    required: true,
                },
                PromptArgument {
                    name: "mood".to_string(),
                    required: false,
                },
            ],
        }
    }

    #[test]
    fn prompt_arguments_parse_and_check_against_the_declared_ones() {
        let parsed = parse_prompt_arguments("mcp__web__greet", "name=Ada mood=calm", &greet())
            .expect("valid arguments");
        assert_eq!(parsed["name"], "Ada");
        assert_eq!(parsed["mood"], "calm");
        // A required argument alone is enough.
        let parsed =
            parse_prompt_arguments("mcp__web__greet", "name=Ada", &greet()).expect("valid");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn a_missing_required_argument_a_malformed_pair_and_an_unknown_key_refuse_with_the_usage() {
        for (argument, why) in [
            ("mood=calm", "missing: name"),
            ("name", "`name` is not a `key=value` pair"),
            ("name=Ada who=you", "`who` is not an argument it declares"),
        ] {
            let error = parse_prompt_arguments("mcp__web__greet", argument, &greet()).unwrap_err();
            assert!(error.contains(why), "{error}");
            assert!(error.contains("name (required), mood"), "{error}");
            assert!(
                error.contains("For example: `/mcp__web__greet name=<value> mood=<value>`"),
                "{error}"
            );
        }
    }

    #[test]
    fn only_user_role_text_flattens_and_other_content_refuses() {
        use rmcp::model::{ContentBlock, PromptMessage, Role};
        let messages = vec![
            PromptMessage::new_text(Role::User, "first"),
            PromptMessage::new_text(Role::User, "second"),
        ];
        assert_eq!(
            flatten_prompt_messages(&messages).unwrap(),
            "first\n\nsecond"
        );
        let assistant = vec![PromptMessage::new_text(Role::Assistant, "note")];
        let error = flatten_prompt_messages(&assistant).unwrap_err();
        assert!(error.contains("Assistant"), "{error}");
        assert!(error.contains("did nothing"), "{error}");
        let image = vec![PromptMessage::new(
            Role::User,
            ContentBlock::image("data", "image/png"),
        )];
        assert!(flatten_prompt_messages(&image).is_err());
        assert_eq!(flatten_prompt_messages(&[]).unwrap(), "");
    }

    fn limits_for(value: Option<&str>) -> (usize, Vec<String>) {
        let warnings = RefCell::new(Vec::new());
        let value = value.map(OsString::from);
        let limits = output_limits(
            &move |name| {
                (name == "MAX_MCP_OUTPUT_TOKENS")
                    .then(|| value.clone())
                    .flatten()
            },
            Some(Path::new("/scratch")),
            &|m| warnings.borrow_mut().push(m.to_string()),
        );
        assert_eq!(
            limits.spill.as_ref().unwrap().0,
            Path::new("/scratch/mcp-results")
        );
        (limits.max_tokens, warnings.into_inner())
    }

    #[test]
    fn expansion_changes_the_digest_and_its_absence_leaves_the_old_shape() {
        let mut server = crate::mcp_config::stdio(
            "echo",
            &["${TOKEN}".to_string()],
            Default::default(),
            Vec::new(),
        );
        let plain = server_digest("srv", &server);
        server.expand = true;
        assert_ne!(plain, server_digest("srv", &server));
        let shape = serde_json::json!({
            "name": "srv", "command": "echo", "args": ["${TOKEN}"],
            "env_vars": null, "env": null, "url": null,
        });
        use sha2::Digest;
        let old: String = sha2::Sha256::digest(serde_jcs::to_vec(&shape).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(plain, old);
    }

    #[test]
    fn headers_and_the_helper_change_the_digest_only_when_set() {
        let mut server = crate::mcp_config::http("https://h/mcp");
        let plain = server_digest("srv", &server);
        server.headers = Some(std::collections::BTreeMap::from([(
            "Authorization".to_string(),
            "Bearer t".to_string(),
        )]));
        assert_ne!(plain, server_digest("srv", &server));
        let with_headers = server_digest("srv", &server);
        server.headers_helper = Some("/opt/helper".to_string());
        assert_ne!(with_headers, server_digest("srv", &server));
        // Unset again: the digest returns to the shape without the keys.
        let mut bare = crate::mcp_config::http("https://h/mcp");
        bare.headers = None;
        bare.headers_helper = None;
        assert_eq!(plain, server_digest("srv", &bare));
    }

    #[test]
    fn the_headers_helper_is_refused_only_for_repository_defined_servers() {
        let mut server = crate::mcp_config::http("https://h/mcp");
        server.headers_helper = Some("helper".to_string());
        let origin = |slot, user_trust| crate::config::Origin { slot, user_trust };
        for slot in [
            crate::config::Slot::Managed,
            crate::config::Slot::Cli,
            crate::config::Slot::Local,
            crate::config::Slot::ProjectLocal,
            crate::config::Slot::User,
            crate::config::Slot::Plugin,
        ] {
            assert!(
                headers_helper_refusal("srv", &server, origin(slot, true)).is_none(),
                "{slot:?} is a user-trust scope"
            );
        }
        assert!(
            headers_helper_refusal(
                "srv",
                &server,
                origin(crate::config::Slot::ProjectLocal, false)
            )
            .is_some(),
            "a tracked project-local file is repository-controlled"
        );
        let project = origin(crate::config::Slot::Project, false);
        let refusal =
            headers_helper_refusal("srv", &server, project).expect("project scope is refused");
        assert!(refusal.contains("project"), "{refusal}");
        assert!(refusal.contains("`srv`"), "{refusal}");
        let mut no_helper = server.clone();
        no_helper.headers_helper = None;
        assert!(headers_helper_refusal("srv", &no_helper, project).is_none());
    }

    #[test]
    fn header_values_expand_like_the_url_for_a_repositorys_definition() {
        let env = |name: &str| (name == "SET").then(|| OsString::from("v"));
        let credential = |name: &str| name == "KEY";
        let cx = crate::mcp_json::StartContext {
            project_dir: Path::new("/root"),
            env: &env,
            is_credential: &credential,
        };
        assert!(
            crate::mcp_json::expand_at_start("Bearer ${SET}", &cx)
                .unwrap()
                .contains('v')
        );
        let err = crate::mcp_json::expand_at_start("Bearer ${KEY}", &cx).unwrap_err();
        assert!(err.contains("credential"), "{err}");
    }

    #[test]
    fn a_login_resolves_a_variable_url_the_way_a_connect_does() {
        let mut config = crate::config::Config::defaults("/root".into());
        let mut server = crate::mcp_config::http("https://${HOST}/mcp");
        server.expand = true;
        config.mcp_servers.insert("srv".to_string(), server);
        let env: super::EnvLookup =
            std::sync::Arc::new(|name| (name == "HOST").then(|| OsString::from("h.example")));

        let resolved = super::resolved_server(&config, "srv", Path::new("/root"), &env, &[])
            .unwrap()
            .expect("the server is configured");
        assert_eq!(resolved.url.as_deref(), Some("https://h.example/mcp"));

        let unset: super::EnvLookup = std::sync::Arc::new(|_| None);
        let err =
            super::resolved_server(&config, "srv", Path::new("/root"), &unset, &[]).unwrap_err();
        assert!(err.contains("HOST"), "{err}");
        assert!(
            super::resolved_server(&config, "nope", Path::new("/root"), &env, &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_token_limit_defaults_and_follows_a_valid_environment_value() {
        assert_eq!(limits_for(None), (25_000, Vec::new()));
        assert_eq!(limits_for(Some("40000")), (40_000, Vec::new()));
        assert_eq!(limits_for(Some(" 8000 ")), (8_000, Vec::new()));
    }

    #[test]
    fn an_unusable_token_limit_is_warned_about_and_the_default_used() {
        for bad in ["lots", "0", "-5", "1.5", ""] {
            let (tokens, warnings) = limits_for(Some(bad));
            assert_eq!(tokens, 25_000, "{bad}");
            assert_eq!(warnings.len(), 1, "{bad}");
            assert!(warnings[0].contains("MAX_MCP_OUTPUT_TOKENS"), "{bad}");
        }
    }

    fn planned(name: &str, needs_approval: bool) -> McpServerPlan {
        McpServerPlan {
            name: name.to_string(),
            description: String::new(),
            project: true,
            needs_approval,
            digest: String::new(),
        }
    }

    #[test]
    fn a_server_without_a_slot_is_refused_with_the_reason_it_was_never_started() {
        let plan = [planned("left-off", true)];
        assert!(not_attempted("repo", true, &plan).contains("workspace is not trusted"));
        assert!(not_attempted("left-off", true, &plan).contains("left off at startup"));
        assert!(not_attempted("nope", false, &plan).contains("no MCP server named `nope`"));
        // A name nobody configured is unknown even when the plan is empty.
        assert!(not_attempted("nope", false, &[]).contains("no MCP server named"));
    }

    #[test]
    fn without_a_scratch_directory_there_is_nowhere_to_spill() {
        let limits = output_limits(&|_| None, None, &|_| {});
        assert!(limits.spill.is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn env_keys_match_case_sensitively_off_windows() {
        assert!(env_key_matches(&OsString::from("PATH"), "PATH"));
        assert!(!env_key_matches(&OsString::from("PATH"), "path"));
    }

    #[cfg(windows)]
    #[test]
    fn env_keys_match_case_insensitively_on_windows() {
        assert!(env_key_matches(&OsString::from("PATH"), "path"));
        assert!(!env_key_matches(&OsString::from("PATH"), "HOME"));
    }
}
