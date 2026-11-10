use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::config_scope::check_scope;
use crate::credentials;
use crate::effort::Effort;
use crate::permission_rules::{RuleError, RuleSource, SourcedRule, parse_rule, validate_mcp_name};
use crate::providers::{self, Provider};
use crate::trust::{self, LocalClassification, ProjectDemotion};
use fontaine_agents::registry::ModelSpec;
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
    /// `auto` with nobody to ask.
    #[serde(rename = "afk")]
    Afk,
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
            PermissionMode::Afk => "afk",
        }
    }

    /// The mode whose rules decide what runs. AFK decides as `auto` does; what it changes is that
    /// nobody is asked.
    pub fn rules(self) -> PermissionMode {
        match self {
            PermissionMode::Afk => PermissionMode::Auto,
            mode => mode,
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

/// The quota endpoint shape an account's provider speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum QuotaKind {
    Kimi,
}

/// How an `anthropic` account sends its credential.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum AnthropicAuth {
    #[default]
    #[serde(rename = "x-api-key")]
    XApiKey,
    #[serde(rename = "bearer")]
    Bearer,
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
    /// `anthropic`, `openrouter` and `opencode-go` only: whether requests carry cache
    /// breakpoints. Unset means the kind's default.
    pub prompt_caching: Option<bool>,
    /// `anthropic`, `openrouter` and `opencode-go` only: the lifetime of a cached prefix; `"5m"`
    /// when unset.
    pub cache_ttl: Option<CacheTtl>,
    /// `openai-compatible` only: where to read the provider's own subscription quota for the
    /// status bar. Sends the account's key to `<base_url>/usages`.
    pub quota: Option<QuotaKind>,
    /// `anthropic` only: `"bearer"` sends the credential as `Authorization: Bearer` instead of
    /// `x-api-key`, for gateways that accept only that. `"x-api-key"` when unset.
    pub auth: Option<AnthropicAuth>,
    /// `claude-subscription` only: the official `claude` executable; `claude` on `PATH` when
    /// unset.
    pub cli_path: Option<PathBuf>,
    /// `claude-subscription` only: the CLI's own config directory, handed to it as
    /// `CLAUDE_CONFIG_DIR` so two accounts can keep separate logins.
    pub claude_config_dir: Option<PathBuf>,
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
    /// An account of `kind` with every other setting unset.
    pub fn new(kind: AccountKind, base_url: Option<String>) -> Account {
        Account {
            kind,
            api_key_env: None,
            base_url,
            models: None,
            wires: None,
            context_tokens: None,
            think: None,
            prompt_caching: None,
            cache_ttl: None,
            quota: None,
            auth: None,
            cli_path: None,
            claude_config_dir: None,
            https_proxy: None,
            http_proxy: None,
            no_proxy: None,
            ca_bundle: None,
            client_cert: None,
            client_key: None,
        }
    }

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

/// A model setting that takes one entry or an ordered list of them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn entries(&self) -> &[String] {
        match self {
            OneOrMany::One(one) => std::slice::from_ref(one),
            OneOrMany::Many(many) => many,
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self, OneOrMany::Many(_))
    }

    /// The text when this is one string.
    pub fn single(&self) -> Option<&str> {
        match self {
            OneOrMany::One(one) => Some(one),
            OneOrMany::Many(_) => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSection {
    /// `<account>/<model>`, a role name, or an ordered list of them: shorthand for the `main`
    /// role.
    pub model: Option<OneOrMany>,
    pub effort: Option<Effort>,
    /// The output cap a main or subagent turn sends, lowered to the model's own limit.
    pub max_output_tokens: Option<u32>,
}

/// `[models]`: the user's model roles.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelsSection {
    /// Role name to an ordered list of `<account>/<model>` or other role names.
    pub roles: Option<BTreeMap<String, Vec<String>>>,
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
    pub composition: Option<GuardianCompositionSection>,
}

/// `[guardian.composition]`: how the guardian is composed from the LLM guardian and the local
/// logprob reads, as a built-in preset or a Rune script.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuardianCompositionSection {
    /// A built-in preset; exactly one of `preset` and `script`.
    pub preset: Option<String>,
    /// A Rune file; a relative path resolves against the user config directory.
    pub script: Option<String>,
    /// `<account>/<model>`, an account of kind `ollama` (a local model, read from its logprobs)
    /// or `openrouter` (a hosted `/v1/systemone` model such as `typesafe/jev-1.14`): the default
    /// model of every read.
    pub model: Option<String>,
    pub shadow: Option<bool>,
    /// Bounds each read.
    pub timeout_ms: Option<u64>,
    /// Preset parameters, or the values passed to a script.
    pub params: Option<BTreeMap<String, serde_json::Value>>,
    /// The reads a script declares; a preset declares its own.
    pub reads: Option<BTreeMap<String, CompositionReadSection>>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompositionReadSection {
    /// `<account>/<model>`; defaults to the composition's `model`.
    pub model: Option<String>,
    pub prompt: Option<fontaine_guardian::Prompt>,
    pub question: Option<fontaine_guardian::Question>,
}

impl GuardianCompositionSection {
    /// Whether the section does anything beyond asking for shadow mode, which can only tighten.
    fn loosens(&self) -> bool {
        self.preset.is_some()
            || self.script.is_some()
            || self.model.is_some()
            || self.timeout_ms.is_some()
            || self.params.is_some()
            || self.reads.is_some()
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
    pub auto_tracked_edits: Option<bool>,
    pub auto_scratch_writes: Option<bool>,
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

/// The kind of handler a hook runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandlerKind {
    /// A shell command that gets the event on stdin.
    #[default]
    Command,
    /// A POST of the event to a URL.
    Http,
    /// One question to the guardian's model.
    Prompt,
}

impl HandlerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HandlerKind::Command => "command",
            HandlerKind::Http => "http",
            HandlerKind::Prompt => "prompt",
        }
    }
}

/// What a hook's `matcher` selects on, per event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatcherKind {
    /// A tool-name rule in the permission-rule grammar.
    Tool,
    /// One of a fixed set of values, `|`-separated.
    Values(&'static [&'static str]),
    /// Any agent type name, `|`-separated.
    AgentType,
    /// The event has nothing to select on.
    None,
}

/// Whether a repository may define hooks for an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectScope {
    Loads,
    /// The repository's hooks are dropped with a warning, for the given reason.
    IgnoredWithWarning(&'static str),
}

/// Every hook event Fontaine fires.
///
/// Adding an event is one variant and one arm in each method below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionDenied,
    TaskCompleted,
    TaskCreated,
    TeammateIdle,
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    Notification,
}

/// The values a `stop_failure` matcher names.
pub const STOP_FAILURE_KINDS: &[&str] = &[
    "rate_limit",
    "overloaded",
    "authentication_failed",
    "oauth_org_not_allowed",
    "account_on_hold",
    "billing_error",
    "invalid_request",
    "model_not_found",
    "server_error",
    "max_output_tokens",
    "cloud_credential_error",
    "unknown",
];

const COMMAND_ONLY: &[HandlerKind] = &[HandlerKind::Command];
const COMMAND_HTTP: &[HandlerKind] = &[HandlerKind::Command, HandlerKind::Http];
const ALL_HANDLERS: &[HandlerKind] =
    &[HandlerKind::Command, HandlerKind::Http, HandlerKind::Prompt];

impl HookEvent {
    pub const ALL: [HookEvent; 17] = [
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure,
        HookEvent::PermissionDenied,
        HookEvent::TaskCompleted,
        HookEvent::TaskCreated,
        HookEvent::TeammateIdle,
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::UserPromptSubmit,
        HookEvent::Stop,
        HookEvent::StopFailure,
        HookEvent::SubagentStart,
        HookEvent::SubagentStop,
        HookEvent::PreCompact,
        HookEvent::PostCompact,
        HookEvent::Notification,
    ];

    /// The key in `[[hooks.<key>]]`.
    pub fn config_key(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::PostToolUseFailure => "post_tool_use_failure",
            HookEvent::PermissionDenied => "permission_denied",
            HookEvent::TaskCompleted => "task_completed",
            HookEvent::TaskCreated => "task_created",
            HookEvent::TeammateIdle => "teammate_idle",
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::UserPromptSubmit => "user_prompt_submit",
            HookEvent::Stop => "stop",
            HookEvent::StopFailure => "stop_failure",
            HookEvent::SubagentStart => "subagent_start",
            HookEvent::SubagentStop => "subagent_stop",
            HookEvent::PreCompact => "pre_compact",
            HookEvent::PostCompact => "post_compact",
            HookEvent::Notification => "notification",
        }
    }

    /// The name the event carries on the wire and in Claude Code's `hooks.json`.
    pub fn wire_name(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::PermissionDenied => "PermissionDenied",
            HookEvent::TaskCompleted => "TaskCompleted",
            HookEvent::TaskCreated => "TaskCreated",
            HookEvent::TeammateIdle => "TeammateIdle",
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::Stop => "Stop",
            HookEvent::StopFailure => "StopFailure",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::PostCompact => "PostCompact",
            HookEvent::Notification => "Notification",
        }
    }

    pub fn from_wire_name(name: &str) -> Option<HookEvent> {
        HookEvent::ALL.into_iter().find(|e| e.wire_name() == name)
    }

    pub fn matcher_kind(self) -> MatcherKind {
        match self {
            HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionDenied => MatcherKind::Tool,
            HookEvent::SessionStart => {
                MatcherKind::Values(&["startup", "resume", "fork", "clear", "compact"])
            }
            HookEvent::SessionEnd => {
                MatcherKind::Values(&["clear", "resume", "prompt_input_exit", "other"])
            }
            HookEvent::StopFailure => MatcherKind::Values(STOP_FAILURE_KINDS),
            HookEvent::SubagentStart | HookEvent::SubagentStop => MatcherKind::AgentType,
            HookEvent::PreCompact | HookEvent::PostCompact => {
                MatcherKind::Values(&["manual", "auto"])
            }
            HookEvent::Notification => {
                MatcherKind::Values(&["permission_prompt", "elicitation_dialog"])
            }
            HookEvent::TaskCompleted
            | HookEvent::TaskCreated
            | HookEvent::TeammateIdle
            | HookEvent::UserPromptSubmit
            | HookEvent::Stop => MatcherKind::None,
        }
    }

    /// The handler types the event accepts.
    pub fn handlers(self) -> &'static [HandlerKind] {
        match self {
            HookEvent::SessionStart => COMMAND_ONLY,
            HookEvent::SessionEnd
            | HookEvent::StopFailure
            | HookEvent::SubagentStart
            | HookEvent::PreCompact
            | HookEvent::PostCompact
            | HookEvent::Notification => COMMAND_HTTP,
            HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionDenied
            | HookEvent::TaskCompleted
            | HookEvent::TaskCreated
            | HookEvent::TeammateIdle
            | HookEvent::UserPromptSubmit
            | HookEvent::Stop
            | HookEvent::SubagentStop => ALL_HANDLERS,
        }
    }

    /// Whether a repository's hooks for the event load.
    pub fn project_scope(self) -> ProjectScope {
        match self {
            HookEvent::TeammateIdle | HookEvent::Stop | HookEvent::SubagentStop => {
                ProjectScope::IgnoredWithWarning(
                    "it forces more model calls, and a repository may only tighten",
                )
            }
            _ => ProjectScope::Loads,
        }
    }

    /// How long a hook of the event runs before it is cut off, unless it sets `timeout_secs`.
    pub fn default_timeout(self) -> std::time::Duration {
        match self {
            HookEvent::SessionEnd => std::time::Duration::from_millis(1500),
            _ => std::time::Duration::from_secs(30),
        }
    }

    /// Checks a matcher against the event's matcher kind.
    pub fn check_matcher(self, matcher: &str) -> Result<(), String> {
        match self.matcher_kind() {
            MatcherKind::None => {
                Err("this event has nothing to select on, so it takes no `matcher`".to_string())
            }
            // The rule grammar is checked by the caller, which owns the file path for the error.
            MatcherKind::Tool => Ok(()),
            MatcherKind::AgentType | MatcherKind::Values(_)
                if matcher.contains('|')
                    && matcher.split('|').any(|name| name.trim().is_empty() || name == "*") =>
            {
                Err("a `|` list takes exact, non-empty names; `*` and empty entries only work as the whole matcher".to_string())
            }
            MatcherKind::AgentType => Ok(()),
            MatcherKind::Values(values) => {
                for name in matcher.split('|') {
                    if name != "*" && !name.is_empty() && !values.contains(&name) {
                        return Err(format!(
                            "`{name}` is not a value this event matches; use one of: {}",
                            values.join(", ")
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// What the hook runs for; see [`HookEvent::matcher_kind`]. A tool matcher is in the
    /// permission-rule grammar, and a hook without one runs for every call and counts as broad.
    pub matcher: Option<String>,
    /// The handler type; `command` when absent.
    #[serde(rename = "type", default)]
    pub kind: HandlerKind,
    /// The shell command of a `command` hook.
    pub command: Option<String>,
    /// The endpoint of an `http` hook.
    pub url: Option<String>,
    /// Extra request headers of an `http` hook. `$VAR` and `${VAR}` expand only for names in
    /// `allowed_env_vars`.
    pub headers: Option<BTreeMap<String, String>>,
    /// The environment variables an `http` hook's headers may read.
    pub allowed_env_vars: Option<Vec<String>>,
    /// The question of a `prompt` hook; `$ARGUMENTS` stands for the event.
    pub prompt: Option<String>,
    pub timeout_secs: Option<u64>,
    #[serde(skip)]
    pub origin: Origin,
    /// The plugin a plugin-origin hook came from, for per-plugin reporting. Not part of
    /// equality: the same hook from two plugins dedupes to one, as the same hook from two
    /// scopes does.
    #[serde(skip)]
    pub plugin: Option<String>,
}

impl Hook {
    /// A shell-command hook.
    pub fn command(matcher: Option<&str>, command: &str) -> Hook {
        Hook {
            matcher: matcher.map(str::to_string),
            command: Some(command.to_string()),
            ..Hook::default()
        }
    }

    /// What names the handler in reports and logs: the command, the URL or the prompt.
    pub fn target(&self) -> &str {
        match self.kind {
            HandlerKind::Command => self.command.as_deref(),
            HandlerKind::Http => self.url.as_deref(),
            HandlerKind::Prompt => self.prompt.as_deref(),
        }
        .unwrap_or("")
    }
}

impl PartialEq for Hook {
    fn eq(&self, other: &Self) -> bool {
        self.matcher == other.matcher
            && self.kind == other.kind
            && self.command == other.command
            && self.url == other.url
            && self.headers == other.headers
            && self.allowed_env_vars == other.allowed_env_vars
            && self.prompt == other.prompt
            && self.timeout_secs == other.timeout_secs
    }
}

/// The `[[hooks.<event>]]` tables of one file.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct HooksSection(pub BTreeMap<HookEvent, Vec<Hook>>);

impl JsonSchema for HooksSection {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HooksSection".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let list = generator.subschema_for::<Vec<Hook>>().to_value();
        let properties: serde_json::Map<String, serde_json::Value> = HookEvent::ALL
            .into_iter()
            .map(|event| (event.config_key().to_string(), list.clone()))
            .collect();
        schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        }))
        .expect("an object is a schema")
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_vars: Option<Vec<String>>,
    /// Literal variables set on the child after the scrub; plugin `.mcp.json` `env` maps land
    /// here, already expanded.
    pub env: Option<BTreeMap<String, String>>,
    pub url: Option<String>,
    /// The text holds `${VAR}` references to expand when the server starts: a repository's
    /// `.mcp.json`. Never set from a settings file.
    #[serde(skip)]
    pub expand: bool,
}

impl McpServer {
    /// The command line or URL, as a person would read it.
    pub fn command_line(&self) -> String {
        match (&self.command, &self.url) {
            (Some(command), _) => {
                let mut line = command.clone();
                for arg in self.args.iter().flatten() {
                    line.push(' ');
                    line.push_str(arg);
                }
                line
            }
            (None, Some(url)) => url.clone(),
            (None, None) => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    pub servers: Option<BTreeMap<String, McpServer>>,
}

/// A language server: a program that runs on the user's machine, so only the user file,
/// `--settings` and installed plugins define one.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LspServer {
    pub command: String,
    pub args: Option<Vec<String>>,
    /// File extension, with its dot, to the language id sent in `didOpen`.
    pub extension_to_language: BTreeMap<String, String>,
    /// Parent variables passed through to the server, on top of the base set.
    pub env_vars: Option<Vec<String>>,
    /// Literal variables set on the server after the scrub.
    pub env: Option<BTreeMap<String, String>>,
    /// Sent in `initialize`.
    pub initialization_options: Option<serde_json::Value>,
    /// Sent by `workspace/didChangeConfiguration` and answered to `workspace/configuration`.
    pub settings: Option<serde_json::Value>,
    pub startup_timeout_ms: Option<u64>,
    pub shutdown_timeout_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
    pub restart_on_crash: Option<bool>,
    pub max_restarts: Option<u32>,
    pub diagnostics: Option<bool>,
}

pub const LSP_STARTUP_TIMEOUT_MS: u64 = 30_000;
pub const LSP_SHUTDOWN_TIMEOUT_MS: u64 = 2_000;
pub const LSP_REQUEST_TIMEOUT_MS: u64 = 60_000;
pub const LSP_MAX_RESTARTS: u32 = 3;

impl LspServer {
    /// Why this definition cannot be used, if it cannot.
    pub fn problem(&self) -> Option<String> {
        if self.command.is_empty() {
            return Some("`command` must not be empty".into());
        }
        if self.extension_to_language.is_empty() {
            return Some("`extension_to_language` must name at least one extension".into());
        }
        if let Some(ext) = self
            .extension_to_language
            .keys()
            .find(|ext| ext.len() < 2 || !ext.starts_with('.'))
        {
            return Some(format!(
                "`extension_to_language` key `{ext}` must start with `.` and name an extension"
            ));
        }
        for (key, value) in [
            ("startup_timeout_ms", self.startup_timeout_ms),
            ("shutdown_timeout_ms", self.shutdown_timeout_ms),
            ("request_timeout_ms", self.request_timeout_ms),
        ] {
            if value == Some(0) {
                return Some(format!("`{key}` must be greater than zero"));
            }
        }
        None
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LspSection {
    pub servers: Option<BTreeMap<String, LspServer>>,
}

/// The language servers a session has, and which one handles each extension.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LspTable {
    /// User-defined servers in name order, then plugin servers in install order.
    pub servers: Vec<(String, LspServer)>,
    /// Lower-cased extension to the name of the server that handles it.
    pub claims: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoModeSection {
    pub environment: Option<Vec<String>>,
    pub allow: Option<Vec<String>>,
    pub soft_deny: Option<Vec<String>>,
    pub hard_deny: Option<Vec<String>>,
    pub setup_reminder: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentType {
    pub model: Option<OneOrMany>,
    pub tools: Option<Vec<String>>,
    pub mode: Option<AgentMode>,
    pub effort: Option<Effort>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentsSection {
    pub max_parallel: Option<u32>,
    pub subagent_token_budget: Option<u64>,
    pub subagent_cost_budget_usd: Option<f64>,
    pub session_token_budget: Option<u64>,
    pub session_cost_budget_usd: Option<f64>,
    pub teams: Option<bool>,
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
pub struct CheckpointsSection {
    pub enabled: Option<bool>,
    pub max_file_bytes: Option<u64>,
    pub max_session_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginsSection {
    pub auto_update: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemorySection {
    pub auto: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalSection {
    pub max_turns: Option<u32>,
    pub max_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AfkSection {
    /// The session temperature at which an AFK stretch is stopped; a positive, finite number.
    pub temperature_ceiling: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellSection {
    pub max_timeout_secs: Option<u64>,
}

/// How the `draft_bug_report` tool behaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DraftsMode {
    /// The tool is offered and the footer counts the drafts.
    #[default]
    On,
    /// The tool is offered; the footer stays silent.
    Quiet,
    /// The tool is not offered.
    Off,
}

impl DraftsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftsMode::On => "on",
            DraftsMode::Quiet => "quiet",
            DraftsMode::Off => "off",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSection {
    pub drafts: Option<DraftsMode>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatuslineSection {
    pub command: Option<String>,
    pub refresh_interval_secs: Option<u64>,
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

/// The input-box editing mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EditorMode {
    #[default]
    Emacs,
    Vim,
}

impl EditorMode {
    pub fn as_str(self) -> &'static str {
        match self {
            EditorMode::Emacs => "emacs",
            EditorMode::Vim => "vim",
        }
    }

    pub fn parse(name: &str) -> Option<EditorMode> {
        match name {
            "emacs" => Some(EditorMode::Emacs),
            "vim" => Some(EditorMode::Vim),
            _ => None,
        }
    }
}

/// How the terminal UI signals that a prompt is waiting for an answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyMethod {
    #[default]
    Bell,
    Osc9,
    Osc777,
    Off,
}

impl NotifyMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyMethod::Bell => "bell",
            NotifyMethod::Osc9 => "osc9",
            NotifyMethod::Osc777 => "osc777",
            NotifyMethod::Off => "off",
        }
    }
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
    /// Show the model's reasoning in the transcript (default true).
    pub show_reasoning: Option<bool>,
    pub editor_mode: Option<EditorMode>,
    /// How a pending approval or question is signalled (default bell).
    pub notify: Option<NotifyMethod>,
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
    pub models: Option<ModelsSection>,
    pub guardian: Option<GuardianSection>,
    pub permissions: Option<PermissionsSection>,
    pub hooks: Option<HooksSection>,
    pub mcp: Option<McpSection>,
    pub lsp: Option<LspSection>,
    pub auto_mode: Option<AutoModeSection>,
    pub agents: Option<AgentsSection>,
    pub context: Option<ContextSection>,
    pub schedule: Option<ScheduleSection>,
    pub memory: Option<MemorySection>,
    pub checkpoints: Option<CheckpointsSection>,
    pub plugins: Option<PluginsSection>,
    pub goal: Option<GoalSection>,
    pub afk: Option<AfkSection>,
    pub shell: Option<ShellSection>,
    pub shell_preclassifier: Option<ShellPreclassifierSection>,
    pub statusline: Option<StatuslineSection>,
    pub feedback: Option<FeedbackSection>,
    pub telemetry: Option<TelemetrySection>,
    pub websearch: Option<WebsearchSection>,
    pub env: Option<BTreeMap<String, EnvValue>>,
    pub keybindings: Option<BTreeMap<String, String>>,
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

/// The shape of the entries of one model list: `<account>/<model>`, a role name or `$main`, and
/// `$guardian_default` alone where `allow_guardian_default` says it means something. Whether a
/// role or an account exists is checked against the merged config.
fn check_model_entries(entries: &[String], allow_guardian_default: bool) -> Result<(), String> {
    if entries.is_empty() {
        return Err("a model list names at least one model".to_string());
    }
    for entry in entries {
        if entry == "$main" || model_ref_ok(entry) {
            continue;
        }
        if entry == "$guardian_default" {
            if allow_guardian_default && entries.len() == 1 {
                continue;
            }
            return Err("`$guardian_default` stands alone in an agent type's `model`".to_string());
        }
        if entry.contains('/') {
            return Err(format!("`{entry}`: expected `<account>/<model>`"));
        }
        if let Err(message) = fontaine_agents::registry::valid_role_name(entry) {
            return Err(format!(
                "`{entry}` is neither `<account>/<model>` nor a role name ({message})"
            ));
        }
    }
    Ok(())
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
    if account.quota.is_some() && !provider.accepts_quota() {
        return Err((
            format!("accounts.{name}.quota"),
            format!("not used by kind = \"{}\"", provider.name()),
        ));
    }
    if account.auth.is_some() && !provider.accepts_auth() {
        return Err((
            format!("accounts.{name}.auth"),
            format!("not used by kind = \"{}\"", provider.name()),
        ));
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
    for (key, set) in [
        ("cli_path", account.cli_path.is_some()),
        ("claude_config_dir", account.claude_config_dir.is_some()),
    ] {
        if set && provider.credential() != crate::providers::CredentialPolicy::ExternalCli {
            return Err((
                format!("accounts.{name}.{key}"),
                format!("not used by kind = \"{}\"", provider.name()),
            ));
        }
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

/// Whether an MCP server definition is usable: exactly one of a command and a URL, the keys that
/// belong to its transport, and a name every tool name can carry.
pub(crate) fn check_mcp_server(name: &str, server: &McpServer) -> Result<(), String> {
    match (&server.command, &server.url) {
        (Some(command), None) => {
            if command.is_empty() {
                return Err("`command` must not be empty".to_string());
            }
        }
        (None, Some(url)) => {
            if url.is_empty() {
                return Err("`url` must not be empty".to_string());
            }
            if server.args.is_some() || server.env_vars.is_some() || server.env.is_some() {
                return Err("`args`, `env_vars` and `env` apply to stdio servers only".to_string());
            }
        }
        _ => return Err("set exactly one of `command` (stdio) or `url`".to_string()),
    }
    if !crate::mcp_json::valid_server_name(name) {
        return Err(format!("server names: {}", crate::mcp_json::NAME_RULE));
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
    for (keystroke, action) in s.keybindings.iter().flatten() {
        if let Err(message) = crate::keybindings::parse_binding(keystroke, action) {
            return bad(&format!("keybindings.{keystroke}"), &message);
        }
    }
    if let Some(network) = &s.network
        && let Err((key, message)) = check_network_shape(&network.settings())
    {
        return bad(&format!("network.{key}"), &message);
    }
    if let Some(model) = s.model.as_ref().and_then(|m| m.model.as_ref())
        && let Err(message) = check_model_entries(model.entries(), false)
    {
        return bad("model.model", &message);
    }
    if s.model.as_ref().and_then(|m| m.max_output_tokens) == Some(0) {
        return bad("model.max_output_tokens", "must be at least 1");
    }
    for (name, list) in s.models.iter().flat_map(|m| m.roles.iter().flatten()) {
        let key = format!("models.roles.{name}");
        if let Err(message) = fontaine_agents::registry::valid_role_name(name) {
            return bad(&key, &message);
        }
        if let Err(message) = check_model_entries(list, false) {
            return bad(&key, &message);
        }
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
        if let Some(composition) = &g.composition {
            if let Some(model) = composition.model.as_deref()
                && !model_ref_ok(model)
            {
                return bad("guardian.composition.model", "expected `<account>/<model>`");
            }
            if composition.timeout_ms == Some(0) {
                return bad(
                    "guardian.composition.timeout_ms",
                    "must be greater than zero",
                );
            }
            if let (Some(name), Some(params)) = (&composition.preset, &composition.params) {
                let params = match composition_params(params) {
                    Ok(params) => params,
                    Err(e) => return bad(&e.key, &e.message),
                };
                if let Err(e) =
                    fontaine_guardian::composition::check_preset_params(name, &params, None)
                    && let Some(key) = &e.key
                {
                    return bad(&format!("guardian.composition.params.{key}"), &e.message);
                }
            }
            for (name, read) in composition.reads.iter().flatten() {
                if let Some(model) = read.model.as_deref()
                    && !model_ref_ok(model)
                {
                    return bad(
                        &format!("guardian.composition.reads.{name}.model"),
                        "expected `<account>/<model>`",
                    );
                }
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
        for (event, hooks) in &h.0 {
            for hook in hooks {
                let key = format!("hooks.{}", event.config_key());
                if let Some(matcher) = &hook.matcher {
                    event.check_matcher(matcher).or_else(|m| bad(&key, &m))?;
                    if event.matcher_kind() == MatcherKind::Tool {
                        parse_rule(matcher).map_err(|e| rule_error(path, &key, e))?;
                    }
                }
                if !event.handlers().contains(&hook.kind) {
                    return bad(
                        &key,
                        &format!(
                            "this event takes {} handlers, not `{}`",
                            event
                                .handlers()
                                .iter()
                                .map(|k| format!("`{}`", k.as_str()))
                                .collect::<Vec<_>>()
                                .join(" or "),
                            hook.kind.as_str()
                        ),
                    );
                }
                check_handler_fields(hook).or_else(|m| bad(&key, &m))?;
                if hook.timeout_secs == Some(0) {
                    return bad(&key, "`timeout_secs` must be greater than zero");
                }
            }
        }
    }
    for (name, server) in s.mcp.iter().flat_map(|m| m.servers.iter().flatten()) {
        if let Err(message) = check_mcp_server(name, server) {
            return bad(&format!("mcp.servers.{name}"), &message);
        }
    }
    for (name, server) in s.lsp.iter().flat_map(|l| l.servers.iter().flatten()) {
        let key = format!("lsp.servers.{name}");
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
        if let Some(problem) = server.problem() {
            return bad(&key, &problem);
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
            if let Some(model) = &ty.model
                && let Err(message) = check_model_entries(model.entries(), true)
            {
                return bad(&format!("{key}.model"), &message);
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
    if let Some(afk) = &s.afk
        && let Some(v) = afk.temperature_ceiling
        && !(v.is_finite() && v > 0.0)
    {
        return bad(
            "afk.temperature_ceiling",
            "must be a finite number above zero",
        );
    }
    if let Some(sh) = &s.shell
        && let Some(v) = sh.max_timeout_secs
        && !(1..=MAX_SHELL_TIMEOUT_SECS).contains(&v)
    {
        return bad(
            "shell.max_timeout_secs",
            &format!("must be between 1 and {MAX_SHELL_TIMEOUT_SECS}"),
        );
    }
    if let Some(c) = &s.checkpoints {
        for (key, v) in [
            ("checkpoints.max_file_bytes", c.max_file_bytes),
            ("checkpoints.max_session_bytes", c.max_session_bytes),
        ] {
            if v == Some(0) {
                return bad(key, "must be at least 1");
            }
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
    if let Some(sl) = &s.statusline
        && sl.refresh_interval_secs == Some(0)
    {
        return bad("statusline.refresh_interval_secs", "must be at least 1");
    }
    if let Some(sl) = &s.statusline
        && sl.refresh_interval_secs.is_some()
        && sl.command.as_deref().is_none_or(|c| c.trim().is_empty())
    {
        return bad(
            "statusline.refresh_interval_secs",
            "requires statusline.command",
        );
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

/// A hook has exactly the fields its handler type uses.
pub(crate) fn check_handler_fields(hook: &Hook) -> Result<(), String> {
    let (needs, takes): (&str, &[&str]) = match hook.kind {
        HandlerKind::Command => ("command", &["command"]),
        HandlerKind::Http => ("url", &["url", "headers", "allowed_env_vars"]),
        HandlerKind::Prompt => ("prompt", &["prompt"]),
    };
    let present = [
        ("command", hook.command.is_some()),
        ("url", hook.url.is_some()),
        ("headers", hook.headers.is_some()),
        ("allowed_env_vars", hook.allowed_env_vars.is_some()),
        ("prompt", hook.prompt.is_some()),
    ];
    let kind = hook.kind.as_str();
    if !present.contains(&(needs, true)) {
        return Err(format!("a `{kind}` hook needs `{needs}`"));
    }
    if let Some((name, _)) = present
        .iter()
        .find(|(name, set)| *set && !takes.contains(name))
    {
        return Err(format!("`{name}` does not apply to a `{kind}` hook"));
    }
    if hook.target().trim().is_empty() {
        return Err(format!("the {kind} hook's text must not be empty"));
    }
    if hook.kind == HandlerKind::Http {
        let url = hook.target();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("`url` must start with http:// or https://".to_string());
        }
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
    /// Set only by the user file; see [`GuardianComposition`].
    pub composition: Option<GuardianComposition>,
    /// A shared project file asked for shadow mode, which no other scope can undo.
    pub composition_shadow_forced: bool,
}

/// The merged `[guardian.composition]`.
#[derive(Clone, Debug, PartialEq)]
pub struct GuardianComposition {
    pub preset: Option<String>,
    /// The script path as written.
    pub script: Option<String>,
    /// The script's text, read when the configuration is checked.
    pub script_source: Option<String>,
    /// `<account>/<model>`: the default model of every read.
    pub model: Option<String>,
    pub shadow: bool,
    pub timeout_ms: u64,
    pub params: BTreeMap<String, serde_json::Value>,
    pub reads: BTreeMap<String, CompositionRead>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompositionRead {
    pub model: Option<String>,
    pub prompt: fontaine_guardian::Prompt,
    pub question: fontaine_guardian::Question,
}

impl GuardianComposition {
    /// The model label a read uses: its own, else the composition's.
    pub fn read_model<'a>(&'a self, read: &'a CompositionRead) -> Option<&'a str> {
        read.model.as_deref().or(self.model.as_deref())
    }

    /// Every distinct `<account>/<model>` the composition reads with, sorted.
    pub fn models(&self) -> Vec<String> {
        let mut models: std::collections::BTreeSet<String> = self
            .reads
            .values()
            .filter_map(|read| self.read_model(read))
            .map(str::to_owned)
            .collect();
        if self.preset.is_some() {
            models.extend(self.model.clone());
        }
        models.into_iter().collect()
    }

    /// The spec the composition compiles from. `accounts` say which backend each read model's
    /// account is; an account that is not configured, or of another kind, is read over Ollama,
    /// and [`check_composition`] is what refuses it.
    pub fn spec(
        &self,
        accounts: &BTreeMap<String, Account>,
    ) -> Result<fontaine_guardian::composition::CompositionSpec, ConfigSpecError> {
        use fontaine_guardian::composition::{CompositionSpec, Script};
        let script = match (&self.preset, &self.script_source) {
            (Some(name), _) => Script::Preset(name.clone()),
            (None, Some(source)) => Script::Source(source.clone()),
            (None, None) => {
                return Err(ConfigSpecError {
                    key: "guardian.composition.script".into(),
                    message: "the script was not loaded".into(),
                });
            }
        };
        let params = composition_params(&self.params)?;
        let kinds: BTreeMap<String, fontaine_guardian::BackendKind> = accounts
            .iter()
            .filter_map(|(name, account)| Some((name.clone(), backend_of(account.kind)?)))
            .collect();
        let mut reads = BTreeMap::new();
        for (name, read) in &self.reads {
            let Some(model) = self.read_model(read) else {
                return Err(ConfigSpecError {
                    key: format!("guardian.composition.reads.{name}.model"),
                    message: "requires `model`, here or on `[guardian.composition]`".into(),
                });
            };
            let account = model.split_once('/').map_or("", |(account, _)| account);
            reads.insert(
                name.clone(),
                fontaine_guardian::Read {
                    model: model.to_owned(),
                    prompt: read.prompt,
                    question: read.question,
                    backend: kinds
                        .get(account)
                        .copied()
                        .unwrap_or(fontaine_guardian::BackendKind::Ollama),
                },
            );
        }
        Ok(CompositionSpec {
            script,
            model: self.model.clone().unwrap_or_default(),
            reads,
            params,
            shadow: self.shadow,
            kinds,
        })
    }
}

/// What a screen read over an account of `kind` is asked through; `None` for a kind that cannot
/// serve one.
pub fn backend_of(kind: AccountKind) -> Option<fontaine_guardian::BackendKind> {
    match kind.as_str() {
        "ollama" => Some(fontaine_guardian::BackendKind::Ollama),
        "openrouter" => Some(fontaine_guardian::BackendKind::SystemOne),
        _ => None,
    }
}

/// The `/v1/systemone` URL of a hosted screen account: its `base_url`, which also serves chat,
/// else the kind's default, with `systemone` joined on.
pub fn systemone_endpoint(account: &Account) -> Result<url::Url, String> {
    let base = account
        .base_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .or_else(|| account.kind.provider().default_endpoint())
        .ok_or("the account has no `base_url`")?;
    let parsed = url::Url::parse(base).map_err(|e| format!("`base_url` is not a URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.has_host() {
        return Err("the guardian composition requires an http or https URL with a host".into());
    }
    url::Url::parse(&format!("{}/systemone", base.trim_end_matches('/')))
        .map_err(|e| format!("`base_url` is not a URL: {e}"))
}

/// The key of a hosted screen account, or why none resolves. Config loading and the reader's
/// construction both resolve through here, so they cannot disagree.
pub(crate) fn screen_key(
    name: &str,
    account: &Account,
    env: EnvLookup<'_>,
    stored: &credentials::Credentials,
    can_login: bool,
) -> Result<String, String> {
    use crate::credentials::StoredCredential;
    use crate::models::{Credential, credential};
    match credential(name, account, env, stored) {
        Credential::Found(StoredCredential::ApiKey(key)) => Ok(key),
        Credential::Found(_) | Credential::NoSource => Err(if can_login {
            format!(
                "account `{name}` has no OpenRouter key; set `api_key_env` in [accounts.{name}] or run `/login {name}`"
            )
        } else {
            // Startup fails here, so `/login` is unreachable.
            format!(
                "account `{name}` has no OpenRouter key; set `api_key_env` in [accounts.{name}], or fix or remove [guardian.composition]"
            )
        }),
        Credential::Unresolved(why) => Err(format!("account `{name}`: {why}")),
    }
}

/// The values a script or preset receives as `params`.
fn composition_params(
    params: &BTreeMap<String, serde_json::Value>,
) -> Result<BTreeMap<String, fontaine_guardian::composition::Param>, ConfigSpecError> {
    use fontaine_guardian::composition::Param;
    let mut out = BTreeMap::new();
    for (key, value) in params {
        let param = match value {
            serde_json::Value::Number(n) => Param::Number(n.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(text) => Param::Str(text.clone()),
            serde_json::Value::Bool(b) => Param::Bool(*b),
            _ => {
                return Err(ConfigSpecError {
                    key: format!("guardian.composition.params.{key}"),
                    message: "must be a number, a string or a boolean".into(),
                });
            }
        };
        out.insert(key.clone(), param);
    }
    Ok(out)
}

/// A composition key that cannot be turned into a spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigSpecError {
    pub key: String,
    pub message: String,
}

impl std::fmt::Display for ConfigSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

impl std::error::Error for ConfigSpecError {}

/// Each read's default time limit.
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
    /// In `auto`, edits to tracked files, new files and files this session created skip the
    /// guardian.
    pub auto_tracked_edits: bool,
    /// In `auto`, file-tool writes inside the session scratch directory skip the guardian.
    pub auto_scratch_writes: bool,
}

/// The merged hooks, one list per event.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hooks(BTreeMap<HookEvent, Vec<Hook>>);

impl Hooks {
    pub fn get(&self, event: HookEvent) -> &[Hook] {
        self.0.get(&event).map_or(&[], Vec::as_slice)
    }

    pub fn list_mut(&mut self, event: HookEvent) -> &mut Vec<Hook> {
        self.0.entry(event).or_default()
    }

    /// Every event in declaration order, with its hooks (possibly none).
    pub fn iter(&self) -> impl Iterator<Item = (HookEvent, &[Hook])> {
        HookEvent::ALL.into_iter().map(|e| (e, self.get(e)))
    }

    /// Every hook of every event.
    pub fn all(&self) -> impl Iterator<Item = &Hook> {
        self.0.values().flatten()
    }

    pub fn len(&self) -> usize {
        self.0.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends another set's hooks, event by event.
    pub fn extend(&mut self, other: Hooks) {
        for (event, hooks) in other.0 {
            self.0.entry(event).or_default().extend(hooks);
        }
    }

    /// Keeps the hooks `keep` accepts.
    pub fn retain(&mut self, mut keep: impl FnMut(HookEvent, &Hook) -> bool) {
        for (event, hooks) in &mut self.0 {
            hooks.retain(|hook| keep(*event, hook));
        }
    }

    /// Visits every hook mutably.
    pub fn for_each_mut(&mut self, mut f: impl FnMut(&mut Hook)) {
        self.0.values_mut().flatten().for_each(&mut f);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutoMode {
    pub environment: Vec<String>,
    pub allow: Vec<String>,
    pub soft_deny: Vec<String>,
    pub hard_deny: Vec<String>,
    /// Whether an `auto` session without stored project facts warns to run the setup command.
    pub setup_reminder: bool,
}

impl Default for AutoMode {
    fn default() -> Self {
        AutoMode {
            environment: Vec::new(),
            allow: Vec::new(),
            soft_deny: Vec::new(),
            hard_deny: Vec::new(),
            setup_reminder: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Agents {
    pub max_parallel: u32,
    pub subagent_token_budget: Option<u64>,
    pub subagent_cost_budget_usd: Option<f64>,
    pub session_token_budget: Option<u64>,
    pub session_cost_budget_usd: Option<f64>,
    /// The task boards and their quality hooks. Idling and waking do not depend on it.
    pub teams: bool,
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
    /// `mcp-local.toml` in the user's config directory: servers private to one workspace.
    Local,
    ProjectLocal,
    Project,
    User,
    /// A plugin: installed by the user, but its content is third-party and tighten-only.
    Plugin,
}

impl Slot {
    pub fn label(self) -> &'static str {
        match self {
            Slot::Managed => "managed",
            Slot::Cli => "cli",
            Slot::Local => "local",
            Slot::ProjectLocal => "project-local",
            Slot::Project => "project",
            Slot::User => "user",
            Slot::Plugin => "plugin",
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
/// A full day; keeps `Instant + Duration` far from overflow.
const MAX_SHELL_TIMEOUT_SECS: u64 = 86_400;
const DEFAULT_SHELL_MAX_TIMEOUT_SECS: u64 = 600;

/// The merged `[schedule]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    /// Whether the scheduling tools and `/loop` are available.
    pub enabled: bool,
}

/// The merged `[checkpoints]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoints {
    /// Whether `write` and `edit` save the file's pre-image for `/rewind`.
    pub enabled: bool,
    /// A file larger than this is not saved.
    pub max_file_bytes: u64,
    /// Once the session's saved pre-images reach this many bytes, later files are not saved.
    pub max_session_bytes: u64,
}

impl Default for Checkpoints {
    fn default() -> Self {
        Checkpoints {
            enabled: true,
            max_file_bytes: DEFAULT_CHECKPOINT_FILE_BYTES,
            max_session_bytes: DEFAULT_CHECKPOINT_SESSION_BYTES,
        }
    }
}

pub const DEFAULT_CHECKPOINT_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_CHECKPOINT_SESSION_BYTES: u64 = 256 * 1024 * 1024;

/// The merged `[plugins]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Plugins {
    /// Whether registered marketplaces and installed plugins update themselves at interactive
    /// start.
    pub auto_update: bool,
}

impl Default for Plugins {
    fn default() -> Self {
        Plugins { auto_update: true }
    }
}

/// The merged `[memory]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Memory {
    /// Whether the agent may save notes to the per-project memory store and the store's index
    /// loads into each session.
    pub auto: bool,
}

/// The merged `[goal]` section: what bounds an unattended stretch of goal turns.
#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    /// Goal turns in one unattended stretch.
    pub max_turns: u32,
    /// Spend of one unattended stretch; unset is no cost bound.
    pub max_cost_usd: Option<f64>,
}

/// The merged `[afk]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Afk {
    /// What the user file, `--settings` or `--afk-temperature-ceiling` set, or the default.
    pub temperature_ceiling: f64,
    /// The lowest value a project file asked for. A repository can lower the ceiling but never
    /// raise it, and which file is read first does not matter.
    pub ceiling_project_cap: Option<f64>,
}

impl Afk {
    /// The temperature at which a stretch stops.
    pub fn ceiling(&self) -> f64 {
        self.ceiling_project_cap
            .map_or(self.temperature_ceiling, |cap| {
                cap.min(self.temperature_ceiling)
            })
    }
}

/// The merged `[shell]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Shell {
    /// The ceiling on a model-requested `shell` timeout, in seconds.
    pub max_timeout_secs: u64,
}

/// The merged `[feedback]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Feedback {
    pub drafts: DraftsMode,
}

/// The merged `[statusline]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Statusline {
    pub command: Option<String>,
    pub refresh_interval_secs: Option<u64>,
}

/// The merged `[tui]` section.
#[derive(Clone, Debug, PartialEq)]
pub struct Tui {
    pub theme: ThemeName,
    pub screen_reader: Option<bool>,
    pub show_reasoning: bool,
    pub editor_mode: EditorMode,
    pub notify: NotifyMethod,
}

/// Where the session's main model list comes from, once the layers are merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainSource {
    /// Neither `[model] model` nor `[models.roles] main` is set.
    Unset,
    /// `[model] model` as one string.
    ModelKeyString,
    /// `[model] model` as a list.
    ModelKeyList,
    /// `[models.roles] main`.
    Role,
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
    /// `[model] model`, as written: shorthand for the `main` role.
    pub model: Option<OneOrMany>,
    /// `[models.roles]`, each role replaced whole by the highest-precedence layer that sets it.
    pub roles: BTreeMap<String, Vec<String>>,
    /// Which of `[model] model` and `[models.roles] main` names the session's main models.
    pub main_source: MainSource,
    /// `[model] effort`; unset sends no reasoning parameter.
    pub effort: Option<Effort>,
    /// `[model] max_output_tokens`; unset sends the model's output limit, else a fixed fallback.
    pub max_output_tokens: Option<u32>,
    pub guardian: Guardian,
    pub permissions: Permissions,
    pub hooks: Hooks,
    pub mcp_servers: BTreeMap<String, McpServer>,
    /// Where each server in `mcp_servers` was defined.
    pub mcp_origins: BTreeMap<String, Origin>,
    /// Problems that did not stop the load: a malformed `mcp-local.toml` or `.mcp.json`, an
    /// entry in one that cannot be used, a name defined twice.
    pub warnings: Vec<String>,
    /// The language servers of the user file and `--settings`.
    pub lsp_servers: BTreeMap<String, LspServer>,
    /// The language servers installed plugins define, in install order, under composed names.
    pub plugin_lsp_servers: Vec<(String, LspServer)>,
    pub auto_mode: AutoMode,
    /// Deny-side guardian prose a plugin contributes, spliced into the guardian's rule lists.
    /// Kept apart from `auto_mode` so re-applying plugins replaces these rather than
    /// accumulating entries of plugins that are no longer installed.
    pub plugin_hard_deny: Vec<String>,
    pub plugin_soft_deny: Vec<String>,
    pub agents: Agents,
    pub context: Context,
    pub schedule: Schedule,
    pub memory: Memory,
    pub checkpoints: Checkpoints,
    pub plugins: Plugins,
    pub goal: Goal,
    pub afk: Afk,
    pub shell: Shell,
    pub shell_preclassifier_allow_extra: Vec<String>,
    pub statusline: Statusline,
    pub feedback: Feedback,
    pub telemetry: Telemetry,
    pub websearch: Websearch,
    pub tui: Tui,
    /// `[env]`: variables set on every tool and extension subprocess.
    pub env: BTreeMap<String, EnvValue>,
    /// `[keybindings]`: keystroke → action map.
    pub keybindings: BTreeMap<String, String>,
}

impl Config {
    /// The language servers of this session and which one handles each extension. The first
    /// server to name an extension keeps it (the user's, in name order, then plugins' in
    /// install order); a later claimant is reported.
    pub fn lsp_table(&self) -> LspTable {
        let mut table = LspTable::default();
        let all = self
            .lsp_servers
            .iter()
            .map(|(name, server)| (name.clone(), server.clone()))
            .chain(self.plugin_lsp_servers.iter().cloned());
        for (name, server) in all {
            for extension in server.extension_to_language.keys() {
                let key = extension.to_ascii_lowercase();
                match table.claims.entry(key) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(name.clone());
                    }
                    std::collections::btree_map::Entry::Occupied(claimed) => {
                        if *claimed.get() == name {
                            continue;
                        }
                        table.warnings.push(format!(
                            "LSP server \"{name}\" is not used for {extension} files"
                        ));
                    }
                }
            }
            table.servers.push((name, server));
        }
        table
    }
}

impl Config {
    /// The environment and rule lists a session hands the guardian: the configured `[auto_mode]`
    /// entries after `$defaults` splicing and the tighten-only rules.
    pub fn effective_auto_mode(
        &self,
    ) -> (
        fontaine_guardian::EnvironmentConfig,
        fontaine_guardian::RuleConfig,
    ) {
        let auto_mode = &self.auto_mode;
        let mut hard_deny = auto_mode.hard_deny.clone();
        hard_deny.extend(self.plugin_hard_deny.iter().cloned());
        let mut soft_deny = auto_mode.soft_deny.clone();
        soft_deny.extend(self.plugin_soft_deny.iter().cloned());
        (
            fontaine_guardian::EnvironmentConfig::expand(&auto_mode.environment),
            fontaine_guardian::RuleConfig::expand(&hard_deny, &soft_deny, &auto_mode.allow),
        )
    }

    pub(crate) fn defaults(workspace: PathBuf) -> Self {
        Config {
            workspace,
            scopes: Vec::new(),
            sources: BTreeMap::new(),
            accounts: BTreeMap::new(),
            shadowed_accounts: Default::default(),
            network: NetworkSettings::default(),
            model: None,
            roles: BTreeMap::new(),
            main_source: MainSource::Unset,
            effort: None,
            max_output_tokens: None,
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
                composition: None,
                composition_shadow_forced: false,
            },
            permissions: Permissions {
                default_mode: None,
                deny: Vec::new(),
                ask: Vec::new(),
                allow: Vec::new(),
                additional_directories: Vec::new(),
                auto_tracked_edits: true,
                auto_scratch_writes: true,
            },
            hooks: Hooks::default(),
            mcp_servers: BTreeMap::new(),
            mcp_origins: BTreeMap::new(),
            warnings: Vec::new(),
            lsp_servers: BTreeMap::new(),
            plugin_lsp_servers: Vec::new(),
            auto_mode: AutoMode::default(),
            plugin_hard_deny: Vec::new(),
            plugin_soft_deny: Vec::new(),
            agents: Agents {
                max_parallel: 4,
                subagent_token_budget: None,
                subagent_cost_budget_usd: None,
                session_token_budget: None,
                session_cost_budget_usd: None,
                teams: true,
                types: BTreeMap::new(),
            },
            context: Context::default(),
            schedule: Schedule { enabled: true },
            memory: Memory { auto: true },
            checkpoints: Checkpoints::default(),
            plugins: Plugins::default(),
            goal: Goal {
                max_turns: DEFAULT_GOAL_MAX_TURNS,
                max_cost_usd: None,
            },
            afk: Afk {
                temperature_ceiling: fontaine_guardian::temperature::DEFAULT_CEILING,
                ceiling_project_cap: None,
            },
            shell: Shell {
                max_timeout_secs: DEFAULT_SHELL_MAX_TIMEOUT_SECS,
            },
            shell_preclassifier_allow_extra: Vec::new(),
            statusline: Statusline {
                command: None,
                refresh_interval_secs: None,
            },
            feedback: Feedback {
                drafts: DraftsMode::default(),
            },
            tui: Tui {
                theme: ThemeName::default(),
                screen_reader: None,
                show_reasoning: true,
                editor_mode: EditorMode::default(),
                notify: NotifyMethod::default(),
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
            keybindings: BTreeMap::new(),
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
            if let Some(v) = tui.show_reasoning {
                self.tui.show_reasoning = v;
            }
            if let Some(v) = tui.editor_mode {
                self.tui.editor_mode = v;
            }
            if let Some(v) = tui.notify {
                self.tui.notify = v;
            }
        }
        if let Some(model) = s.model.as_ref().and_then(|m| m.model.clone()) {
            self.model = Some(model);
        }
        for (name, list) in s.models.iter().flat_map(|m| m.roles.iter().flatten()) {
            self.roles.insert(name.clone(), list.clone());
        }
        if let Some(effort) = s.model.as_ref().and_then(|m| m.effort) {
            self.effort = Some(effort);
        }
        if let Some(cap) = s.model.as_ref().and_then(|m| m.max_output_tokens) {
            self.max_output_tokens = Some(cap);
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
            if let Some(composition) = &g.composition {
                if column != Column::User && composition.shadow == Some(true) {
                    t.composition_shadow_forced = true;
                }
                if composition.loosens() {
                    let merged = t.composition.get_or_insert_with(|| GuardianComposition {
                        preset: None,
                        script: None,
                        script_source: None,
                        model: None,
                        shadow: true,
                        timeout_ms: DEFAULT_SCREEN_TIMEOUT_MS,
                        params: BTreeMap::new(),
                        reads: BTreeMap::new(),
                    });
                    if let Some(v) = &composition.preset {
                        merged.preset = Some(v.clone());
                    }
                    if let Some(v) = &composition.script {
                        merged.script = Some(v.clone());
                    }
                    if let Some(v) = &composition.model {
                        merged.model = Some(v.clone());
                    }
                    if let Some(v) = composition.timeout_ms {
                        merged.timeout_ms = v;
                    }
                    if let Some(v) = composition.shadow {
                        merged.shadow = v;
                    }
                    if let Some(v) = &composition.params {
                        merged.params = v.clone();
                    }
                    if let Some(v) = &composition.reads {
                        merged.reads = v
                            .iter()
                            .map(|(name, read)| {
                                (
                                    name.clone(),
                                    CompositionRead {
                                        model: read.model.clone(),
                                        prompt: read.prompt.unwrap_or_default(),
                                        question: read.question.unwrap_or_default(),
                                    },
                                )
                            })
                            .collect();
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
            if let Some(v) = p.auto_tracked_edits {
                t.auto_tracked_edits = v;
            }
            if let Some(v) = p.auto_scratch_writes {
                t.auto_scratch_writes = v;
            }
        }
        if let Some(h) = &s.hooks {
            for (event, hooks) in &h.0 {
                union_hooks(self.hooks.list_mut(*event), Some(hooks), origin);
            }
        }
        for (name, server) in s.mcp.iter().flat_map(|m| m.servers.iter().flatten()) {
            self.mcp_servers.insert(name.clone(), server.clone());
            self.mcp_origins.insert(name.clone(), origin);
        }
        for (name, server) in s.lsp.iter().flat_map(|l| l.servers.iter().flatten()) {
            self.lsp_servers.insert(name.clone(), server.clone());
        }
        if let Some(a) = &s.auto_mode {
            let t = &mut self.auto_mode;
            union(&mut t.environment, a.environment.as_deref());
            union(&mut t.allow, a.allow.as_deref());
            union(&mut t.soft_deny, a.soft_deny.as_deref());
            union(&mut t.hard_deny, a.hard_deny.as_deref());
            if let Some(v) = a.setup_reminder {
                t.setup_reminder = v;
            }
        }
        if let Some(a) = &s.agents {
            let t = &mut self.agents;
            if let Some(v) = a.max_parallel {
                t.max_parallel = v;
            }
            if let Some(v) = a.subagent_token_budget {
                t.subagent_token_budget = Some(v);
            }
            if let Some(v) = a.subagent_cost_budget_usd {
                t.subagent_cost_budget_usd = Some(v);
            }
            if let Some(v) = a.session_token_budget {
                t.session_token_budget = Some(v);
            }
            if let Some(v) = a.session_cost_budget_usd {
                t.session_cost_budget_usd = Some(v);
            }
            if let Some(v) = a.teams {
                t.teams = v;
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
        if let Some(v) = s.memory.as_ref().and_then(|c| c.auto) {
            self.memory.auto = v;
        }
        if let Some(c) = &s.checkpoints {
            if let Some(v) = c.enabled {
                self.checkpoints.enabled = v;
            }
            if let Some(v) = c.max_file_bytes {
                self.checkpoints.max_file_bytes = v;
            }
            if let Some(v) = c.max_session_bytes {
                self.checkpoints.max_session_bytes = v;
            }
        }
        if let Some(v) = s.plugins.as_ref().and_then(|c| c.auto_update) {
            self.plugins.auto_update = v;
        }
        if let Some(g) = &s.goal {
            if let Some(v) = g.max_turns {
                self.goal.max_turns = v;
            }
            if let Some(v) = g.max_cost_usd {
                self.goal.max_cost_usd = Some(v);
            }
        }
        if let Some(v) = s.afk.as_ref().and_then(|afk| afk.temperature_ceiling) {
            if matches!(column, Column::Project | Column::ProjectLocal) {
                let cap = self.afk.ceiling_project_cap.map_or(v, |held| held.min(v));
                self.afk.ceiling_project_cap = Some(cap);
            } else {
                self.afk.temperature_ceiling = v;
            }
        }
        if let Some(v) = s.shell.as_ref().and_then(|sh| sh.max_timeout_secs) {
            self.shell.max_timeout_secs = v;
        }
        if let Some(sp) = &s.shell_preclassifier {
            union(
                &mut self.shell_preclassifier_allow_extra,
                sp.allow_extra.as_deref(),
            );
        }
        if let Some(v) = s.feedback.as_ref().and_then(|f| f.drafts) {
            self.feedback.drafts = v;
        }
        if let Some(sl) = &s.statusline {
            if let Some(v) = &sl.command {
                self.statusline.command = Some(v.clone());
            }
            if let Some(v) = sl.refresh_interval_secs {
                self.statusline.refresh_interval_secs = Some(v);
            }
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
        for (k, v) in s.keybindings.iter().flatten() {
            // Spellings that parse to the same keystroke (`ctrl+t`, `Ctrl+T`) are one
            // binding: store the canonical form so a higher-precedence layer replaces the
            // entry instead of coexisting with it and winning by map iteration order.
            let canonical = crate::keybindings::parse_keystroke(k)
                .map(|stroke| stroke.format())
                .unwrap_or_else(|_| k.clone());
            self.keybindings.retain(|existing, _| {
                crate::keybindings::parse_keystroke(existing)
                    .map(|stroke| stroke.format())
                    .unwrap_or_else(|_| existing.clone())
                    != canonical
            });
            self.keybindings.insert(canonical, v.clone());
        }
    }
}

pub(crate) fn union<T: PartialEq + Clone>(target: &mut Vec<T>, items: Option<&[T]>) {
    for item in items.unwrap_or_default() {
        if !target.contains(item) {
            target.push(item.clone());
        }
    }
}

pub(crate) fn union_hooks(target: &mut Vec<Hook>, items: Option<&[Hook]>, origin: Origin) {
    // The same hook defined in several scopes is kept once, under the origin with more trust.
    let rank = |o: Origin| (o.user_trust, !o.is_project());
    for item in items.unwrap_or_default() {
        match target.iter_mut().find(|kept| *kept == item) {
            Some(kept) => {
                if origin.slot == Slot::Plugin {
                    // A plugin's hook identical to another scope's merges by attribution
                    // only: the kept origin stands, so stripping the plugin's hooks on a
                    // re-application never takes the other scope's hook with it.
                    if kept.plugin.is_none() {
                        kept.plugin = item.plugin.clone();
                    }
                } else if rank(origin) > rank(kept.origin) {
                    kept.origin = origin;
                    kept.plugin = item.plugin.clone();
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
    /// `--afk-temperature-ceiling`: loads as the CLI scope's `afk.temperature_ceiling`.
    pub afk_temperature_ceiling: Option<f64>,
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

/// The workspace and the two project files in it.
struct ProjectFiles {
    workspace: PathBuf,
    project: PathBuf,
    local: PathBuf,
}

fn project_files(launch_dir: &Path) -> Result<ProjectFiles, ConfigError> {
    let workspace = std::fs::canonicalize(launch_dir).map_err(|source| ConfigError::Read {
        path: launch_dir.to_path_buf(),
        source,
    })?;
    let dir = workspace.join(PROJECT_DIR);
    Ok(ProjectFiles {
        project: dir.join(PROJECT_FILE),
        local: dir.join(PROJECT_LOCAL_FILE),
        workspace,
    })
}

/// The column an existing project-local file is validated against, and why it is the project
/// column when it is.
fn local_column(files: &ProjectFiles, data_dir: &Path) -> (Column, Option<ProjectDemotion>) {
    match trust::classify_project_local(&files.workspace, &files.local, data_dir) {
        LocalClassification::UserTrust => (Column::ProjectLocal, None),
        LocalClassification::Project(why) => (Column::Project, Some(why)),
    }
}

/// What one settings layer came to when read on its own.
#[derive(Debug)]
pub enum LayerOutcome {
    /// No file there.
    Absent,
    Loaded,
    /// The file is unreadable, malformed, or sets a key its scope may not.
    Failed(ConfigError),
    /// Not looked at, for the reason given.
    NotChecked(String),
}

#[derive(Debug)]
pub struct LayerDiagnosis {
    pub slot: Slot,
    pub column: Option<Column>,
    /// `None` for the managed slot, which has no file.
    pub path: Option<PathBuf>,
    /// Why a project-local file loads as project settings.
    pub demotion: Option<ProjectDemotion>,
    pub outcome: LayerOutcome,
}

impl LayerDiagnosis {
    /// The layers of a configuration that loaded, in the order [`diagnose`] reports them.
    pub fn from_scopes(config: &Config, data_dir: &Path) -> Vec<LayerDiagnosis> {
        let files = project_files(&config.workspace).ok();
        let mut out: Vec<LayerDiagnosis> = config
            .scopes
            .iter()
            .map(|entry| {
                let demotion = (entry.slot == Slot::ProjectLocal
                    && entry.column == Some(Column::Project)
                    && entry.loaded)
                    .then(|| {
                        let files = files.as_ref()?;
                        match trust::classify_project_local(
                            &files.workspace,
                            &files.local,
                            data_dir,
                        ) {
                            LocalClassification::Project(why) => Some(why),
                            LocalClassification::UserTrust => None,
                        }
                    })
                    .flatten();
                LayerDiagnosis {
                    slot: entry.slot,
                    column: entry.column,
                    path: entry.path.clone(),
                    demotion,
                    outcome: if entry.slot == Slot::Managed {
                        LayerOutcome::NotChecked("not supported".to_string())
                    } else if entry.loaded {
                        LayerOutcome::Loaded
                    } else {
                        LayerOutcome::Absent
                    },
                }
            })
            .collect();
        out.sort_by_key(|d| diagnosis_order(d.slot));
        out
    }
}

fn diagnosis_order(slot: Slot) -> u8 {
    match slot {
        Slot::Managed => 0,
        Slot::User => 1,
        Slot::Project => 2,
        Slot::ProjectLocal => 3,
        Slot::Local => 4,
        Slot::Cli => 5,
        Slot::Plugin => 6,
    }
}

fn diagnose_file(
    slot: Slot,
    column: Column,
    path: &Path,
    read: fn(&Path) -> io::Result<String>,
    demotion: Option<ProjectDemotion>,
    hint: Option<&str>,
) -> LayerDiagnosis {
    let outcome = match read_optional(path, read) {
        Ok(None) => LayerOutcome::Absent,
        Ok(Some(text)) => match parse_settings_with_hint(&text, path, column, hint) {
            Ok(_) => LayerOutcome::Loaded,
            Err(e) => LayerOutcome::Failed(e),
        },
        Err(e) => LayerOutcome::Failed(e),
    };
    LayerDiagnosis {
        slot,
        column: Some(column),
        path: Some(path.to_path_buf()),
        demotion,
        outcome,
    }
}

/// Reads every settings layer independently, so a bad layer does not hide the others. [`load`]
/// stops at the first failure; this reports each layer's own first failure, with the same
/// parser and scope rules. Checks that span layers are not repeated here.
pub fn diagnose(params: &LoadParams<'_>) -> Vec<LayerDiagnosis> {
    let user_path = params.config_dir.join(USER_FILE);
    let mut out = vec![
        LayerDiagnosis {
            slot: Slot::Managed,
            column: None,
            path: None,
            demotion: None,
            outcome: LayerOutcome::NotChecked("not supported".to_string()),
        },
        diagnose_file(
            Slot::User,
            Column::User,
            &user_path,
            crate::file_lock::read_shared,
            None,
            None,
        ),
    ];
    match project_files(params.launch_dir) {
        Ok(files) => {
            out.push(diagnose_file(
                Slot::Project,
                Column::Project,
                &files.project,
                |p| std::fs::read_to_string(p),
                None,
                None,
            ));
            let (column, demotion) = if files.local.is_file() {
                local_column(&files, params.data_dir)
            } else {
                (Column::ProjectLocal, None)
            };
            let hint = demotion
                .as_ref()
                .and_then(|why| demotion_hint(why, params.data_dir));
            out.push(diagnose_file(
                Slot::ProjectLocal,
                column,
                &files.local,
                |p| std::fs::read_to_string(p),
                demotion,
                hint.as_deref(),
            ));
            let json_path = crate::mcp_config::json_path(&files.workspace);
            match crate::mcp_config::check_json_file(&json_path) {
                Ok(false) => {}
                outcome => out.push(LayerDiagnosis {
                    slot: Slot::Project,
                    column: Some(Column::Project),
                    path: Some(json_path),
                    demotion: None,
                    outcome: match outcome {
                        Ok(_) => LayerOutcome::Loaded,
                        Err(e) => LayerOutcome::Failed(e),
                    },
                }),
            }
        }
        Err(_) => {
            for (slot, column) in [
                (Slot::Project, Column::Project),
                (Slot::ProjectLocal, Column::ProjectLocal),
            ] {
                out.push(LayerDiagnosis {
                    slot,
                    column: Some(column),
                    path: None,
                    demotion: None,
                    outcome: LayerOutcome::NotChecked(
                        "the workspace directory cannot be resolved".to_string(),
                    ),
                });
            }
        }
    }
    let local_path = params.config_dir.join(crate::mcp_config::LOCAL_FILE);
    match crate::mcp_config::check_local_file(&local_path) {
        Ok(false) => {}
        outcome => out.push(LayerDiagnosis {
            slot: Slot::Local,
            column: Some(Column::User),
            path: Some(local_path),
            demotion: None,
            outcome: match outcome {
                Ok(_) => LayerOutcome::Loaded,
                Err(e) => LayerOutcome::Failed(e),
            },
        }),
    }
    if let Some(path) = params.settings_file {
        let outcome = match std::fs::read_to_string(path) {
            Err(source) => LayerOutcome::Failed(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
            Ok(text) => match parse_settings_with_hint(&text, path, Column::Cli, None) {
                Ok(_) => LayerOutcome::Loaded,
                Err(e) => LayerOutcome::Failed(e),
            },
        };
        out.push(LayerDiagnosis {
            slot: Slot::Cli,
            column: Some(Column::Cli),
            path: Some(path.to_path_buf()),
            demotion: None,
            outcome,
        });
    }
    out
}

pub fn load(params: &LoadParams<'_>) -> Result<Config, ConfigError> {
    let files = project_files(params.launch_dir)?;

    let user_path = params.config_dir.join(USER_FILE);
    let user = read_optional(&user_path, crate::file_lock::read_shared)?
        .map(|text| parse_settings_with_hint(&text, &user_path, Column::User, None))
        .transpose()?;

    let project = read_optional(&files.project, |p| std::fs::read_to_string(p))?
        .map(|text| parse_settings_with_hint(&text, &files.project, Column::Project, None))
        .transpose()?;

    let (local_column, local) = match read_optional(&files.local, |p| std::fs::read_to_string(p))? {
        None => (Column::ProjectLocal, None),
        Some(text) => {
            let (column, demotion) = local_column(&files, params.data_dir);
            let hint = demotion
                .as_ref()
                .and_then(|why| demotion_hint(why, params.data_dir));
            let settings = parse_settings_with_hint(&text, &files.local, column, hint.as_deref())?;
            (column, Some(settings))
        }
    };
    let ProjectFiles {
        workspace,
        project: project_path,
        local: local_path,
    } = files;

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
    let flag_rules = !params.cli_rules.allow.is_empty()
        || !params.cli_rules.deny.is_empty()
        || params.cli_rules.afk_temperature_ceiling.is_some();
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
        if let Some(ceiling) = params.cli_rules.afk_temperature_ceiling {
            if !(ceiling.is_finite() && ceiling > 0.0) {
                return Err(ConfigError::usage(
                    "--afk-temperature-ceiling: must be a finite number above zero",
                ));
            }
            settings
                .afk
                .get_or_insert_with(Default::default)
                .temperature_ceiling = Some(ceiling);
            table
                .entry("afk")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .expect("a CLI settings file's `afk` already parsed as a table")
                .insert("temperature_ceiling".into(), toml::Value::Float(ceiling));
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

    let mut config = Config::defaults(workspace.clone());
    config.scopes.push(ScopeEntry {
        slot: Slot::Managed,
        column: None,
        path: None,
        loaded: false,
    });
    let local_path = params.config_dir.join(crate::mcp_config::LOCAL_FILE);
    let local_servers = crate::mcp_config::read_local(&local_path, &workspace);
    let json_path = crate::mcp_config::json_path(&workspace);
    let json_servers = crate::mcp_config::read_project_json(&json_path);
    config
        .warnings
        .extend(local_servers.warnings.iter().cloned());
    config
        .warnings
        .extend(json_servers.warnings.iter().cloned());

    for layer in &layers {
        let flags_only = layer.slot == Slot::Cli && cli_path.is_none();
        if flags_only && !flag_rules {
            continue;
        }
        if layer.slot == Slot::ProjectLocal && local_servers.file_present {
            config.scopes.push(ScopeEntry {
                slot: Slot::Local,
                column: Some(Column::User),
                path: Some(local_path.clone()),
                loaded: local_servers.loaded,
            });
        }
        config.scopes.push(ScopeEntry {
            slot: layer.slot,
            column: Some(layer.column),
            path: (!flags_only).then(|| layer.path.clone()),
            loaded: layer.settings.is_some(),
        });
        if layer.slot == Slot::Project && json_servers.file_present {
            config.scopes.push(ScopeEntry {
                slot: Slot::Project,
                column: Some(Column::Project),
                path: Some(json_path.clone()),
                loaded: json_servers.loaded,
            });
        }
    }

    // Name collisions resolve by position: user, then `.mcp.json`, then the project's settings,
    // then the project-local file, then `mcp-local.toml`, then `--settings`.
    let toml_project_servers: Vec<String> = layers
        .iter()
        .find(|l| l.slot == Slot::Project)
        .and_then(|l| l.settings.as_ref())
        .and_then(|s| s.mcp.as_ref())
        .and_then(|m| m.servers.as_ref())
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default();
    for layer in layers.iter().rev() {
        if layer.slot == Slot::Cli {
            for (name, server) in &local_servers.servers {
                config.mcp_servers.insert(name.clone(), server.clone());
                config.mcp_origins.insert(
                    name.clone(),
                    Origin {
                        slot: Slot::Local,
                        user_trust: true,
                    },
                );
            }
        }
        if layer.slot == Slot::Project {
            for (name, server) in &json_servers.servers {
                if toml_project_servers.contains(name) {
                    config.warnings.push(format!(
                        "MCP server `{name}` is defined in both .mcp.json and .fontaine/settings.toml; the settings.toml definition is used"
                    ));
                }
                config.mcp_servers.insert(name.clone(), server.clone());
                config.mcp_origins.insert(
                    name.clone(),
                    Origin {
                        slot: Slot::Project,
                        user_trust: false,
                    },
                );
            }
        }
        if let Some(settings) = &layer.settings {
            config.apply(settings, layer.slot, layer.column);
        }
    }
    config.sources = collect_sources(
        layers
            .iter()
            .filter_map(|l| l.table.as_ref().map(|t| (l.slot, t))),
    );

    config.main_source = main_source(&config, &layers)?;
    check_network(&config)?;
    check_scanner(&config.guardian, &user_path)?;
    check_composition(&mut config, &user_path, params.config_dir)?;
    let roles = crate::model_roles::Roles::from_config(&config)?;
    for (name, ty) in &config.agents.types {
        if let Some(model) = &ty.model
            && let Ok(spec @ (ModelSpec::Named(_) | ModelSpec::List(_))) =
                ModelSpec::from_entries(model.entries())
        {
            roles.entries_of(&spec).map_err(|why| {
                ConfigError::model(format!("[agents.types.{name}] `model`: {why}"))
            })?;
        }
    }
    check_composition_credentials(&config, &user_path, params)?;
    resolve_telemetry(&config.telemetry, params)?;
    check_websearch(&config.websearch)?;
    config.websearch_config(params.env, params.data_dir)?;
    Ok(config)
}

/// Whether `[model] model` or `[models.roles] main` names the main models. Both are the one
/// scalar, so a layer setting both is refused and across layers the higher precedence wins.
fn main_source(config: &Config, layers: &[Layer]) -> Result<MainSource, ConfigError> {
    let slots = |key: &str| {
        config
            .sources
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    let (model, role) = (slots("model.model"), slots("models.roles.main"));
    if let Some(slot) = model.iter().find(|slot| role.contains(slot)) {
        let path = layers
            .iter()
            .find(|layer| layer.slot == *slot)
            .map(|layer| layer.path.clone())
            .unwrap_or_default();
        return Err(ConfigError::invalid(
            &path,
            "model.model",
            "set together with `models.roles.main` in the same file; `[model] model` is shorthand \
             for the `main` role, so set one",
        ));
    }
    let model_wins = match (model.first(), role.first()) {
        (None, None) => return Ok(MainSource::Unset),
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(m), Some(r)) => crate::config_catalog::rank(*m) < crate::config_catalog::rank(*r),
    };
    Ok(match (&config.model, model_wins) {
        (_, false) => MainSource::Role,
        (Some(OneOrMany::Many(_)), true) => MainSource::ModelKeyList,
        (_, true) => MainSource::ModelKeyString,
    })
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

/// The composition comes only from the user file, so that is where an invalid one is reported. It
/// must name exactly one of a preset and a script, read with models on Ollama or OpenRouter accounts, and
/// compile; a configured composition that cannot be built stops startup rather than vanishing.
/// `user_path` names the file in errors, and a relative script path resolves against `config_dir`.
pub fn check_composition(
    config: &mut Config,
    user_path: &Path,
    config_dir: &Path,
) -> Result<(), ConfigError> {
    let forced = config.guardian.composition_shadow_forced;
    let Some(composition) = &mut config.guardian.composition else {
        return Ok(());
    };
    let bad = |key: &str, message: &str| Err(ConfigError::invalid(user_path, key, message));
    match (&composition.preset, &composition.script) {
        (Some(_), Some(_)) => {
            return bad(
                "guardian.composition.script",
                "set exactly one of `preset` and `script`",
            );
        }
        (None, None) => {
            return bad(
                "guardian.composition.preset",
                "set one of `preset` and `script`",
            );
        }
        _ => {}
    }
    if composition.preset.is_some() && !composition.reads.is_empty() {
        return bad(
            "guardian.composition.reads",
            "a preset declares its own reads; `reads` is for scripts",
        );
    }
    if composition.preset.is_some() && composition.model.is_none() {
        return bad(
            "guardian.composition.model",
            "the composition requires `model`",
        );
    }
    if forced {
        composition.shadow = true;
    }
    if let Some(script) = &composition.script {
        let file = config_dir.join(script);
        let text = std::fs::read_to_string(&file).map_err(|e| {
            ConfigError::invalid(
                user_path,
                "guardian.composition.script",
                format!(
                    "cannot read {}: {}",
                    file.display(),
                    fontaine_tools::error_chain::chain(&e)
                ),
            )
        })?;
        composition.script_source = Some(text);
    }
    let spec = composition
        .spec(&config.accounts)
        .map_err(|e| ConfigError::invalid(user_path, &e.key, &e.message))?;
    let models = spec.read_models().map_err(|e| {
        ConfigError::invalid(user_path, composition_key(composition, &e), &e.message)
    })?;
    for model in models {
        let key = model_key(composition, &model);
        let account = model.split_once('/').map_or("", |(a, _)| a);
        let Some(configured) = config.accounts.get(account) else {
            return bad(&key, &format!("account `{account}` is not configured"));
        };
        match backend_of(configured.kind) {
            Some(fontaine_guardian::BackendKind::Ollama) => {
                if let Some(base) = configured.base_url.as_deref().filter(|u| !u.is_empty()) {
                    let usable = url::Url::parse(base)
                        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host());
                    if !usable {
                        return bad(
                            &format!("accounts.{account}.base_url"),
                            "the guardian composition requires an http or https URL with a host",
                        );
                    }
                }
            }
            Some(fontaine_guardian::BackendKind::SystemOne) => {
                let endpoint = systemone_endpoint(configured).map_err(|e| {
                    ConfigError::invalid(user_path, format!("accounts.{account}.base_url"), e)
                })?;
                if !fontaine_guardian::systemone::key_may_travel(&endpoint) {
                    return bad(
                        &format!("accounts.{account}.base_url"),
                        "the guardian composition sends the account's key only over https or to a loopback host",
                    );
                }
            }
            None => {
                return bad(
                    &key,
                    &format!("account `{account}` is not of kind = \"ollama\" or \"openrouter\""),
                );
            }
        }
    }
    fontaine_guardian::composition::Composition::compile(spec, Vec::new()).map_err(|e| {
        ConfigError::invalid(user_path, composition_key(composition, &e), &e.message)
    })?;
    Ok(())
}

/// A hosted screen model needs its account's key now, not at the first call: `check_composition`
/// lacks the environment and the stored credentials, so this runs after it. A key that does not
/// resolve stops startup rather than quietly leaving the screen unable to read.
fn check_composition_credentials(
    config: &Config,
    user_path: &Path,
    params: &LoadParams<'_>,
) -> Result<(), ConfigError> {
    let Some(composition) = &config.guardian.composition else {
        return Ok(());
    };
    let spec = composition
        .spec(&config.accounts)
        .map_err(|e| ConfigError::invalid(user_path, &e.key, &e.message))?;
    let models = spec.read_models().map_err(|e| {
        ConfigError::invalid(user_path, composition_key(composition, &e), &e.message)
    })?;
    let mut stored = None;
    for model in models {
        let account = model.split_once('/').map_or("", |(a, _)| a);
        let Some(configured) = config.accounts.get(account) else {
            continue;
        };
        if backend_of(configured.kind) != Some(fontaine_guardian::BackendKind::SystemOne) {
            continue;
        }
        if stored.is_none() {
            stored = Some(credentials::Credentials::load(params.data_dir)?);
        }
        let stored = stored.as_ref().expect("the credentials were just loaded");
        screen_key(account, configured, params.env, stored, false).map_err(|message| {
            ConfigError::invalid(user_path, model_key(composition, &model), message)
        })?;
    }
    Ok(())
}

/// The settings key that supplies a read model: the read's own `model`, else the section's.
fn model_key(composition: &GuardianComposition, model: &str) -> String {
    composition
        .reads
        .iter()
        .find(|(_, read)| read.model.as_deref() == Some(model))
        .map_or_else(
            || "guardian.composition.model".to_owned(),
            |(name, _)| format!("guardian.composition.reads.{name}.model"),
        )
}

/// The settings key a composition build error points at.
fn composition_key(
    composition: &GuardianComposition,
    error: &fontaine_guardian::composition::CompositionError,
) -> String {
    match (&error.key, composition.preset.is_some()) {
        (Some(key), true) => format!("guardian.composition.params.{key}"),
        (Some(key), false) => format!("guardian.composition.reads.{key}"),
        (None, true) => "guardian.composition.preset".to_owned(),
        (None, false) => "guardian.composition.script".to_owned(),
    }
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
        self.websearch_config_with_notice(
            env,
            data_dir,
            std::sync::Arc::new(|_| {}),
            std::sync::Arc::new(|_| {}),
        )
    }

    /// [`Config::websearch_config`], with `notice` told when the default container is started
    /// and `warn` told when a leftover or unhealthy container cannot be removed.
    pub fn websearch_config_with_notice(
        &self,
        env: EnvLookup<'_>,
        data_dir: &Path,
        notice: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
        warn: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Option<fontaine_tools::websearch::WebsearchConfig>, ConfigError> {
        use fontaine_tools::websearch::{BackendConfig, Launch, WebsearchConfig};
        let w = &self.websearch;
        let backend = match w.backend {
            Some(WebsearchBackend::Off) => return Ok(None),
            None => {
                let Some(runtime) = self.default_search_runtime(env) else {
                    return Ok(None);
                };
                BackendConfig::Managed(crate::managed_search::managed(
                    runtime,
                    data_dir.to_path_buf(),
                    fontaine_tools::child_env::container_runtime_env(env),
                    notice,
                    warn,
                ))
            }
            Some(WebsearchBackend::Tavily) => {
                BackendConfig::tavily(websearch_key(w, env, data_dir)?)
            }
            Some(WebsearchBackend::Searxng) => BackendConfig::Searxng {
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
    #[test]
    fn hook_matcher_wildcards_only_work_as_the_whole_matcher() {
        let start = crate::config::HookEvent::SessionStart;
        assert!(start.check_matcher("*").is_ok());
        assert!(start.check_matcher("startup|resume").is_ok());
        assert!(start.check_matcher("startup|*").is_err());
        assert!(start.check_matcher("startup|").is_err());
        assert!(
            crate::config::HookEvent::SubagentStart
                .check_matcher("a|*")
                .is_err()
        );
    }

    use super::{Account, AccountKind, AutoMode, case_colliding_env_name, systemone_endpoint};
    use fontaine_guardian::rules::{DEFAULT_ENVIRONMENT, DEFAULT_HARD_DENY};

    fn config_with(auto_mode: AutoMode) -> super::Config {
        let mut config = super::Config::defaults(std::path::PathBuf::from("."));
        config.auto_mode = auto_mode;
        config
    }

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_string()).collect()
    }

    #[test]
    fn afk_is_a_setting_and_decides_as_auto() {
        use super::PermissionMode::{Afk, Auto, Manual};
        let settings =
            toml::from_str::<super::Settings>("[permissions]\ndefault_mode = \"afk\"\n").unwrap();
        assert_eq!(settings.permissions.unwrap().default_mode, Some(Afk));
        assert_eq!(
            (Afk.as_str(), Afk.rules(), Auto.rules(), Manual.rules()),
            ("afk", Auto, Auto, Manual)
        );
    }

    #[test]
    fn effective_auto_mode_splices_replaces_and_only_tightens() {
        let (environment, rules) = config_with(AutoMode {
            environment: s(&["$defaults", "mine"]),
            allow: s(&["only mine"]),
            soft_deny: Vec::new(),
            hard_deny: s(&["extra"]),
            setup_reminder: true,
        })
        .effective_auto_mode();
        assert_eq!(environment.entries.len(), DEFAULT_ENVIRONMENT.len() + 1);
        assert_eq!(rules.allow, s(&["only mine"]));
        assert_eq!(rules.hard_deny.len(), DEFAULT_HARD_DENY.len() + 1);
        assert_eq!(rules.hard_deny[0], DEFAULT_HARD_DENY[0]);
    }

    #[test]
    fn keybindings_merge_canonically_so_the_higher_layer_wins() {
        let user = super::Settings {
            keybindings: Some(
                [("ctrl+t".to_string(), "todos".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let cli = super::Settings {
            keybindings: Some(
                [("Ctrl+T".to_string(), "reprint".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let mut config = super::Config::defaults(std::path::PathBuf::from("."));
        config.apply(&user, super::Slot::User, super::Column::User);
        config.apply(&cli, super::Slot::Cli, super::Column::Cli);
        // The equivalent spelling from the higher-precedence layer replaces, not coexists.
        assert_eq!(config.keybindings.len(), 1);
        assert_eq!(config.keybindings["ctrl+t"], "reprint");
    }

    #[test]
    fn statusline_refresh_requires_a_command() {
        let check = |text: &str| {
            super::validate_settings(
                text,
                std::path::Path::new("config.toml"),
                super::Column::User,
            )
        };
        assert!(check("[statusline]\nrefresh_interval_secs = 5\n").is_err());
        assert!(check("[statusline]\ncommand = \" \"\nrefresh_interval_secs = 5\n").is_err());
        assert!(check("[statusline]\ncommand = \"date\"\nrefresh_interval_secs = 5\n").is_ok());
    }

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

    fn openrouter(base_url: Option<&str>) -> Account {
        Account::new(
            AccountKind::named("openrouter"),
            base_url.map(str::to_owned),
        )
    }

    #[test]
    fn the_systemone_endpoint_joins_onto_the_chat_base_url() {
        for (base, expected) in [
            (None, "https://openrouter.ai/api/v1/systemone"),
            (
                Some("https://openrouter.ai/api/v1"),
                "https://openrouter.ai/api/v1/systemone",
            ),
            (
                Some("https://openrouter.ai/api/v1/"),
                "https://openrouter.ai/api/v1/systemone",
            ),
            (
                Some("http://127.0.0.1:8000/api/v1"),
                "http://127.0.0.1:8000/api/v1/systemone",
            ),
        ] {
            let url = systemone_endpoint(&openrouter(base)).unwrap();
            assert_eq!(url.as_str(), expected, "{base:?}");
        }
        for bad in ["not a url", "ftp://host/", "http://"] {
            systemone_endpoint(&openrouter(Some(bad))).unwrap_err();
        }
    }
}
