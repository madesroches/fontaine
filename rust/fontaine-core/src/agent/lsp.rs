//! The language servers of a session, built from the resolved configuration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fontaine_tools::lsp::{LspServerSpec, LspServers, LspWorkspace};
use fontaine_tools::scope::Scope;

use super::EnvLookup;
use super::mcp::merged_server_env;
use crate::config::{
    Config, LSP_MAX_RESTARTS, LSP_REQUEST_TIMEOUT_MS, LSP_SHUTDOWN_TIMEOUT_MS,
    LSP_STARTUP_TIMEOUT_MS,
};
use crate::gate::SessionState;
use crate::tools;

/// The session's scope as the servers read it, so a root added by `/add-dir` is seen by a
/// server that starts afterwards.
struct StateWorkspace {
    state: Arc<SessionState>,
}

impl LspWorkspace for StateWorkspace {
    fn scope(&self) -> Arc<Scope> {
        self.state.scope()
    }

    fn folders(&self) -> Vec<PathBuf> {
        self.state
            .scope()
            .roots()
            .iter()
            .filter(|root| {
                Some(root.as_path()) != self.state.scratch.as_deref()
                    && Some(root.as_path()) != self.state.memory_root.as_deref()
            })
            .cloned()
            .collect()
    }
}

pub(super) fn workspace(state: &Arc<SessionState>) -> Arc<dyn LspWorkspace> {
    Arc::new(StateWorkspace {
        state: state.clone(),
    })
}

pub(super) fn build(config: &Config, state: &Arc<SessionState>, env: EnvLookup) -> Arc<LspServers> {
    let table = config.lsp_table();
    let base = tools::subprocess_env(config);
    let specs = table
        .servers
        .into_iter()
        .map(|(name, server)| LspServerSpec {
            name,
            command: server.command,
            args: server.args.unwrap_or_default(),
            env_vars: server.env_vars.unwrap_or_default(),
            set_env: merged_server_env(base.clone(), &server.env),
            extension_to_language: server.extension_to_language,
            initialization_options: server.initialization_options,
            settings: server.settings,
            startup_timeout: Duration::from_millis(
                server.startup_timeout_ms.unwrap_or(LSP_STARTUP_TIMEOUT_MS),
            ),
            shutdown_timeout: Duration::from_millis(
                server
                    .shutdown_timeout_ms
                    .unwrap_or(LSP_SHUTDOWN_TIMEOUT_MS),
            ),
            request_timeout: Duration::from_millis(
                server.request_timeout_ms.unwrap_or(LSP_REQUEST_TIMEOUT_MS),
            ),
            restart_on_crash: server.restart_on_crash.unwrap_or(true),
            max_restarts: server.max_restarts.unwrap_or(LSP_MAX_RESTARTS),
            diagnostics: server.diagnostics.unwrap_or(true),
        })
        .collect();
    LspServers::new(specs, &table.claims, workspace(state), env)
}
