//! Starting the MCP servers a session would start, only to count their tools, for `fontaine
//! doctor` and `fontaine mcp`. A server a session would hold back for the user's say-so is
//! reported, never started.

use fontaine_core::agent::{mcp_plan, probe_server, server_spec};
use fontaine_core::config::Config;
use fontaine_core::dirs::Dirs;
use fontaine_core::doctor_report::{self as report, McpGate, McpState};
use fontaine_core::plugins::{self, PluginContributions};
use fontaine_tools::error_chain::chain;

use crate::{Host, shared_env};

/// One configured server and what probing it came to.
pub(crate) struct ProbedServer {
    pub name: String,
    pub gate: McpGate,
    pub state: McpState,
}

/// The configuration with the installed plugins applied, or as it was with the reason they were
/// not. Staged updates are not applied, because applying them writes.
pub(crate) fn apply_installed_plugins(
    config: Config,
    dirs: &Dirs,
    host: &Host,
) -> (Config, PluginContributions, Option<String>) {
    let loaded = plugins::load_installed(
        &dirs.config,
        &plugins::PluginContext {
            project_root: &config.workspace,
            env: &*host.env,
        },
    );
    let contributions = match loaded {
        Ok(contributions) => contributions,
        Err(e) => return (config, PluginContributions::default(), Some(chain(&e))),
    };
    let mut applied = config.clone();
    match applied.apply_plugins(&contributions) {
        Ok(()) => (applied, contributions, None),
        Err(e) => (config, PluginContributions::default(), Some(chain(&e))),
    }
}

/// Probes the configured servers, or only `only`. The interactive rule with no prompt: a
/// repository's server the user has not approved, or any of them in an untrusted workspace, is
/// never run. With `connect` false nothing is started.
pub(crate) async fn probe_servers(
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    trusted: bool,
    connect: bool,
    only: Option<&str>,
) -> Vec<ProbedServer> {
    let plan = mcp_plan(config, &config.workspace, &dirs.data, trusted, true);
    let env = shared_env(host);
    let scrubbed = fontaine_core::tools::scrubbed_env(config, (host.env_names)());
    let probes = config
        .mcp_servers
        .keys()
        .filter(|name| only.is_none_or(|only| only == name.as_str()))
        .map(|name| {
            let gate = report::mcp_gate(plan.iter().find(|p| &p.name == name));
            let (env, scrubbed) = (env.clone(), &scrubbed);
            async move {
                let state = match gate {
                    McpGate::Untrusted => {
                        McpState::NotStarted("the workspace is not trusted".into())
                    }
                    McpGate::Unapproved => McpState::NotStarted(
                        "a repository-defined server the user has not approved; the interactive UI asks first"
                            .into(),
                    ),
                    McpGate::Start if !connect => {
                        McpState::NotStarted("probe skipped (--connect=no)".into())
                    }
                    McpGate::Start => probe(config, dirs, name, &env, scrubbed).await,
                };
                ProbedServer {
                    name: name.clone(),
                    gate,
                    state,
                }
            }
        });
    futures::future::join_all(probes).await
}

async fn probe(
    config: &Config,
    dirs: &Dirs,
    name: &str,
    env: &fontaine_core::agent::EnvLookup,
    scrubbed: &[std::ffi::OsString],
) -> McpState {
    let spec = match server_spec(config, name, &config.workspace, &dirs.data, env, scrubbed) {
        Ok(Some(spec)) => spec,
        Ok(None) => return McpState::Failed("names neither a command nor a URL".into()),
        Err(error) => return McpState::Failed(error),
    };
    let network = match config.loaded_network(None) {
        Ok(network) => network,
        Err(e) => return McpState::Failed(chain(&e)),
    };
    let env = env.clone();
    match probe_server(&spec, &network, &move |name| env(name)).await {
        Ok(tools) => McpState::Connected(tools),
        Err(error) => McpState::Failed(error),
    }
}
