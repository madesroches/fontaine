//! Starting the MCP servers a session is configured with, and the approval repository-defined
//! ones need first.

use std::collections::BTreeMap;
use std::ffi::OsString;

use fontaine_mcp::{ServerSpec, Transport, connect as connect_server};
use fontaine_tools::error_chain::chain;
use fontaine_tools::guarded::Guard;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::AgentRuntime;
use crate::config::McpServer;
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

fn server_digest(name: &str, server: &McpServer) -> String {
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

impl AgentRuntime {
    /// The MCP servers this session would start, and which of them need the user's say-so.
    pub fn mcp_plan(&self) -> Vec<McpServerPlan> {
        let root = self.primary_root();
        self.config
            .mcp_servers
            .iter()
            .filter_map(|(name, server)| {
                let origin = self
                    .config
                    .mcp_origins
                    .get(name)
                    .copied()
                    .unwrap_or_default();
                let project = origin.is_project();
                if project && !self.setup.project_executables {
                    return None;
                }
                let digest = server_digest(name, server);
                // A repository's server needs the user's approval; one in the user's own
                // settings, or in an untracked local file of a trusted root, does not.
                let repository_defined =
                    origin.slot == crate::config::Slot::Project || (project && !origin.user_trust);
                let needs_approval = repository_defined
                    && self.setup.confirm_project_servers
                    && !trust::server_approved(&self.setup.data_dir, &root, name, &digest);
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
        };
        let mut outcomes = Vec::new();
        for plan in self.mcp_plan() {
            if plan.needs_approval && !approved.contains(&plan.name) {
                continue;
            }
            let Some(server) = self.config.mcp_servers.get(&plan.name) else {
                continue;
            };
            let spec = ServerSpec {
                name: plan.name.clone(),
                transport: match (&server.command, &server.url) {
                    (Some(command), _) => Transport::Stdio {
                        command: command.clone(),
                        args: server.args.clone().unwrap_or_default(),
                        env_vars: server.env_vars.clone().unwrap_or_default(),
                        env: merged_server_env(tools::subprocess_env(&self.config), &server.env),
                        cwd: self.primary_root(),
                    },
                    (None, Some(url)) => Transport::Http { url: url.clone() },
                    (None, None) => continue,
                },
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
