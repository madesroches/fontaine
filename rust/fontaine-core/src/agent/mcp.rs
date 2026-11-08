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
use fontaine_tools::mcp::{ServerSpec, Transport, connect as connect_server};
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::tls::LoadedNetwork;
use futures::future::BoxFuture;
use rig_core::tool::{DynamicTool, ManagedToolSink, ManagedToolToken};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::AgentRuntime;
use crate::config::{Config, McpServer};
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
    match (&server.command, &server.url) {
        (Some(command), _) => {
            let mut line = command.clone();
            for arg in server.args.iter().flatten() {
                line.push(' ');
                line.push_str(arg);
            }
            if let Some(names) = server.env_vars.as_ref().filter(|n| !n.is_empty()) {
                line.push_str(&format!("\nenvironment: {}", names.join(", ")));
            }
            line
        }
        (None, Some(url)) => url.clone(),
        (None, None) => String::new(),
    }
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

/// How to start the configured server `name` with its working directory at `root`; `None` when
/// it is unknown or names neither a command nor a URL.
pub fn server_spec(config: &Config, name: &str, root: &Path) -> Option<ServerSpec> {
    let server = config.mcp_servers.get(name)?;
    let transport = match (&server.command, &server.url) {
        (Some(command), _) => Transport::Stdio {
            command: command.clone(),
            args: server.args.clone().unwrap_or_default(),
            env_vars: server.env_vars.clone().unwrap_or_default(),
            env: merged_server_env(tools::subprocess_env(config), &server.env),
            cwd: root.to_path_buf(),
        },
        (None, Some(url)) => Transport::Http { url: url.clone() },
        (None, None) => return None,
    };
    Some(ServerSpec {
        name: name.to_string(),
        transport,
    })
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
    let attempt = async {
        let connected = connect_server(spec, sink.clone(), guard, false, network, env)
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
    /// `approved`. A server that fails to start is reported and skipped; the session goes on.
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
        let mut outcomes = Vec::new();
        for plan in self.mcp_plan() {
            if plan.needs_approval && !approved.contains(&plan.name) {
                continue;
            }
            let Some(spec) = server_spec(&self.config, &plan.name, &self.primary_root()) else {
                continue;
            };
            let env = self.setup.env.clone();
            match connect_server(
                &spec,
                self.tool_server.clone(),
                guard.clone(),
                reveal,
                &self.setup.network,
                &move |name| env(name),
            )
            .await
            {
                Ok(connected) => {
                    self.mcp.push(connected);
                    outcomes.push(McpOutcome {
                        server: plan.name,
                        error: None,
                    });
                }
                Err(e) => {
                    let rendered = chain(&e);
                    self.approver.warn(&rendered);
                    outcomes.push(McpOutcome {
                        server: plan.name,
                        error: Some(rendered),
                    });
                }
            }
        }
        // The rebuild at startup derived each rebuilt agent's toolset before any server
        // connected; now that the tool server holds every MCP tool, the handle-less rebuilt
        // toolsets are re-derived, parents before children, so a later revival carries them.
        self.state
            .agents()
            .refresh_rebuilt_toolsets(&self.agent_types);
        outcomes
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
    use super::env_key_matches;
    use std::ffi::OsString;

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
