use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::config_scope::check_scope;
use crate::credentials;
use crate::permission_rules::{RuleError, RuleSource, SourcedRule, parse_rule, validate_mcp_name};
use crate::providers::{self, Provider};
use crate::trust::{self, LocalClassification, ProjectDemotion};
use fontaine_tools::names::{is_builtin, is_mcp};
use fontaine_tools::tls::{LoadedNetwork, NetworkError, NetworkSettings};

/// Every config problem maps to process exit code 2.
pub const CONFIG_EXIT_CODE: u8 = 2;

#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Write {
        path: PathBuf,
        source: io::Error,
    },
    /// Not TOML, or TOML that does not match the schema (including unknown keys and sections).
    Parse {
        path: PathBuf,
        message: String,
    },
    /// A key the file's scope may not set.
    Scope {
        path: PathBuf,
        column: Column,
        key: String,
        reason: String,
        hint: Option<String>,
    },
    /// A credentials file that does not match the schema. Carries no source text: toml quotes
    /// the offending line, which here would be the key.
    Credentials {
        path: PathBuf,
        /// `None` for structural failures, which have no single source position.
        position: Option<(usize, usize)>,
        message: String,
    },
    /// A value that parses but is not acceptable.
    Invalid {
        path: PathBuf,
        key: String,
        message: String,
    },
    /// A config or data directory with no platform default and no override.
    Dir {
        what: &'static str,
        var: &'static str,
    },
    /// `[telemetry] enabled = true` without an endpoint or an ingestion key.
    Telemetry {
        message: String,
    },
    /// A `[websearch]` section that is inconsistent or cannot be resolved (a missing key).
    Websearch {
        message: String,
    },
    /// A `[network]` or per-account network setting that cannot be loaded.
    Network {
        key: String,
        source: NetworkError,
    },
    /// A model reference or account that cannot be resolved: unknown account or model, no
    /// main model, a missing credential.
    Model {
        message: String,
    },
    /// A command line or input that cannot be acted on.
    Usage {
        message: String,
    },
}

impl ConfigError {
    pub fn exit_code(&self) -> u8 {
        CONFIG_EXIT_CODE
    }

    pub fn model(message: impl Into<String>) -> Self {
        ConfigError::Model {
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        ConfigError::Usage {
            message: message.into(),
        }
    }

    pub(crate) fn invalid(path: &Path, key: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError::Invalid {
            path: path.to_path_buf(),
            key: key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            ConfigError::Write { path, source } => {
                write!(f, "cannot write {}: {source}", path.display())
            }
            ConfigError::Parse { path, message } => {
                write!(f, "invalid settings in {}: {message}", path.display())
            }
            ConfigError::Credentials {
                path,
                position,
                message,
            } => match position {
                Some((line, column)) => write!(
                    f,
                    "invalid credentials file {}:{line}:{column}: {message}",
                    path.display()
                ),
                None => write!(f, "invalid credentials file {}: {message}", path.display()),
            },
            ConfigError::Scope {
                path,
                column,
                key,
                reason,
                hint,
            } => {
                write!(
                    f,
                    "{}: `{key}` cannot be set from {} scope: {reason}",
                    path.display(),
                    column.label()
                )?;
                if let Some(hint) = hint {
                    write!(f, "\n{hint}")?;
                }
                Ok(())
            }
            ConfigError::Invalid { path, key, message } => {
                write!(f, "{}: `{key}`: {message}", path.display())
            }
            ConfigError::Dir { what, var } => write!(
                f,
                "cannot determine the {what} directory; set {var} to a directory"
            ),
            ConfigError::Telemetry { message } => write!(f, "invalid [telemetry]: {message}"),
            ConfigError::Websearch { message } => write!(f, "invalid [websearch]: {message}"),
            ConfigError::Network { key, source } => write!(
                f,
                "invalid network settings in `{key}`: {}",
                fontaine_tools::error_chain::chain(source)
            ),
            ConfigError::Model { message } | ConfigError::Usage { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } | ConfigError::Write { source, .. } => Some(source),
            ConfigError::Network { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The per-key scope table column a settings file is validated against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// The user file: may set every key.
    User,
    /// `--settings` overlay.
    Cli,
    /// An untracked `settings.local.toml` in a trusted root.
    ProjectLocal,
    /// Shared project settings, and any project-local file that did not earn user trust.
    Project,
}

impl Column {
    pub fn label(self) -> &'static str {
        match self {
            Column::User => "user",
            Column::Cli => "CLI",
            Column::ProjectLocal => "project-local",
            Column::Project => "project",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum PermissionMode {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "dontAsk")]
    DontAsk,
    #[serde(rename = "auto")]
    Auto,
}

impl PermissionMode {
    /// The name the mode has in settings, on the command line and in every event.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Manual => "manual",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Auto => "auto",
        }
    }
}

/// The provider an account talks to: a handle on one entry of the provider registry.
#[derive(Clone, Copy, JsonSchema)]
#[schemars(with = "String")]
pub struct AccountKind(&'static dyn Provider);

impl AccountKind {
    /// The name the kind has in settings and in the guardian model list.
    pub fn as_str(self) -> &'static str {
        self.0.name()
    }

    pub fn provider(self) -> &'static dyn Provider {
        self.0
    }

    /// The kind registered under `name`, if any.
    pub fn find(name: &str) -> Option<AccountKind> {
        providers::find(name).map(AccountKind)
    }

    /// For tests: the kind registered under `name`.
    pub fn named(name: &str) -> AccountKind {
        Self::find(name).unwrap_or_else(|| panic!("no provider is registered as `{name}`"))
    }
}

impl PartialEq for AccountKind {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for AccountKind {}

impl std::fmt::Debug for AccountKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AccountKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        AccountKind::find(&name).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown account kind `{name}`; expected one of: {}",
                providers::names().collect::<Vec<_>>().join(", ")
            ))
        })
    }
}

/// One of the wire formats a gateway can serve a model on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    Chat,
    Responses,
    Messages,
    Gemini,
}

/// How long a provider keeps a cached prompt prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum CacheTtl {
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GuardianProfile {
    Full,
    Minimal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scanner {
    Off,
    Pg2,
    Llm,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum AgentMode {
    #[serde(rename = "inherit")]
    Inherit,
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "dontAsk")]
    DontAsk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryBackend {
    Micromegas,
    Otlp,
}

#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub kind: AccountKind,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub models: Option<Vec<String>>,
    /// Per-model wire overrides, for kinds that route one account over several wires.
    pub wires: Option<BTreeMap<String, Wire>>,
    /// `ollama` only: the context window requested with every call (`options.num_ctx`).
    pub context_tokens: Option<u32>,
    /// `ollama` only: `false` asks a thinking model to answer without reasoning.
    pub think: Option<bool>,
    /// `anthropic` and `opencode-go` only: whether requests on the Messages wire carry cache
    /// breakpoints. Unset means the kind's default.
    pub prompt_caching: Option<bool>,
    /// `anthropic` and `opencode-go` only: the lifetime of a cached prefix; `"5m"` when unset.
    pub cache_ttl: Option<CacheTtl>,
    /// Each network key replaces the `[network]` value for this account only. Declared here
    /// rather than flattened from `NetworkSection`, since `deny_unknown_fields` and `flatten`
    /// do not combine.
    pub https_proxy: Option<String>,
    pub http_proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub ca_bundle: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

/// `[network]`: proxy, extra CA roots and a client certificate for the HTTP clients Fontaine
/// builds. Files are read when the config is loaded at startup, not when it is parsed.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    pub https_proxy: Option<String>,
    pub http_proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub ca_bundle: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

impl NetworkSection {
    pub fn settings(&self) -> NetworkSettings {
        NetworkSettings {
            https_proxy: self.https_proxy.clone(),
            http_proxy: self.http_proxy.clone(),
            no_proxy: self.no_proxy.clone(),
            ca_bundle: self.ca_bundle.clone(),
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
        }
    }
}

/// The window an `ollama` account requests when it sets no `context_tokens`: Ollama's own
/// default (4096) cannot hold the guardian's rules.
pub const DEFAULT_OLLAMA_CONTEXT_TOKENS: u32 = 16384;

/// The smallest `context_tokens` accepted; a guard against typos, not a size that fits the
/// guardian prompt.
const MIN_OLLAMA_CONTEXT_TOKENS: u32 = 2048;

impl Account {
    /// The network keys this account sets itself.
    pub fn network(&self) -> NetworkSettings {
        NetworkSettings {
            https_proxy: self.https_proxy.clone(),
            http_proxy: self.http_proxy.clone(),
            no_proxy: self.no_proxy.clone(),
            ca_bundle: self.ca_bundle.clone(),
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
        }
    }

    /// The context window an `ollama` call requests: the configured value or the default.
    pub fn ollama_context_tokens(&self) -> u32 {
        self.context_tokens.unwrap_or(DEFAULT_OLLAMA_CONTEXT_TOKENS)
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSection {
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuardianSection {
    pub model: Option<String>,
    pub profile: Option<GuardianProfile>,
    pub timeout_secs: Option<u64>,
    pub confidence_threshold: Option<f64>,
    pub cache: Option<bool>,
    pub scanner: Option<Scanner>,
    pub scanner_command: Option<String>,
    pub scanner_model_path: Option<String>,
    pub scanner_hf_token_env: Option<String>,
    pub screen: Option<GuardianScreenSection>,
}

/// `[guardian.screen]`: the fast typed-decision stage in front of the guardian.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuardianScreenSection {
    /// `<account>/<model>`, an account of kind `ollama`.
    pub model: Option<String>,
    pub question: Option<fontaine_guardian::Question>,
    pub stop_threshold: Option<f64>,
    pub allow_threshold: Option<f64>,
    pub steering_veto: Option<f64>,
    pub timeout_ms: Option<u64>,
    pub shadow: Option<bool>,
}

impl GuardianScreenSection {
    /// Whether the section does anything beyond asking for shadow mode, which can only tighten.
    fn loosens(&self) -> bool {
        self.model.is_some()
            || self.question.is_some()
            || self.stop_threshold.is_some()
            || self.allow_threshold.is_some()
            || self.steering_veto.is_some()
            || self.timeout_ms.is_some()
            || self.shadow == Some(false)
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionsSection {
    pub default_mode: Option<PermissionMode>,
    pub deny: Option<Vec<String>>,
    pub ask: Option<Vec<String>>,
    pub allow: Option<Vec<String>>,
    pub additional_directories: Option<Vec<String>>,
}

/// Where a hook or an MCP server came from, which decides whether it needs workspace trust and
/// whether its `allow` counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    pub slot: Slot,
    /// The scope the file was validated against grants user trust: the user file, `--settings`
    /// and an untracked `settings.local.toml` of a trusted root.
    pub user_trust: bool,
}

impl Origin {
    /// Defined by the repository, so it starts only after workspace trust.
    pub fn is_project(self) -> bool {
        matches!(self.slot, Slot::Project | Slot::ProjectLocal)
    }
}

impl Default for Origin {
    fn default() -> Self {
        Origin {
            slot: Slot::User,
            user_trust: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// In the permission-rule grammar. A hook without one runs for every call and counts as
    /// broad.
    pub matcher: Option<String>,
    pub command: String,
    pub timeout_secs: Option<u64>,
    #[serde(skip)]
    pub origin: Origin,
}

impl PartialEq for Hook {
    fn eq(&self, other: &Self) -> bool {
        self.matcher == other.matcher
            && self.command == other.command
            && self.timeout_secs == other.timeout_secs
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksSection {
    pub pre_tool_use: Option<Vec<Hook>>,
    pub post_tool_use: Option<Vec<Hook>>,
    pub permission_denied: Option<Vec<Hook>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_vars: Option<Vec<String>>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    pub servers: Option<BTreeMap<String, McpServer>>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoModeSection {
    pub environment: Option<Vec<String>>,
    pub allow: Option<Vec<String>>,
    pub soft_deny: Option<Vec<String>>,
    pub hard_deny: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentType {
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub mode: Option<AgentMode>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentsSection {
    pub max_parallel: Option<u32>,
    pub subagent_token_budget: Option<u64>,
    pub subagent_cost_budget_usd: Option<f64>,
    pub session_token_budget: Option<u64>,
    pub session_cost_budget_usd: Option<f64>,
    pub types: Option<BTreeMap<String, AgentType>>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextSection {
    pub autocompact_window: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSection {
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalSection {
    pub max_turns: Option<u32>,
    pub max_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellPreclassifierSection {
    pub allow_extra: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySection {
    pub enabled: Option<bool>,
    pub backend: Option<TelemetryBackend>,
    pub endpoint: Option<String>,
    pub api_key_env: Option<String>,
    pub traces: Option<bool>,
    pub log_user_prompts: Option<bool>,
    pub log_assistant_responses: Option<bool>,
    pub log_tool_details: Option<bool>,
    pub log_tool_content: Option<bool>,
    pub content_max_bytes: Option<u64>,
    pub resource_attributes: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WebsearchBackend {
    Tavily,
    Searxng,
    /// No client-side search tool, not even the default one.
    Off,
}

/// Whether the provider's own search is offered to the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NativeSearch {
    /// Where the provider supports it, on the provider's own API.
    #[default]
    Auto,
    /// Also on other hosts of a supported provider kind.
    On,
    Off,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebsearchSection {
    pub backend: Option<WebsearchBackend>,
    pub base_url: Option<String>,
    pub command: Option<Vec<String>>,
    pub startup_timeout_secs: Option<u64>,
    pub native: Option<NativeSearch>,
    pub api_key_env: Option<String>,
    pub max_results: Option<u32>,
    pub timeout_secs: Option<u64>,
}

/// A terminal colour theme, by name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    Dark,
    Light,
    DarkDaltonized,
    LightDaltonized,
    #[default]
    DarkAnsi,
    LightAnsi,
}

impl ThemeName {
    pub const ALL: [ThemeName; 6] = [
        ThemeName::DarkAnsi,
        ThemeName::LightAnsi,
        ThemeName::Dark,
        ThemeName::Light,
        ThemeName::DarkDaltonized,
        ThemeName::LightDaltonized,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeName::Dark => "dark",
            ThemeName::Light => "light",
            ThemeName::DarkDaltonized => "dark-daltonized",
            ThemeName::LightDaltonized => "light-daltonized",
            ThemeName::DarkAnsi => "dark-ansi",
            ThemeName::LightAnsi => "light-ansi",
        }
    }

    pub fn parse(name: &str) -> Option<ThemeName> {
        ThemeName::ALL.into_iter().find(|t| t.as_str() == name)
    }
}

/// `[tui]`: terminal preferences.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TuiSection {
    pub theme: Option<ThemeName>,
    pub screen_reader: Option<bool>,
}

/// One `[env]` value. Its `Debug` is redacted so a stray `{config:?}` cannot put a proxy password
/// in a log or panic message, and its `Deserialize` accepts only strings with a fixed error text
/// because toml would otherwise quote the offending value (`invalid type: integer `12345``).
#[derive(Clone, PartialEq, Eq, JsonSchema)]
#[schemars(with = "String")]
pub struct EnvValue(String);

impl EnvValue {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EnvValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvValue(<redacted>)")
    }
}

const ENV_VALUE_NOT_STRING: &str = "environment values must be strings";

impl<'de> Deserialize<'de> for EnvValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = EnvValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(ENV_VALUE_NOT_STRING)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<EnvValue, E> {
                Ok(EnvValue(v.to_string()))
            }

            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<EnvValue, E> {
                Ok(EnvValue(v))
            }

            // Every non-string visit falls back to the default impl, which formats the value
            // into the message; override the ones TOML can produce.
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<EnvValue, E> {
                Err(E::custom(ENV_VALUE_NOT_STRING))
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<EnvValue, E> {
                Err(E::custom(ENV_VALUE_NOT_STRING))
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<EnvValue, E> {
                Err(E::custom(ENV_VALUE_NOT_STRING))
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<EnvValue, E> {
                Err(E::custom(ENV_VALUE_NOT_STRING))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, _: A) -> Result<EnvValue, A::Error> {
                Err(serde::de::Error::custom(ENV_VALUE_NOT_STRING))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, _: A) -> Result<EnvValue, A::Error> {
                Err(serde::de::Error::custom(ENV_VALUE_NOT_STRING))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// One settings file, exactly as written.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub tui: Option<TuiSection>,
    pub accounts: Option<BTreeMap<String, Account>>,
    pub network: Option<NetworkSection>,
    pub model: Option<ModelSection>,
    pub guardian: Option<GuardianSection>,
    pub permissions: Option<PermissionsSection>,
    pub hooks: Option<HooksSection>,
    pub mcp: Option<McpSection>,
    pub auto_mode: Option<AutoModeSection>,
    pub agents: Option<AgentsSection>,
    pub context: Option<ContextSection>,
    pub schedule: Option<ScheduleSection>,
    pub goal: Option<GoalSection>,
    pub shell_preclassifier: Option<ShellPreclassifierSection>,
    pub telemetry: Option<TelemetrySection>,
    pub websearch: Option<WebsearchSection>,
    pub env: Option<BTreeMap<String, EnvValue>>,
}

/// Parses one settings file and runs every check that does not depend on the rest of the
/// environment: schema, per-scope key rules and value shapes. Phase-gated value rules and
/// env/credential resolution are left to [`load`].
pub fn parse_settings(text: &str, path: &Path, column: Column) -> Result<Settings, ConfigError> {
    parse_settings_with_hint(text, path, column, None).map(|(settings, _)| settings)
}

/// Schema and scope validation only; the entry point docs checks use.
pub fn validate_settings(text: &str, path: &Path, column: Column) -> Result<(), ConfigError> {
    parse_settings(text, path, column).map(|_| ())
}

fn parse_settings_with_hint(
    text: &str,
    path: &Path,
    column: Column,
    hint: Option<&str>,
) -> Result<(Settings, toml::Table), ConfigError> {
    // toml's `Display` quotes the offending source line, which for an `[env]` entry would echo
    // a secret; report the position and toml's bare message instead.
    let settings: Settings = toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        message: bare_toml_message(text, e.span(), e.message()),
    })?;
    let table: toml::Table = toml::from_str(text).expect("typed parse already accepted this text");
    if let Err((key, reason)) = check_scope(&table, column) {
        return Err(ConfigError::Scope {
            path: path.to_path_buf(),
            column,
            key,
            reason,
            hint: hint.map(str::to_string),
        });
    }
    validate_values(&settings, path)?;
    Ok((settings, table))
}

pub(crate) fn bare_toml_message(
    text: &str,
    span: Option<std::ops::Range<usize>>,
    message: &str,
) -> String {
    match span {
        Some(span) => {
            let (line, column) = line_col(text, span.start);
            format!("{line}:{column}: {message}")
        }
        None => message.to_string(),
    }
}

/// 1-based line and column of a byte offset.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text.as_bytes()[..offset.min(text.len())];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let column = before.len()
        - before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1)
        + 1;
    (line, column)
}

fn model_ref_ok(value: &str) -> bool {
    value
        .split_once('/')
        .is_some_and(|(account, model)| !account.is_empty() && !model.is_empty())
}

/// The shape rules for one account, shared by the settings loader and the provider dialog. The
/// error is the key under `accounts.` that is wrong, with what is wrong with it.
pub(crate) fn check_account(name: &str, account: &Account) -> Result<(), (String, String)> {
    if name.is_empty() || name.contains('/') {
        return Err((
            format!("accounts.{name}"),
            "account names are non-empty and contain no `/`".to_string(),
        ));
    }
    let provider = account.kind.provider();
    if account.wires.is_some() && !provider.accepts_wires() {
        return Err((
            format!("accounts.{name}.wires"),
            format!("not used by kind = \"{}\"", provider.name()),
        ));
    }
    for (key, set) in [
        ("prompt_caching", account.prompt_caching.is_some()),
        ("cache_ttl", account.cache_ttl.is_some()),
    ] {
        if set && !provider.accepts_prompt_caching() {
            return Err((
                format!("accounts.{name}.{key}"),
                format!("not used by kind = \"{}\"", provider.name()),
            ));
        }
    }
    for (key, set) in [
        ("context_tokens", account.context_tokens.is_some()),
        ("think", account.think.is_some()),
    ] {
        if set && !provider.accepts_model_options() {
            return Err((
                format!("accounts.{name}.{key}"),
                format!("not used by kind = \"{}\"", provider.name()),
            ));
        }
    }
    if let Some(tokens) = account.context_tokens
        && tokens < MIN_OLLAMA_CONTEXT_TOKENS
    {
        return Err((
            format!("accounts.{name}.context_tokens"),
            format!("must be at least {MIN_OLLAMA_CONTEXT_TOKENS}"),
        ));
    }
    check_network_shape(&account.network())
        .map_err(|(key, message)| (format!("accounts.{name}.{key}"), message))?;
    provider
        .validate(account)
        .map_err(|(key, message)| (format!("accounts.{name}.{key}"), message))
}

/// The first name that differs only by ASCII case from an earlier one, when `fold` is set.
fn case_colliding_env_name<'a>(
    names: impl Iterator<Item = &'a str>,
    fold: bool,
) -> Option<&'a str> {
    if !fold {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    names
        .into_iter()
        .find(|n| !seen.insert(n.to_ascii_uppercase()))
}

/// Value shape only; files and cross-key rules are checked by [`NetworkSettings::load`].
fn check_network_shape(n: &NetworkSettings) -> Result<(), (String, String)> {
    for (key, value) in [
        ("https_proxy", &n.https_proxy),
        ("http_proxy", &n.http_proxy),
        ("no_proxy", &n.no_proxy),
    ] {
        if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
            return Err((key.to_string(), "must not be empty".to_string()));
        }
    }
    for (key, value) in [
        ("ca_bundle", &n.ca_bundle),
        ("client_cert", &n.client_cert),
        ("client_key", &n.client_key),
    ] {
        if value.as_deref().is_some_and(|v| v.as_os_str().is_empty()) {
            return Err((key.to_string(), "must not be empty".to_string()));
        }
    }
    Ok(())
}

fn validate_values(s: &Settings, path: &Path) -> Result<(), ConfigError> {
    let bad = |key: &str, msg: &str| Err(ConfigError::invalid(path, key, msg));

    for (name, value) in s.env.iter().flatten() {
        let key = format!("env.{name}");
        if name.is_empty() || name.contains('=') || name.contains('\0') {
            return bad(
                &key,
                "names are non-empty and contain no `=` and no NUL character",
            );
        }
        if name
            .get(..ENV_RESERVED_PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(ENV_RESERVED_PREFIX))
        {
            return bad(&key, "names starting with `FONTAINE_` are reserved");
        }
        if value.as_str().contains('\0') {
            return bad(&key, "values contain no NUL character");
        }
    }

    if let Some(env) = &s.env
        && let Some(name) = case_colliding_env_name(env.keys().map(String::as_str), cfg!(windows))
    {
        return bad(
            &format!("env.{name}"),
            "names differ only by case, which Windows treats as the same variable",
        );
    }

    for (name, account) in s.accounts.iter().flatten() {
        if let Err((key, message)) = check_account(name, account) {
            return bad(&key, &message);
        }
    }
    if let Some(network) = &s.network
        && let Err((key, message)) = check_network_shape(&network.settings())
    {
        return bad(&format!("network.{key}"), &message);
    }
    if let Some(model) = s.model.as_ref().and_then(|m| m.model.as_deref())
        && !model_ref_ok(model)
    {
        return bad("model.model", "expected `<account>/<model>`");
    }
    if let Some(g) = &s.guardian {
        if let Some(model) = g.model.as_deref()
            && !model_ref_ok(model)
        {
            return bad("guardian.model", "expected `<account>/<model>`");
        }
        if g.timeout_secs == Some(0) {
            return bad("guardian.timeout_secs", "must be greater than zero");
        }
        if let Some(t) = g.confidence_threshold
            && !(0.0..=1.0).contains(&t)
        {
            return bad("guardian.confidence_threshold", "must be between 0 and 1");
        }
        if let Some(screen) = &g.screen {
            if let Some(model) = screen.model.as_deref()
                && !model_ref_ok(model)
            {
                return bad("guardian.screen.model", "expected `<account>/<model>`");
            }
            if let Some(t) = screen.stop_threshold
                && !(t > 0.0 && t <= 1.0)
            {
                return bad(
                    "guardian.screen.stop_threshold",
                    "must be greater than 0 and at most 1",
                );
            }
            if let Some(t) = screen.allow_threshold
                && !(t > 0.0 && t < 1.0)
            {
                return bad(
                    "guardian.screen.allow_threshold",
                    "must be greater than 0 and less than 1",
                );
            }
            if let Some(v) = screen.steering_veto
                && !(v > 0.0 && v <= 1.0)
            {
                return bad(
                    "guardian.screen.steering_veto",
                    "must be greater than 0 and at most 1",
                );
            }
            if screen.timeout_ms == Some(0) {
                return bad("guardian.screen.timeout_ms", "must be greater than zero");
            }
        }
    }
    if let Some(p) = &s.permissions {
        for (key, list) in [
            ("permissions.deny", &p.deny),
            ("permissions.ask", &p.ask),
            ("permissions.allow", &p.allow),
        ] {
            for text in list.iter().flatten() {
                parse_rule(text).map_err(|e| rule_error(path, key, e))?;
            }
        }
        if p.additional_directories
            .iter()
            .flatten()
            .any(|dir| dir.is_empty())
        {
            return bad(
                "permissions.additional_directories",
                "entries must be non-empty paths",
            );
        }
    }
    if let Some(h) = &s.hooks {
        for (event, hooks) in [
            ("pre_tool_use", &h.pre_tool_use),
            ("post_tool_use", &h.post_tool_use),
            ("permission_denied", &h.permission_denied),
        ] {
            for hook in hooks.iter().flatten() {
                let key = format!("hooks.{event}");
                if let Some(matcher) = &hook.matcher {
                    parse_rule(matcher).map_err(|e| rule_error(path, &key, e))?;
                }
                if hook.command.is_empty() {
                    return bad(&key, "`command` must not be empty");
                }
                if hook.timeout_secs == Some(0) {
                    return bad(&key, "`timeout_secs` must be greater than zero");
                }
            }
        }
    }
    for (name, server) in s.mcp.iter().flat_map(|m| m.servers.iter().flatten()) {
        let key = format!("mcp.servers.{name}");
        match (&server.command, &server.url) {
            (Some(command), None) => {
                if command.is_empty() {
                    return bad(&key, "`command` must not be empty");
                }
            }
            (None, Some(url)) => {
                if url.is_empty() {
                    return bad(&key, "`url` must not be empty");
                }
                if server.args.is_some() || server.env_vars.is_some() {
                    return bad(&key, "`args` and `env_vars` apply to stdio servers only");
                }
            }
            _ => return bad(&key, "set exactly one of `command` (stdio) or `url`"),
        }
        if name.is_empty()
            || name.contains("__")
            || name.starts_with('_')
            || name.ends_with('_')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return bad(
                &key,
                "server names are non-empty, contain no `__`, neither start nor end with `_`, and use only letters, digits, `_` and `-`",
            );
        }
    }
    if let Some(c) = &s.context
        && c.autocompact_window == Some(0)
    {
        return bad("context.autocompact_window", "must be at least 1");
    }
    if let Some(a) = &s.agents {
        if a.max_parallel == Some(0) {
            return bad("agents.max_parallel", "must be at least 1");
        }
        for (key, v) in [
            (
                "agents.subagent_cost_budget_usd",
                a.subagent_cost_budget_usd,
            ),
            ("agents.session_cost_budget_usd", a.session_cost_budget_usd),
        ] {
            if let Some(v) = v
                && !(v.is_finite() && v >= 0.0)
            {
                return bad(key, "must be a finite number, zero or more");
            }
        }
        for (name, ty) in a.types.iter().flatten() {
            let key = format!("agents.types.{name}");
            if let Some(model) = ty.model.as_deref()
                && !matches!(model, "$main" | "$guardian_default")
                && !model_ref_ok(model)
            {
                return bad(
                    &key,
                    "`model` is `$main`, `$guardian_default` or `<account>/<model>`",
                );
            }
            if let Some(tool) = ty
                .tools
                .iter()
                .flatten()
                .find(|t| !(is_builtin(t) || is_mcp(t) && validate_mcp_name(t).is_ok()))
            {
                return bad(&key, &format!("unknown tool `{tool}` in `tools`"));
            }
        }
    }
    if let Some(g) = &s.goal {
        if let Some(v) = g.max_turns
            && !(1..=MAX_GOAL_TURNS).contains(&v)
        {
            return bad(
                "goal.max_turns",
                &format!("must be between 1 and {MAX_GOAL_TURNS}"),
            );
        }
        if let Some(v) = g.max_cost_usd
            && !(v.is_finite() && v >= 0.0)
        {
            return bad("goal.max_cost_usd", "must be a finite number, zero or more");
        }
    }
    if let Some(sp) = &s.shell_preclassifier {
        for entry in sp.allow_extra.iter().flatten() {
            let words: Vec<_> = entry.split_whitespace().collect();
            if words.is_empty() || words.len() > 2 || words.iter().any(|w| w.starts_with('-')) {
                return bad(
                    "shell_preclassifier.allow_extra",
                    "entries are a command or a `command subcommand` pair with no flags",
                );
            }
        }
    }
    if let Some(t) = &s.telemetry
        && t.endpoint.as_deref() == Some("")
    {
        return bad("telemetry.endpoint", "must not be empty");
    }
    if let Some(w) = &s.websearch
        && let Err((key, message)) = check_websearch_values(w)
    {
        return bad(&format!("websearch.{key}"), &message);
    }
    Ok(())
}

pub const WEBSEARCH_MAX_RESULTS_LIMIT: u32 = 20;

fn check_websearch_values(w: &WebsearchSection) -> Result<(), (String, String)> {
    let err = |key: &str, message: &str| Err((key.to_string(), message.to_string()));
    if let Some(url) = w.base_url.as_deref() {
        let usable = url::Url::parse(url)
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host());
        if !usable {
            return err("base_url", "must be an http or https URL with a host");
        }
    }
    if let Some(command) = &w.command
        && (command.is_empty() || command.iter().any(String::is_empty))
    {
        return err("command", "must be a non-empty argv with no empty element");
    }
    if w.startup_timeout_secs == Some(0) {
        return err("startup_timeout_secs", "must be at least 1");
    }
    if w.timeout_secs == Some(0) {
        return err("timeout_secs", "must be at least 1");
    }
    if w.api_key_env.as_deref() == Some("") {
        return err("api_key_env", "must not be empty");
    }
    if let Some(n) = w.max_results
        && !(1..=WEBSEARCH_MAX_RESULTS_LIMIT).contains(&n)
    {
        return err(
            "max_results",
            &format!("must be between 1 and {WEBSEARCH_MAX_RESULTS_LIMIT}"),
        );
    }
    Ok(())
}

fn rule_error(path: &Path, key: &str, e: RuleError) -> ConfigError {
    ConfigError::invalid(path, key, e.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Guardian {
    pub model: Option<String>,
    pub profile: GuardianProfile,
    pub timeout_secs: u64,
    pub confidence_threshold: f64,
    pub cache: bool,
    pub scanner: Scanner,
    pub scanner_command: Option<String>,
    pub scanner_model_path: Option<String>,
    /// The variable holding the Hugging Face token. Always set, so the shell and hook
    /// environments are scrubbed of it whatever `scanner` is.
    pub scanner_hf_token_env: String,
    /// Set only by the user file; see [`GuardianScreen`].
    pub screen: Option<GuardianScreen>,
    /// A shared project file asked for shadow mode, which no other scope can undo.
    pub screen_shadow_forced: bool,
}

/// The merged `[guardian.screen]` stage.
#[derive(Clone, Debug, PartialEq)]
pub struct GuardianScreen {
    /// `<account>/<model>`.
    pub model: String,
    pub question: fontaine_guardian::Question,
    /// Absent only in shadow mode, where the raw score is all that is recorded.
    pub stop_threshold: Option<f64>,
    /// Unset: the screen never allows a call. In shadow mode the allow scores are still logged.
    pub allow_threshold: Option<f64>,
    pub steering_veto: f64,
    pub timeout_ms: u64,
    pub shadow: bool,
}

/// The screen's default time limit.
pub const DEFAULT_SCREEN_TIMEOUT_MS: u64 = 2_000;

/// The variable `scanner_hf_token_env` names when the user sets none.
pub const DEFAULT_HF_TOKEN_ENV: &str = "HF_TOKEN";

#[derive(Clone, Debug, PartialEq)]
pub struct Permissions {
    /// The mode a session starts in; unset leaves the choice to the surface (the interactive UI
    /// starts in `auto`, a headless run in `manual`).
    pub default_mode: Option<PermissionMode>,
    pub deny: Vec<SourcedRule>,
    pub ask: Vec<SourcedRule>,
    pub allow: Vec<SourcedRule>,
    pub additional_directories: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hooks {
    pub pre_tool_use: Vec<Hook>,
    pub post_tool_use: Vec<Hook>,
    pub permission_denied: Vec<Hook>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutoMode {
    pub environment: Vec<String>,
    pub allow: Vec<String>,
    pub soft_deny: Vec<String>,
    pub hard_deny: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Agents {
    pub max_parallel: u32,
    pub subagent_token_budget: u64,
    pub subagent_cost_budget_usd: f64,
    pub session_token_budget: Option<u64>,
    pub session_cost_budget_usd: Option<f64>,
    pub types: BTreeMap<String, AgentType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Telemetry {
    pub enabled: bool,
    pub backend: TelemetryBackend,
    pub endpoint: Option<String>,
    pub api_key_env: Option<String>,
    pub traces: bool,
    pub log_user_prompts: bool,
    pub log_assistant_responses: bool,
    pub log_tool_details: bool,
    pub log_tool_content: bool,
    pub content_max_bytes: u64,
    pub resource_attributes: BTreeMap<String, String>,
}

impl Telemetry {
    /// Whether any prompt, response or tool content may be recorded on spans.
    pub fn records_content(&self) -> bool {
        self.enabled
            && (self.log_user_prompts
                || self.log_assistant_responses
                || self.log_tool_details
                || self.log_tool_content)
    }
}

/// The merged `[websearch]` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Websearch {
    /// `None`: the default, a SearXNG container when a runtime is installed.
    pub backend: Option<WebsearchBackend>,
    pub base_url: Option<String>,
    pub command: Option<Vec<String>>,
    pub startup_timeout_secs: Option<u64>,
    pub native: NativeSearch,
    pub api_key_env: Option<String>,
    pub max_results: u32,
    pub timeout_secs: u64,
}

/// Fontaine's own variable namespace (`FONTAINE_SH_COMMAND`, `FONTAINE_PROJECT_DIR`).
const ENV_RESERVED_PREFIX: &str = "FONTAINE_";

/// Where a layer sits in the precedence order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Managed,
    Cli,
    ProjectLocal,
    Project,
    User,
}

impl Slot {
    pub fn label(self) -> &'static str {
        match self {
            Slot::Managed => "managed",
            Slot::Cli => "cli",
            Slot::ProjectLocal => "project-local",
            Slot::Project => "project",
            Slot::User => "user",
        }
    }
}

/// One entry of the resolved scope stack, highest precedence first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeEntry {
    pub slot: Slot,
    /// The column the layer was validated against; `None` for the reserved managed slot.
    pub column: Option<Column>,
    pub path: Option<PathBuf>,
    /// Whether a file was found and applied.
    pub loaded: bool,
}

impl fmt::Display for ScopeEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.slot.label())?;
        if let Some(column) = self.column {
            write!(f, "[{}]", column.label())?;
        }
        match (&self.path, self.loaded) {
            (Some(path), true) => write!(f, "={}", path.display()),
            (Some(path), false) => write!(f, "={} (absent)", path.display()),
            (None, _) => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// Tokens of conversation above which compaction runs, when lower than the model's own
    /// threshold.
    pub autocompact_window: Option<u64>,
}

pub const DEFAULT_GOAL_MAX_TURNS: u32 = 50;
const MAX_GOAL_TURNS: u32 = 1000;

/// The merged `[schedule]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    /// Whether the scheduling tools and `/loop` are available.
    pub enabled: bool,
}

/// The merged `[goal]` section: what bounds an unattended stretch of goal turns.
#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    /// Goal turns in one unattended stretch.
    pub max_turns: u32,
    /// Spend of one unattended stretch; unset is no cost bound.
    pub max_cost_usd: Option<f64>,
}

/// The merged `[tui]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Tui {
    pub theme: ThemeName,
    pub screen_reader: Option<bool>,
}

/// The merged, validated configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub workspace: PathBuf,
    pub scopes: Vec<ScopeEntry>,
    /// For each settings key a layer set, the layers that set it, highest precedence first.
    /// Tables add a segment; arrays (including arrays of tables such as `hooks.pre_tool_use`) and
    /// scalars are leaves, so `accounts.work.kind` and `env.HTTPS_PROXY` are leaves. A key no
    /// layer set is absent and comes from the built-in default.
    pub sources: BTreeMap<String, Vec<Slot>>,
    pub accounts: BTreeMap<String, Account>,
    /// Accounts defined by more than one scope, where the higher scope's definition won.
    pub shadowed_accounts: std::collections::BTreeSet<String>,
    /// The `[network]` section; an account's own keys are laid over it by [`Config::network_for`].
    pub network: NetworkSettings,
    pub model: Option<String>,
    pub guardian: Guardian,
    pub permissions: Permissions,
    pub hooks: Hooks,
    pub mcp_servers: BTreeMap<String, McpServer>,
    /// Where each server in `mcp_servers` was defined.
    pub mcp_origins: BTreeMap<String, Origin>,
    pub auto_mode: AutoMode,
    pub agents: Agents,
    pub context: Context,
    pub schedule: Schedule,
    pub goal: Goal,
    pub shell_preclassifier_allow_extra: Vec<String>,
    pub telemetry: Telemetry,
    pub websearch: Websearch,
    pub tui: Tui,
    /// `[env]`: variables set on every tool and extension subprocess.
    pub env: BTreeMap<String, EnvValue>,
}

impl Config {
    pub(crate) fn defaults(workspace: PathBuf) -> Self {
        Config {
            workspace,
            scopes: Vec::new(),
            sources: BTreeMap::new(),
            accounts: BTreeMap::new(),
            shadowed_accounts: Default::default(),
            network: NetworkSettings::default(),
            model: None,
            guardian: Guardian {
                model: None,
                profile: GuardianProfile::Full,
                timeout_secs: 30,
                confidence_threshold: 0.7,
                cache: true,
                scanner: Scanner::Off,
                scanner_command: None,
                scanner_model_path: None,
                scanner_hf_token_env: DEFAULT_HF_TOKEN_ENV.to_string(),
                screen: None,
                screen_shadow_forced: false,
            },
            permissions: Permissions {
                default_mode: None,
                deny: Vec::new(),
                ask: Vec::new(),
                allow: Vec::new(),
                additional_directories: Vec::new(),
            },
            hooks: Hooks::default(),
            mcp_servers: BTreeMap::new(),
            mcp_origins: BTreeMap::new(),
            auto_mode: AutoMode::default(),
            agents: Agents {
                max_parallel: 4,
                subagent_token_budget: 200_000,
                subagent_cost_budget_usd: 2.0,
                session_token_budget: None,
                session_cost_budget_usd: None,
                types: BTreeMap::new(),
            },
            context: Context::default(),
            schedule: Schedule { enabled: true },
            goal: Goal {
                max_turns: DEFAULT_GOAL_MAX_TURNS,
                max_cost_usd: None,
            },
            shell_preclassifier_allow_extra: Vec::new(),
            tui: Tui {
                theme: ThemeName::default(),
                screen_reader: None,
            },
            telemetry: Telemetry {
                enabled: false,
                backend: TelemetryBackend::Micromegas,
                endpoint: None,
                api_key_env: None,
                traces: true,
                log_user_prompts: false,
                log_assistant_responses: false,
                log_tool_details: false,
                log_tool_content: false,
                content_max_bytes: 61_440,
                resource_attributes: BTreeMap::new(),
            },
            websearch: Websearch {
                backend: None,
                base_url: None,
                command: None,
                startup_timeout_secs: None,
                native: NativeSearch::Auto,
                api_key_env: None,
                max_results: 10,
                timeout_secs: 30,
            },
            env: BTreeMap::new(),
        }
    }

    /// Applies one layer on top of what is already merged: scalars override, lists union,
    /// named table entries are replaced whole, map-valued keys merge per key.
    fn apply(&mut self, s: &Settings, slot: Slot, column: Column) {
        let origin = Origin {
            slot,
            user_trust: column != Column::Project,
        };
        for (name, account) in s.accounts.iter().flatten() {
            if self
                .accounts
                .insert(name.clone(), account.clone())
                .is_some()
            {
                self.shadowed_accounts.insert(name.clone());
            }
        }
        if let Some(network) = &s.network {
            self.network = self.network.overlaid_by(&network.settings());
        }
        if let Some(tui) = &s.tui {
            if let Some(v) = tui.theme {
                self.tui.theme = v;
            }
            if let Some(v) = tui.screen_reader {
                self.tui.screen_reader = Some(v);
            }
        }
        if let Some(model) = s.model.as_ref().and_then(|m| m.model.clone()) {
            self.model = Some(model);
        }
        if let Some(g) = &s.guardian {
            let t = &mut self.guardian;
            if let Some(v) = &g.model {
                t.model = Some(v.clone());
            }
            if let Some(v) = g.profile {
                t.profile = v;
            }
            if let Some(v) = g.timeout_secs {
                t.timeout_secs = v;
            }
            if let Some(v) = g.confidence_threshold {
                t.confidence_threshold = v;
            }
            if let Some(v) = g.cache {
                t.cache = v;
            }
            if let Some(v) = g.scanner {
                t.scanner = v;
            }
            if let Some(v) = &g.scanner_command {
                t.scanner_command = Some(v.clone());
            }
            if let Some(v) = &g.scanner_model_path {
                t.scanner_model_path = Some(v.clone());
            }
            if let Some(v) = &g.scanner_hf_token_env {
                t.scanner_hf_token_env = v.clone();
            }
            if let Some(screen) = &g.screen {
                if column != Column::User && screen.shadow == Some(true) {
                    t.screen_shadow_forced = true;
                }
                if screen.loosens() {
                    let merged = t.screen.get_or_insert_with(|| GuardianScreen {
                        model: String::new(),
                        question: fontaine_guardian::Question::default(),
                        stop_threshold: None,
                        allow_threshold: None,
                        steering_veto: fontaine_guardian::DEFAULT_STEERING_VETO,
                        timeout_ms: DEFAULT_SCREEN_TIMEOUT_MS,
                        shadow: true,
                    });
                    if let Some(v) = &screen.model {
                        merged.model = v.clone();
                    }
                    if let Some(v) = screen.question {
                        merged.question = v;
                    }
                    if let Some(v) = screen.stop_threshold {
                        merged.stop_threshold = Some(v);
                    }
                    if let Some(v) = screen.allow_threshold {
                        merged.allow_threshold = Some(v);
                    }
                    if let Some(v) = screen.steering_veto {
                        merged.steering_veto = v;
                    }
                    if let Some(v) = screen.timeout_ms {
                        merged.timeout_ms = v;
                    }
                    if let Some(v) = screen.shadow {
                        merged.shadow = v;
                    }
                }
            }
        }
        if let Some(p) = &s.permissions {
            let t = &mut self.permissions;
            if let Some(v) = p.default_mode {
                t.default_mode = Some(v);
            }
            let source = RuleSource::Config(slot);
            union_rules(&mut t.deny, p.deny.as_deref(), source);
            union_rules(&mut t.ask, p.ask.as_deref(), source);
            union_rules(&mut t.allow, p.allow.as_deref(), source);
            union(
                &mut t.additional_directories,
                p.additional_directories.as_deref(),
            );
        }
        if let Some(h) = &s.hooks {
            union_hooks(
                &mut self.hooks.pre_tool_use,
                h.pre_tool_use.as_deref(),
                origin,
            );
            union_hooks(
                &mut self.hooks.post_tool_use,
                h.post_tool_use.as_deref(),
                origin,
            );
            union_hooks(
                &mut self.hooks.permission_denied,
                h.permission_denied.as_deref(),
                origin,
            );
        }
        for (name, server) in s.mcp.iter().flat_map(|m| m.servers.iter().flatten()) {
            self.mcp_servers.insert(name.clone(), server.clone());
            self.mcp_origins.insert(name.clone(), origin);
        }
        if let Some(a) = &s.auto_mode {
            let t = &mut self.auto_mode;
            union(&mut t.environment, a.environment.as_deref());
            union(&mut t.allow, a.allow.as_deref());
            union(&mut t.soft_deny, a.soft_deny.as_deref());
            union(&mut t.hard_deny, a.hard_deny.as_deref());
        }
        if let Some(a) = &s.agents {
            let t = &mut self.agents;
            if let Some(v) = a.max_parallel {
                t.max_parallel = v;
            }
            if let Some(v) = a.subagent_token_budget {
                t.subagent_token_budget = v;
            }
            if let Some(v) = a.subagent_cost_budget_usd {
                t.subagent_cost_budget_usd = v;
            }
            if let Some(v) = a.session_token_budget {
                t.session_token_budget = Some(v);
            }
            if let Some(v) = a.session_cost_budget_usd {
                t.session_cost_budget_usd = Some(v);
            }
            for (name, ty) in a.types.iter().flatten() {
                t.types.insert(name.clone(), ty.clone());
            }
        }
        if let Some(v) = s.context.as_ref().and_then(|c| c.autocompact_window) {
            self.context.autocompact_window = Some(v);
        }
        if let Some(v) = s.schedule.as_ref().and_then(|c| c.enabled) {
            self.schedule.enabled = v;
        }
        if let Some(g) = &s.goal {
            if let Some(v) = g.max_turns {
                self.goal.max_turns = v;
            }
            if let Some(v) = g.max_cost_usd {
                self.goal.max_cost_usd = Some(v);
            }
        }
        if let Some(sp) = &s.shell_preclassifier {
            union(
                &mut self.shell_preclassifier_allow_extra,
                sp.allow_extra.as_deref(),
            );
        }
        if let Some(tel) = &s.telemetry {
            let t = &mut self.telemetry;
            if let Some(v) = tel.enabled {
                t.enabled = v;
            }
            if let Some(v) = tel.backend {
                t.backend = v;
            }
            if let Some(v) = &tel.endpoint {
                t.endpoint = Some(v.clone());
            }
            if let Some(v) = &tel.api_key_env {
                t.api_key_env = Some(v.clone());
            }
            if let Some(v) = tel.traces {
                t.traces = v;
            }
            if let Some(v) = tel.log_user_prompts {
                t.log_user_prompts = v;
            }
            if let Some(v) = tel.log_assistant_responses {
                t.log_assistant_responses = v;
            }
            if let Some(v) = tel.log_tool_details {
                t.log_tool_details = v;
            }
            if let Some(v) = tel.log_tool_content {
                t.log_tool_content = v;
            }
            if let Some(v) = tel.content_max_bytes {
                t.content_max_bytes = v;
            }
            for (k, v) in tel.resource_attributes.iter().flatten() {
                t.resource_attributes.insert(k.clone(), v.clone());
            }
        }
        if let Some(web) = &s.websearch {
            let w = &mut self.websearch;
            if let Some(v) = web.backend {
                w.backend = Some(v);
            }
            if let Some(v) = &web.base_url {
                w.base_url = Some(v.clone());
            }
            if let Some(v) = &web.command {
                w.command = Some(v.clone());
            }
            if let Some(v) = web.startup_timeout_secs {
                w.startup_timeout_secs = Some(v);
            }
            if let Some(v) = web.native {
                w.native = v;
            }
            if let Some(v) = &web.api_key_env {
                w.api_key_env = Some(v.clone());
            }
            if let Some(v) = web.max_results {
                w.max_results = v;
            }
            if let Some(v) = web.timeout_secs {
                w.timeout_secs = v;
            }
        }
        for (k, v) in s.env.iter().flatten() {
            // Windows names are case-insensitive: `Path` from a higher scope replaces `PATH`.
            if cfg!(windows) {
                self.env
                    .retain(|existing, _| !existing.eq_ignore_ascii_case(k));
            }
            self.env.insert(k.clone(), v.clone());
        }
    }
}

fn union<T: PartialEq + Clone>(target: &mut Vec<T>, items: Option<&[T]>) {
    for item in items.unwrap_or_default() {
        if !target.contains(item) {
            target.push(item.clone());
        }
    }
}

fn union_hooks(target: &mut Vec<Hook>, items: Option<&[Hook]>, origin: Origin) {
    // The same hook defined in several scopes is kept once, under the origin with more trust.
    let rank = |o: Origin| (o.user_trust, !o.is_project());
    for item in items.unwrap_or_default() {
        match target.iter_mut().find(|kept| *kept == item) {
            Some(kept) => {
                if rank(origin) > rank(kept.origin) {
                    kept.origin = origin;
                }
            }
            None => target.push(Hook {
                origin,
                ..item.clone()
            }),
        }
    }
}

fn union_rules(target: &mut Vec<SourcedRule>, items: Option<&[String]>, source: RuleSource) {
    for text in items.unwrap_or_default() {
        // `validate_values` already parsed every rule of every layer before any is applied.
        let rule = SourcedRule::new(
            parse_rule(text).expect("rule validated before merge"),
            source,
        );
        if !target.contains(&rule) {
            target.push(rule);
        }
    }
}

/// Looks up an environment variable by name; only the binary reads the real environment.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Permission rules given on the command line (`--allowed-tools`, `--disallowed-tools`). They
/// load as the CLI scope's `permissions.allow` and `permissions.deny`.
#[derive(Clone, Copy, Debug, Default)]
pub struct CliRules<'a> {
    pub allow: &'a [String],
    pub deny: &'a [String],
}

pub struct LoadParams<'a> {
    pub launch_dir: &'a Path,
    pub config_dir: &'a Path,
    pub data_dir: &'a Path,
    pub settings_file: Option<&'a Path>,
    pub cli_rules: CliRules<'a>,
    pub env: EnvLookup<'a>,
}

pub const USER_FILE: &str = "config.toml";
pub const PROJECT_DIR: &str = ".fontaine";
pub const PROJECT_FILE: &str = "settings.toml";
pub const PROJECT_LOCAL_FILE: &str = "settings.local.toml";

struct Layer {
    slot: Slot,
    column: Column,
    path: PathBuf,
    settings: Option<Settings>,
    /// The layer's text as parsed, for provenance.
    table: Option<toml::Table>,
}

/// Every settings key path the table holds. Tables add a segment; arrays (including arrays of
/// tables) and scalars are leaves.
fn leaf_paths(table: &toml::Table) -> Vec<String> {
    fn walk(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
        for (key, value) in table {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match value {
                toml::Value::Table(inner) => walk(inner, &path, out),
                _ => out.push(path),
            }
        }
    }
    let mut out = Vec::new();
    walk(table, "", &mut out);
    out
}

/// For each leaf path any layer holds, the slots of the layers that hold it, highest precedence
/// first. `layers` must already be in that order.
fn collect_sources<'a>(
    layers: impl IntoIterator<Item = (Slot, &'a toml::Table)>,
) -> BTreeMap<String, Vec<Slot>> {
    let mut sources: BTreeMap<String, Vec<Slot>> = BTreeMap::new();
    for (slot, table) in layers {
        for path in leaf_paths(table) {
            sources.entry(path).or_default().push(slot);
        }
    }
    sources
}

pub fn load(params: &LoadParams<'_>) -> Result<Config, ConfigError> {
    let workspace =
        std::fs::canonicalize(params.launch_dir).map_err(|source| ConfigError::Read {
            path: params.launch_dir.to_path_buf(),
            source,
        })?;

    let user_path = params.config_dir.join(USER_FILE);
    let user = read_optional(&user_path, crate::file_lock::read_shared)?
        .map(|text| parse_settings_with_hint(&text, &user_path, Column::User, None))
        .transpose()?;

    let project_path = workspace.join(PROJECT_DIR).join(PROJECT_FILE);
    let project = read_optional(&project_path, |p| std::fs::read_to_string(p))?
        .map(|text| parse_settings_with_hint(&text, &project_path, Column::Project, None))
        .transpose()?;

    let local_path = workspace.join(PROJECT_DIR).join(PROJECT_LOCAL_FILE);
    let (local_column, local) = match read_optional(&local_path, |p| std::fs::read_to_string(p))? {
        None => (Column::ProjectLocal, None),
        Some(text) => {
            let classification =
                trust::classify_project_local(&workspace, &local_path, params.data_dir);
            let (column, hint) = match &classification {
                LocalClassification::UserTrust => (Column::ProjectLocal, None),
                LocalClassification::Project(why) => {
                    (Column::Project, demotion_hint(why, params.data_dir))
                }
            };
            let settings = parse_settings_with_hint(&text, &local_path, column, hint.as_deref())?;
            (column, Some(settings))
        }
    };

    let cli_path = params.settings_file;
    let cli = match cli_path {
        None => None,
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            Some(parse_settings_with_hint(&text, path, Column::Cli, None)?)
        }
    };
    let flag_rules = !params.cli_rules.allow.is_empty() || !params.cli_rules.deny.is_empty();
    let cli = if flag_rules {
        let (mut settings, mut table) = cli.unwrap_or_default();
        let permissions = settings.permissions.get_or_insert_with(Default::default);
        for (flag, key, from, into) in [
            (
                "--allowed-tools",
                "allow",
                params.cli_rules.allow,
                &mut permissions.allow,
            ),
            (
                "--disallowed-tools",
                "deny",
                params.cli_rules.deny,
                &mut permissions.deny,
            ),
        ] {
            for text in from {
                parse_rule(text).map_err(|e| ConfigError::usage(format!("{flag}: {e}")))?;
                into.get_or_insert_with(Vec::new).push(text.clone());
                table
                    .entry("permissions")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .expect("a CLI settings file's `permissions` already parsed as a table")
                    .entry(key)
                    .or_insert_with(|| toml::Value::Array(Vec::new()))
                    .as_array_mut()
                    .expect("a CLI settings file's rule list already parsed as an array")
                    .push(toml::Value::String(text.clone()));
            }
        }
        Some((settings, table))
    } else {
        cli
    };

    let (cli, cli_table): (Option<Settings>, Option<toml::Table>) = cli.unzip();
    let (local, local_table): (Option<Settings>, Option<toml::Table>) = local.unzip();
    let (project, project_table): (Option<Settings>, Option<toml::Table>) = project.unzip();
    let (user, user_table): (Option<Settings>, Option<toml::Table>) = user.unzip();
    let layers = [
        Layer {
            slot: Slot::Cli,
            column: Column::Cli,
            path: cli_path.map(Path::to_path_buf).unwrap_or_default(),
            settings: cli,
            table: cli_table,
        },
        Layer {
            slot: Slot::ProjectLocal,
            column: local_column,
            path: local_path,
            settings: local,
            table: local_table,
        },
        Layer {
            slot: Slot::Project,
            column: Column::Project,
            path: project_path,
            settings: project,
            table: project_table,
        },
        Layer {
            slot: Slot::User,
            column: Column::User,
            path: user_path.clone(),
            settings: user,
            table: user_table,
        },
    ];

    let mut config = Config::defaults(workspace);
    config.scopes.push(ScopeEntry {
        slot: Slot::Managed,
        column: None,
        path: None,
        loaded: false,
    });
    for layer in &layers {
        let flags_only = layer.slot == Slot::Cli && cli_path.is_none();
        if flags_only && !flag_rules {
            continue;
        }
        config.scopes.push(ScopeEntry {
            slot: layer.slot,
            column: Some(layer.column),
            path: (!flags_only).then(|| layer.path.clone()),
            loaded: layer.settings.is_some(),
        });
    }

    for layer in layers.iter().rev() {
        let Some(settings) = &layer.settings else {
            continue;
        };
        config.apply(settings, layer.slot, layer.column);
    }
    config.sources = collect_sources(
        layers
            .iter()
            .filter_map(|l| l.table.as_ref().map(|t| (l.slot, t))),
    );

    check_network(&config)?;
    check_scanner(&config.guardian, &user_path)?;
    check_screen(&mut config, &user_path)?;
    resolve_telemetry(&config.telemetry, params)?;
    check_websearch(&config.websearch)?;
    config.websearch_config(params.env, params.data_dir)?;
    Ok(config)
}

/// Loads the network settings of `[network]` and of every account that overrides them, so a bad
/// file or an inconsistent combination stops startup instead of the first request.
fn check_network(config: &Config) -> Result<(), ConfigError> {
    let overriding = config
        .accounts
        .iter()
        .filter(|(_, a)| a.network() != NetworkSettings::default())
        .map(|(name, _)| (format!("accounts.{name}"), Some(name.as_str())));
    for (key, account) in std::iter::once(("network".to_string(), None)).chain(overriding) {
        config
            .loaded_network(account)
            .map_err(|source| ConfigError::Network { key, source })?;
    }
    Ok(())
}

impl Config {
    /// The network settings for one account: `[network]` with the account's own keys laid over
    /// it. An account not in config (or `None`) gets `[network]` alone.
    pub fn network_for(&self, account: Option<&str>) -> NetworkSettings {
        match account.and_then(|name| self.accounts.get(name)) {
            Some(account) => self.network.overlaid_by(&account.network()),
            None => self.network.clone(),
        }
    }

    pub fn loaded_network(&self, account: Option<&str>) -> Result<LoadedNetwork, NetworkError> {
        self.network_for(account).load()
    }
}

/// Scanner settings come only from the user file, so that is where a bad combination is
/// reported. Kept out of `validate_values` so the examples in the docs load under any feature set.
fn check_scanner(guardian: &Guardian, user_path: &Path) -> Result<(), ConfigError> {
    let bad = |key: &str, message: &str| Err(ConfigError::invalid(user_path, key, message));
    match guardian.scanner {
        Scanner::Command
            if guardian
                .scanner_command
                .as_deref()
                .is_none_or(str::is_empty) =>
        {
            bad(
                "guardian.scanner_command",
                "`scanner = \"command\"` requires `scanner_command`",
            )
        }
        Scanner::Pg2 if !cfg!(feature = "pg2") => bad(
            "guardian.scanner",
            "`scanner = \"pg2\"` needs a build with the `pg2` feature; release binaries include it, \
             a source build needs `--features pg2`",
        ),
        _ => Ok(()),
    }
}

/// The screen settings come only from the user file, so that is where an incomplete stage is
/// reported. It must name a model on an Ollama account, and it stops calls only with a threshold
/// the user chose.
fn check_screen(config: &mut Config, user_path: &Path) -> Result<(), ConfigError> {
    let forced = config.guardian.screen_shadow_forced;
    let Some(screen) = &mut config.guardian.screen else {
        return Ok(());
    };
    let bad = |key: &str, message: &str| Err(ConfigError::invalid(user_path, key, message));
    if screen.model.is_empty() {
        return bad("guardian.screen.model", "the screen stage requires `model`");
    }
    let account = screen.model.split_once('/').map_or("", |(a, _)| a);
    match config.accounts.get(account) {
        Some(a) if a.kind.as_str() == "ollama" => {
            if let Some(base) = a.base_url.as_deref().filter(|u| !u.is_empty()) {
                let usable = url::Url::parse(base)
                    .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host());
                if !usable {
                    return bad(
                        &format!("accounts.{account}.base_url"),
                        "the screen stage requires an http or https URL with a host",
                    );
                }
            }
        }
        Some(_) => {
            return bad(
                "guardian.screen.model",
                &format!("account `{account}` is not of kind = \"ollama\""),
            );
        }
        None => {
            return bad(
                "guardian.screen.model",
                &format!("account `{account}` is not configured"),
            );
        }
    }
    if forced {
        screen.shadow = true;
    }
    if !screen.shadow && screen.stop_threshold.is_none() && screen.allow_threshold.is_none() {
        return bad(
            "guardian.screen.stop_threshold",
            "there is no default; set `stop_threshold` and/or `allow_threshold`, or keep `shadow = true` to log scores first",
        );
    }
    Ok(())
}

fn resolve_telemetry(t: &Telemetry, params: &LoadParams<'_>) -> Result<(), ConfigError> {
    if !t.enabled {
        return Ok(());
    }
    let fail = |message: &str| {
        Err(ConfigError::Telemetry {
            message: message.to_string(),
        })
    };
    if t.endpoint.as_deref().is_none_or(str::is_empty) {
        return fail("`enabled = true` requires `endpoint`");
    }
    match &t.api_key_env {
        Some(var) => {
            let set = (params.env)(var).is_some_and(|v| !v.is_empty());
            if !set {
                return fail(&format!(
                    "`enabled = true` requires an ingestion key, but `api_key_env` names {var}, which is unset or empty"
                ));
            }
        }
        None => {
            let stored = credentials::Credentials::load(params.data_dir)?;
            if stored.telemetry().is_none() {
                return fail(&format!(
                    "`enabled = true` requires an ingestion key: set `api_key_env` or store one in {}",
                    credentials::credentials_path(params.data_dir).display()
                ));
            }
        }
    }
    Ok(())
}

/// The cross-key rules of `[websearch]`, checked on the merged section since its keys may come
/// from different files. It is user-or-CLI only, so no file path is named.
fn check_websearch(w: &Websearch) -> Result<(), ConfigError> {
    let fail = |message: &str| {
        Err(ConfigError::Websearch {
            message: message.to_string(),
        })
    };
    match w.backend {
        None | Some(WebsearchBackend::Off) => {
            if w.base_url.is_some() || w.command.is_some() || w.api_key_env.is_some() {
                return fail(
                    "`base_url`, `command` and `api_key_env` need `backend = \"tavily\"` or `\"searxng\"`",
                );
            }
        }
        Some(WebsearchBackend::Tavily) => {
            if w.base_url.is_some() {
                return fail("`base_url` is only for `backend = \"searxng\"`");
            }
            if w.command.is_some() {
                return fail("`command` is only for `backend = \"searxng\"`");
            }
        }
        Some(WebsearchBackend::Searxng) => {
            if w.base_url.is_none() {
                return fail("`backend = \"searxng\"` requires `base_url`");
            }
            if w.api_key_env.is_some() {
                return fail("`api_key_env` is only for `backend = \"tavily\"`");
            }
        }
    }
    if w.startup_timeout_secs.is_some() && w.command.is_none() {
        return fail("`startup_timeout_secs` needs `command`");
    }
    Ok(())
}

/// The key a `tavily` backend uses: the variable `api_key_env` names, else the stored
/// credential. Config loading and tool construction both resolve through here, so they cannot
/// disagree.
fn websearch_key(
    w: &Websearch,
    env: EnvLookup<'_>,
    data_dir: &Path,
) -> Result<fontaine_tools::websearch::ApiKey, ConfigError> {
    let fail = |message: String| Err(ConfigError::Websearch { message });
    let key = match &w.api_key_env {
        Some(var) => match env(var).and_then(|v| v.into_string().ok()) {
            Some(key) if !key.is_empty() => key,
            _ => {
                return fail(format!(
                    "`backend = \"tavily\"` requires an API key, but `api_key_env` names {var}, which is unset or empty"
                ));
            }
        },
        None => {
            let stored = credentials::Credentials::load(data_dir)?;
            match stored.websearch() {
                Some(key) => key.to_string(),
                None => {
                    return fail(format!(
                        "`backend = \"tavily\"` requires an API key: set `api_key_env` or store one under [websearch] in {}",
                        credentials::credentials_path(data_dir).display()
                    ));
                }
            }
        }
    };
    Ok(fontaine_tools::websearch::ApiKey::new(key))
}

impl Config {
    /// The client-side search tool's settings; `None` when search is switched off, or no backend
    /// is configured and no container runtime is installed. Starting the default container is announced
    /// only through [`Config::websearch_config_with_notice`].
    pub fn websearch_config(
        &self,
        env: EnvLookup<'_>,
        data_dir: &Path,
    ) -> Result<Option<fontaine_tools::websearch::WebsearchConfig>, ConfigError> {
        self.websearch_config_with_notice(env, data_dir, std::sync::Arc::new(|_| {}))
    }

    /// [`Config::websearch_config`], with `notice` told when the default container is started.
    pub fn websearch_config_with_notice(
        &self,
        env: EnvLookup<'_>,
        data_dir: &Path,
        notice: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Option<fontaine_tools::websearch::WebsearchConfig>, ConfigError> {
        use fontaine_tools::websearch::{BackendConfig, Launch, WebsearchConfig};
        let w = &self.websearch;
        let backend = match w.backend {
            Some(WebsearchBackend::Off) => return Ok(None),
            None => {
                let Some(runtime) = self.default_search_runtime(env) else {
                    return Ok(None);
                };
                let managed = crate::managed_search::managed(
                    runtime,
                    data_dir.to_path_buf(),
                    fontaine_tools::child_env::container_runtime_env(env),
                    notice,
                );
                let mut config = WebsearchConfig::new(BackendConfig::Managed(managed));
                config.max_results = w.max_results as usize;
                config.timeout = std::time::Duration::from_secs(w.timeout_secs);
                return Ok(Some(config));
            }
            Some(backend) => backend,
        };
        let backend = match backend {
            WebsearchBackend::Off => unreachable!("returned above"),
            WebsearchBackend::Tavily => BackendConfig::tavily(websearch_key(w, env, data_dir)?),
            WebsearchBackend::Searxng => BackendConfig::Searxng {
                base_url: url::Url::parse(
                    w.base_url
                        .as_deref()
                        .expect("a searxng backend without base_url is rejected at load"),
                )
                .expect("base_url is validated as a URL at load"),
                launch: w.command.as_ref().map(|argv| Launch {
                    argv: argv.clone(),
                    startup_timeout: w.startup_timeout_secs.map_or(
                        fontaine_tools::websearch::DEFAULT_STARTUP_TIMEOUT,
                        std::time::Duration::from_secs,
                    ),
                    env: fontaine_tools::child_env::base_env(env),
                }),
            },
        };
        let mut config = WebsearchConfig::new(backend);
        config.max_results = w.max_results as usize;
        config.timeout = std::time::Duration::from_secs(w.timeout_secs);
        Ok(Some(config))
    }
}

impl Config {
    /// The container runtime the default search backend would use: only when no `backend` is set.
    pub fn default_search_runtime(&self, env: EnvLookup<'_>) -> Option<std::path::PathBuf> {
        if self.websearch.backend.is_some() {
            return None;
        }
        crate::managed_search::find_runtime(env("PATH").as_deref())
    }
}

fn demotion_hint(why: &ProjectDemotion, data_dir: &Path) -> Option<String> {
    match why {
        ProjectDemotion::UntrustedRoot(root) => Some(format!(
            "the workspace root {} is not trusted, so this file loads as project settings; \
             to trust it, add this entry to {}:\n\n[[trusted]]\nroot = {}",
            root.display(),
            trust::trust_path(data_dir).display(),
            trust::toml_literal(&root.to_string_lossy()),
        )),
        ProjectDemotion::UnreadableTrustStore => Some(format!(
            "{} is unreadable or malformed, so no workspace is trusted and this file loads as \
             project settings; fix or remove it, then trust the workspace again",
            trust::trust_path(data_dir).display(),
        )),
        _ => None,
    }
}

fn read_optional(
    path: &Path,
    read: fn(&Path) -> io::Result<String>,
) -> Result<Option<String>, ConfigError> {
    match read(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::case_colliding_env_name;

    #[test]
    fn case_collisions_are_found_only_when_folding() {
        let names = ["PATH", "HOME", "Path"];
        assert_eq!(
            case_colliding_env_name(names.into_iter(), true),
            Some("Path")
        );
        assert_eq!(case_colliding_env_name(names.into_iter(), false), None);
        assert_eq!(case_colliding_env_name(["A", "B"].into_iter(), true), None);
    }
}
