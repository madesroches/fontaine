//! The wrapped tool registry for a session.

use std::ffi::OsString;
use std::sync::Arc;

use fontaine_tools::builtins::{self, BuiltinEnv, Registered, Seen, ShellSettings};
use fontaine_tools::gate::ToolGate;
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::scope::Scope;
use fontaine_tools::webfetch::WebfetchConfig;

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
        .chain(config.guardian.scanner_hf_token_env.iter());
    names.extend(configured.map(OsString::from));
    names.sort();
    names.dedup();
    names
}

pub struct RegistryParams {
    pub gate: Arc<dyn ToolGate>,
    pub scope: Arc<Scope>,
    /// Read-before-edit tracking, kept across registry rebuilds.
    pub seen: Seen,
    pub scrub_env: Vec<OsString>,
    pub git_bash: Option<GitBash>,
}

/// Every built-in tool, each wrapped so the permission pipeline decides before it runs.
pub fn registry(params: RegistryParams) -> Result<Vec<Registered>, String> {
    let guard = Guard {
        gate: params.gate,
        results: Arc::new(ResultPipeline::new()),
    };
    builtins::build(&BuiltinEnv {
        scope: params.scope,
        seen: params.seen,
        guard,
        shell: ShellSettings {
            scrub_env: params.scrub_env,
            git_bash: params.git_bash,
            ..ShellSettings::default()
        },
        webfetch: WebfetchConfig::default(),
    })
    .map_err(|e| format!("cannot set up the webfetch tool: {e}"))
}
