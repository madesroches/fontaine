//! Starting the MCP servers a session is configured with, and the approval repository-defined
//! ones need first.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::{CallView, GateDecision, ToolGate};
use fontaine_tools::guarded::Guard;
use fontaine_tools::mcp::output::{DEFAULT_MAX_TOKENS, OutputLimits, Spill};
use fontaine_tools::mcp::{
    ElicitForm, ElicitReply, Elicitor, McpOptions, ServerSpec, Transport, connect as connect_server,
};
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::tls::LoadedNetwork;
use futures::future::BoxFuture;
use rig_core::tool::{DynamicTool, ManagedToolSink, ManagedToolToken};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::mcp_supervisor::{
    McpServerState, ReconnectTarget, ServerConnector, ServerSlot, Supervisor,
};
use super::{AgentRuntime, EnvLookup};
use crate::config::{Config, McpServer};
use crate::mcp_json::{StartContext, expand_at_start};
use crate::permissions::Approver;
use crate::runtime_error::RuntimeError;
use crate::tools;
use crate::trust;

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

pub fn server_digest(name: &str, server: &McpServer) -> String {
    let canonical = json!({
        "name": name,
        "command": server.command,
        "args": server.args,
        "env_vars": server.env_vars,
        "env": server.env,
        "url": server.url,
    });
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
            .chain(server.url.iter());
        for text in texts {
            for name in crate::mcp_json::references(text) {
                if !variables.contains(&name) {
                    variables.push(name);
                }
            }
        }
    }
    let mut line = match (&server.command, &server.url) {
        (Some(command), _) => {
            let mut line = command.clone();
            for arg in server.args.iter().flatten() {
                line.push(' ');
                line.push_str(arg);
            }
            line
        }
        (None, Some(url)) => url.clone(),
        (None, None) => String::new(),
    };
    if !variables.is_empty() && !line.is_empty() {
        line.push_str(&format!("\nenvironment: {}", variables.join(", ")));
    }
    line
}

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
    Ok(out)
}

/// How to start the configured server `name` with its working directory at `root`; `Ok(None)`
/// when it is unknown or names neither a command nor a URL. A repository's definition has its
/// `${VAR}` references expanded here, through `env`; `scrubbed` are the credential variables it
/// may not expand. A reference that cannot be expanded is an error naming it.
pub fn server_spec(
    config: &Config,
    name: &str,
    root: &Path,
    env: &EnvLookup,
    scrubbed: &[OsString],
) -> Result<Option<ServerSpec>, String> {
    let Some(defined) = config.mcp_servers.get(name) else {
        return Ok(None);
    };
    let server = if defined.expand {
        let is_credential = credential_check(config, scrubbed);
        expanded(defined, root, env, &is_credential)
            .map_err(|e| format!("MCP server `{name}`: {e}"))?
    } else {
        defined.clone()
    };
    let transport = match (&server.command, &server.url) {
        (Some(command), _) => Transport::Stdio {
            command: command.clone(),
            args: server.args.clone().unwrap_or_default(),
            env_vars: server.env_vars.clone().unwrap_or_default(),
            env: merged_server_env(tools::subprocess_env(config), &server.env),
            cwd: root.to_path_buf(),
        },
        (None, Some(url)) => Transport::Http { url: url.clone() },
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

    /// Starts the MCP servers the plan allows: those that need no approval and those named in
    /// `approved`, all at once. A server that fails to start is reported and skipped, and keeps a
    /// slot so `/mcp reconnect` can try it again; the session goes on.
    pub async fn connect_mcp(&mut self, approved: &[String]) -> Vec<McpOutcome> {
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
        options.limits = Arc::new(limits);
        options.elicitor = Arc::new(ApproverElicitor(approver.clone()));
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
        let supervisor = Supervisor::new(
            Arc::new(connector),
            approver.clone(),
            Arc::new(move || state.agents().refresh_rebuilt_toolsets(&types)),
        );
        self.mcp.supervisor = Some(supervisor.clone());

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
        // The rebuild at startup derived each rebuilt agent's toolset before any server
        // connected; now that the tool server holds every MCP tool, the handle-less rebuilt
        // toolsets are re-derived, parents before children, so a later revival carries them.
        self.state
            .agents()
            .refresh_rebuilt_toolsets(&self.agent_types);
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
        (attempts, failed)
    }

    /// The state of each MCP server the session attempted, by name.
    pub fn mcp_states(&self) -> Vec<(String, McpServerState)> {
        self.mcp.states()
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
        futures::future::join_all(slots.into_iter().map(|slot| {
            let supervisor = supervisor.clone();
            async move {
                McpOutcome {
                    server: slot.name().to_string(),
                    error: supervisor.reconnect(&slot).await.err().map(|e| chain(&e)),
                }
            }
        }))
        .await
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
    use super::{McpServerPlan, env_key_matches, not_attempted, output_limits};
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::Path;

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
