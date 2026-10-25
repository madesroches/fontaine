//! The wrapped tool registry for a session.

use std::ffi::OsString;
use std::sync::Arc;

use fontaine_agents::spawn::{AgentTool, SendMessageTool};
use fontaine_tools::builtins::{self, BuiltinEnv, Registered, Seen, ShellSettings};
use fontaine_tools::gate::ToolGate;
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::guarded::{Guard, GuardedTool};
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::{RiskClass, ToolSource, extension_risk};
use fontaine_tools::scope::Scope;
use fontaine_tools::skill::SkillTool;
use fontaine_tools::webfetch::WebfetchConfig;
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

/// The tools beyond the built-ins: the subagent tools and the skill loader.
pub struct ExtensionTools {
    pub agent: AgentTool,
    pub send_message: SendMessageTool,
    pub skill: SkillTool,
}

pub struct RegistryParams {
    pub gate: Arc<dyn ToolGate>,
    pub scope: Arc<Scope>,
    /// Read-before-edit tracking, kept across registry rebuilds.
    pub seen: Seen,
    pub scrub_env: Vec<OsString>,
    pub set_env: Vec<(OsString, OsString)>,
    pub git_bash: Option<GitBash>,
    /// The post-call stages every result passes.
    pub results: Arc<ResultPipeline>,
    /// Telemetry names MCP tools `third-party`.
    pub hide_mcp_names: bool,
    pub extension: Option<ExtensionTools>,
}

fn register_extension<T: Tool + 'static>(tool: T, guard: &Guard) -> Registered {
    // A tool missing from the table is the riskiest class, so a forgotten entry only makes it
    // stricter.
    let risk = extension_risk(T::NAME).unwrap_or(RiskClass::Arbitrary);
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
    };
    let mut tools = builtins::build(&BuiltinEnv {
        scope: params.scope,
        seen: params.seen,
        guard: guard.clone(),
        shell: ShellSettings {
            scrub_env: params.scrub_env,
            set_env: params.set_env,
            git_bash: params.git_bash,
            ..ShellSettings::default()
        },
        webfetch: WebfetchConfig::default(),
    })
    .map_err(|e| {
        format!(
            "cannot set up the webfetch tool: {}",
            fontaine_tools::error_chain::chain(&e.without_url())
        )
    })?;
    if let Some(extension) = params.extension {
        tools.push(register_extension(extension.agent, &guard));
        tools.push(register_extension(extension.skill, &guard));
        tools.push(register_extension(extension.send_message, &guard));
    }
    Ok(tools)
}
