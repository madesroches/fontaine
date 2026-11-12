//! The wrapped tool registry for a session.

use std::ffi::OsString;
use std::sync::Arc;

use fontaine_tools::builtins::{self, BuiltinEnv, Registered, Seen, ShellSettings};
use fontaine_tools::checkpoint::PreImages;
use fontaine_tools::gate::ToolGate;
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::guarded::{Guard, GuardedTool};
use fontaine_tools::jobs::{AgentKills, AgentWaits, Jobs, WaitWake};
use fontaine_tools::kills::KillSwitches;
use fontaine_tools::mcp::McpResourceQueries;
use fontaine_tools::mcp::output::OutputLimits;
use fontaine_tools::powershell::PowerShellSettings;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::{RiskClass, ToolSource, registered_risk};
use fontaine_tools::scope::Scope;
use fontaine_tools::shell_cwd::ShellCwd;
use fontaine_tools::tls::LoadedNetwork;
use fontaine_tools::webfetch::WebfetchConfig;
use fontaine_tools::websearch::{SearxngChild, WebsearchConfig};
use rig_core::tool::Tool;

use crate::config::Config;

/// The variables the `shell` tool's child processes must not inherit: telemetry credentials,
/// every configured account's API key variable and the scanner's token variable.
/// `process_env_names` are the names present in the process environment, which is how the
/// `MICROMEGAS_*` family and the OTLP header variables are found.
pub fn scrubbed_env(
    config: &Config,
    process_env_names: impl IntoIterator<Item = OsString>,
) -> Vec<OsString> {
    let mut names: Vec<OsString> = Vec::new();
    for name in process_env_names {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        let otlp_headers = upper.starts_with("OTEL_EXPORTER_OTLP_") && upper.ends_with("HEADERS");
        if upper.starts_with("MICROMEGAS_") || otlp_headers {
            names.push(name);
        }
    }
    names.push(OsString::from("OTEL_EXPORTER_OTLP_HEADERS"));
    let configured = config
        .telemetry
        .api_key_env
        .iter()
        .chain(
            config
                .accounts
                .values()
                .filter_map(|a| a.api_key_env.as_ref()),
        )
        .chain(std::iter::once(&config.guardian.scanner_hf_token_env));
    names.extend(configured.map(OsString::from));
    names.sort();
    names.dedup();
    names
}

/// The `[env]` overrides every tool and extension subprocess gets, in name order.
pub fn subprocess_env(config: &Config) -> Vec<(OsString, OsString)> {
    config
        .env
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value.as_str())))
        .collect()
}

/// One tool beyond the built-ins, built by the caller (it needs session state) and registered
/// under the class its descriptor row gives it.
pub struct ExtensionTool(Box<dyn FnOnce(&Guard) -> Registered>);

impl ExtensionTool {
    pub fn new<T: Tool + 'static>(tool: T) -> Self {
        Self(Box::new(move |guard| register_extension(tool, guard)))
    }
}

/// The tools beyond the built-ins: the subagent tools, the skill loader, the question dialog and
/// the task list.
pub struct ExtensionTools(pub Vec<ExtensionTool>);

pub struct RegistryParams {
    pub gate: Arc<dyn ToolGate>,
    pub scope: Arc<Scope>,
    /// How the MCP resource tools reach the session's connected servers.
    pub mcp_resources: Arc<dyn McpResourceQueries>,
    /// The capping policy the `read_mcp_resource` tool holds its results to.
    pub mcp_limits: Arc<OutputLimits>,
    /// Reports a failed spill save from the MCP resource tools to the person.
    pub mcp_warn: Arc<dyn Fn(&str) + Send + Sync>,
    /// Read-before-edit tracking, kept across registry rebuilds.
    pub seen: Seen,
    pub scrub_env: Vec<OsString>,
    pub set_env: Vec<(OsString, OsString)>,
    /// The ceiling on a model-requested `shell` timeout, in seconds.
    pub shell_max_timeout_secs: u64,
    pub git_bash: Option<GitBash>,
    /// The interpreter of the `powershell` tool; with none, there is no such tool.
    pub powershell: Option<PowerShellSettings>,
    /// The post-call stages every result passes.
    pub results: Arc<ResultPipeline>,
    /// Telemetry names MCP tools `third-party`.
    pub hide_mcp_names: bool,
    /// The `[network]` settings webfetch is built with.
    pub network: Arc<LoadedNetwork>,
    /// The search backend, when one is configured; without it there is no `websearch` tool.
    pub websearch: Option<WebsearchConfig>,
    /// The SearXNG process, owned by the runtime so a rebuild does not restart it.
    pub searxng: Arc<SearxngChild>,
    /// The session's jobs, owned by the runtime so a rebuild keeps the running ones.
    pub jobs: Arc<Jobs>,
    /// Each agent's shell directory, owned by the runtime so a rebuild keeps where they are.
    pub shell_cwd: ShellCwd,
    /// The session scratch directory; with none, the shell directory never moves.
    pub scratch: Option<std::path::PathBuf>,
    /// How `job_output` waits on a subagent; `None` refuses agent ids.
    pub agent_waits: Option<Arc<dyn AgentWaits>>,
    /// How `job_cancel` stops a subagent; `None` refuses agent ids.
    pub agent_kills: Option<Arc<dyn AgentKills>>,
    /// What ends a `job_output` wait early.
    pub wait_wake: Option<Arc<dyn WaitWake>>,
    pub extension: Option<ExtensionTools>,
    /// Saves a file before `write` or `edit` changes it; `None` saves nothing.
    pub pre_images: Option<Arc<dyn PreImages>>,
    /// The session's kill switches: every guarded tool refuses a killed agent's call.
    pub kills: Arc<KillSwitches>,
}

fn register_extension<T: Tool + 'static>(tool: T, guard: &Guard) -> Registered {
    // A tool without a descriptor row is the riskiest class, so a forgotten row only makes it
    // stricter.
    let risk = registered_risk(T::NAME).unwrap_or(RiskClass::Arbitrary);
    let guarded = GuardedTool::single(tool, risk, guard.clone());
    Registered {
        name: T::NAME,
        risk: guarded.risk(),
        source: ToolSource::Builtin,
        tool: guarded.into_dynamic(),
    }
}

/// Every tool a session registers by name, each wrapped so the permission pipeline decides
/// before it runs. MCP tools join later, wrapped the same way, as their servers connect.
pub fn registry(params: RegistryParams) -> Result<Vec<Registered>, String> {
    let guard = Guard {
        gate: params.gate,
        results: params.results,
        hide_mcp_names: params.hide_mcp_names,
        pre_images: params.pre_images,
        kills: Some(params.kills),
    };
    let mut tools = builtins::build(&BuiltinEnv {
        scope: params.scope,
        mcp_resources: params.mcp_resources,
        mcp_limits: params.mcp_limits,
        mcp_warn: params.mcp_warn,
        seen: params.seen,
        guard: guard.clone(),
        shell: ShellSettings {
            scrub_env: params.scrub_env,
            set_env: params.set_env,
            git_bash: params.git_bash,
            max_timeout_secs: params.shell_max_timeout_secs,
            powershell: params.powershell,
        },
        webfetch: WebfetchConfig::default(),
        websearch: params.websearch,
        searxng: params.searxng,
        network: params.network,
        jobs: params.jobs,
        agent_waits: params.agent_waits,
        agent_kills: params.agent_kills,
        wait_wake: params.wait_wake,
        shell_cwd: params.shell_cwd,
        scratch: params.scratch,
        git_fast_path_runner: Some(Arc::new(|cmd, repo_root, cwd| {
            crate::git_fast_path::run(cmd, repo_root, cwd)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync + 'static>)
        })),
    })
    .map_err(|e| fontaine_tools::error_chain::chain(&e))?;
    if let Some(extension) = params.extension {
        tools.extend(extension.0.into_iter().map(|register| (register.0)(&guard)));
    }
    Ok(tools)
}
