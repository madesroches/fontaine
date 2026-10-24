use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::credentials;
use crate::permission_rules::{RuleError, RuleSource, SourcedRule, parse_rule, validate_mcp_name};
use crate::providers::{self, Provider};
use crate::trust::{self, LocalClassification, ProjectDemotion};
use fontaine_tools::names::{is_builtin, is_mcp};

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
            ConfigError::Model { message } | ConfigError::Usage { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } | ConfigError::Write { source, .. } => Some(source),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
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
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    Chat,
    Responses,
    Messages,
    Gemini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardianProfile {
    Full,
    Minimal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scanner {
    Off,
    Pg2,
    Llm,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum AgentMode {
    #[serde(rename = "inherit")]
    Inherit,
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "dontAsk")]
    DontAsk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryBackend {
    Micromegas,
    Otlp,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub kind: AccountKind,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub models: Option<Vec<String>>,
    /// Per-model wire overrides, for kinds that route one account over several wires.
    pub wires: Option<BTreeMap<String, Wire>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSection {
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
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
#[derive(Clone, Debug, Default, Deserialize)]
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

impl GuardianSection {
    fn scanner_keys_set(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.scanner.is_some() {
            keys.push("scanner");
        }
        if self.scanner_command.is_some() {
            keys.push("scanner_command");
        }
        if self.scanner_model_path.is_some() {
            keys.push("scanner_model_path");
        }
        if self.scanner_hf_token_env.is_some() {
            keys.push("scanner_hf_token_env");
        }
        keys
    }

    fn other_keys_set(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.model.is_some() {
            keys.push("model");
        }
        if self.profile.is_some() {
            keys.push("profile");
        }
        if self.timeout_secs.is_some() {
            keys.push("timeout_secs");
        }
        if self.confidence_threshold.is_some() {
            keys.push("confidence_threshold");
        }
        if self.cache.is_some() {
            keys.push("cache");
        }
        keys
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
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

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksSection {
    pub pre_tool_use: Option<Vec<Hook>>,
    pub post_tool_use: Option<Vec<Hook>>,
    pub permission_denied: Option<Vec<Hook>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_vars: Option<Vec<String>>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    pub servers: Option<BTreeMap<String, McpServer>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoModeSection {
    pub environment: Option<Vec<String>>,
    pub allow: Option<Vec<String>>,
    pub soft_deny: Option<Vec<String>>,
    pub hard_deny: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentType {
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub mode: Option<AgentMode>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsSection {
    pub max_parallel: Option<u32>,
    pub subagent_token_budget: Option<u64>,
    pub subagent_cost_budget_usd: Option<f64>,
    pub session_token_budget: Option<u64>,
    pub session_cost_budget_usd: Option<f64>,
    pub types: Option<BTreeMap<String, AgentType>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellPreclassifierSection {
    pub allow_extra: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
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

impl TelemetrySection {
    fn keys_other_than_enabled(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        let flags: [(&'static str, bool); 10] = [
            ("backend", self.backend.is_some()),
            ("endpoint", self.endpoint.is_some()),
            ("api_key_env", self.api_key_env.is_some()),
            ("traces", self.traces.is_some()),
            ("log_user_prompts", self.log_user_prompts.is_some()),
            (
                "log_assistant_responses",
                self.log_assistant_responses.is_some(),
            ),
            ("log_tool_details", self.log_tool_details.is_some()),
            ("log_tool_content", self.log_tool_content.is_some()),
            ("content_max_bytes", self.content_max_bytes.is_some()),
            ("resource_attributes", self.resource_attributes.is_some()),
        ];
        for (name, set) in flags {
            if set {
                keys.push(name);
            }
        }
        keys
    }
}

/// One settings file, exactly as written.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub accounts: Option<BTreeMap<String, Account>>,
    pub model: Option<ModelSection>,
    pub guardian: Option<GuardianSection>,
    pub permissions: Option<PermissionsSection>,
    pub hooks: Option<HooksSection>,
    pub mcp: Option<McpSection>,
    pub auto_mode: Option<AutoModeSection>,
    pub agents: Option<AgentsSection>,
    pub shell_preclassifier: Option<ShellPreclassifierSection>,
    pub telemetry: Option<TelemetrySection>,
}

/// Parses one settings file and runs every check that does not depend on the rest of the
/// environment: schema, per-scope key rules and value shapes. Phase-gated value rules and
/// env/credential resolution are left to [`load`].
pub fn parse_settings(text: &str, path: &Path, column: Column) -> Result<Settings, ConfigError> {
    parse_settings_with_hint(text, path, column, None)
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
) -> Result<Settings, ConfigError> {
    let settings: Settings = toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    if let Err((key, reason)) = check_scope(&settings, column) {
        return Err(ConfigError::Scope {
            path: path.to_path_buf(),
            column,
            key,
            reason,
            hint: hint.map(str::to_string),
        });
    }
    validate_values(&settings, path)?;
    Ok(settings)
}

type ScopeFailure = (String, String);

fn check_scope(s: &Settings, column: Column) -> Result<(), ScopeFailure> {
    if column == Column::User {
        return Ok(());
    }
    let fail = |key: &str, reason: &str| -> Result<(), ScopeFailure> {
        Err((key.to_string(), reason.to_string()))
    };

    if let Some(g) = &s.guardian
        && let Some(key) = g.scanner_keys_set().first()
    {
        return fail(
            &format!("guardian.{key}"),
            "scanner settings come only from the user file",
        );
    }
    if let Some(screen) = s.guardian.as_ref().and_then(|g| g.screen.as_ref())
        && screen.loosens()
    {
        return fail(
            "guardian.screen",
            "the screen stage is configured only in the user file; other scopes may only set `shadow = true`",
        );
    }
    if column == Column::Cli {
        return Ok(());
    }

    if s.accounts.is_some() {
        return fail(
            "accounts",
            "endpoint and credential configuration comes only from the user file or --settings",
        );
    }
    if let Some(p) = &s.permissions
        && let Some(mode) = p.default_mode
        && !matches!(mode, PermissionMode::Manual | PermissionMode::DontAsk)
    {
        return fail(
            "permissions.default_mode",
            "only `manual` and `dontAsk` are accepted here",
        );
    }
    if let Some(t) = &s.telemetry {
        if let Some(key) = t.keys_other_than_enabled().first() {
            return fail(
                &format!("telemetry.{key}"),
                "only `enabled = false` may be set here",
            );
        }
        if t.enabled == Some(true) {
            return fail(
                "telemetry.enabled",
                "only `enabled = false` may be set here",
            );
        }
    }
    if column == Column::ProjectLocal {
        return Ok(());
    }

    let tighten = "shared project settings may only tighten";
    if s.model.is_some() {
        return fail("model", tighten);
    }
    if let Some(g) = &s.guardian
        && let Some(key) = g.other_keys_set().first()
    {
        return fail(&format!("guardian.{key}"), tighten);
    }
    if let Some(p) = &s.permissions {
        if p.allow.is_some() {
            return fail("permissions.allow", tighten);
        }
        if p.additional_directories.is_some() {
            return fail("permissions.additional_directories", tighten);
        }
    }
    if let Some(a) = &s.auto_mode {
        if a.environment.is_some() {
            return fail("auto_mode.environment", tighten);
        }
        if a.allow.is_some() {
            return fail("auto_mode.allow", tighten);
        }
    }
    if s.agents.is_some() {
        return fail("agents", tighten);
    }
    if s.shell_preclassifier.is_some() {
        return fail("shell_preclassifier", tighten);
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
    provider
        .validate(account)
        .map_err(|(key, message)| (format!("accounts.{name}.{key}"), message))
}

fn validate_values(s: &Settings, path: &Path) -> Result<(), ConfigError> {
    let bad = |key: &str, msg: &str| Err(ConfigError::invalid(path, key, msg));

    for (name, account) in s.accounts.iter().flatten() {
        if let Err((key, message)) = check_account(name, account) {
            return bad(&key, &message);
        }
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

/// The merged, validated configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub workspace: PathBuf,
    pub scopes: Vec<ScopeEntry>,
    pub accounts: BTreeMap<String, Account>,
    pub model: Option<String>,
    pub guardian: Guardian,
    pub permissions: Permissions,
    pub hooks: Hooks,
    pub mcp_servers: BTreeMap<String, McpServer>,
    /// Where each server in `mcp_servers` was defined.
    pub mcp_origins: BTreeMap<String, Origin>,
    pub auto_mode: AutoMode,
    pub agents: Agents,
    pub shell_preclassifier_allow_extra: Vec<String>,
    pub telemetry: Telemetry,
}

impl Config {
    fn defaults(workspace: PathBuf) -> Self {
        Config {
            workspace,
            scopes: Vec::new(),
            accounts: BTreeMap::new(),
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
            shell_preclassifier_allow_extra: Vec::new(),
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
            self.accounts.insert(name.clone(), account.clone());
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
}

pub fn load(params: &LoadParams<'_>) -> Result<Config, ConfigError> {
    let config = load_unresolved(params)?;
    resolve_telemetry(&config.telemetry, params)?;
    Ok(config)
}

fn load_unresolved(params: &LoadParams<'_>) -> Result<Config, ConfigError> {
    let workspace =
        std::fs::canonicalize(params.launch_dir).map_err(|source| ConfigError::Read {
            path: params.launch_dir.to_path_buf(),
            source,
        })?;

    let user_path = params.config_dir.join(USER_FILE);
    let user = read_optional(&user_path)?
        .map(|text| parse_settings(&text, &user_path, Column::User))
        .transpose()?;

    let project_path = workspace.join(PROJECT_DIR).join(PROJECT_FILE);
    let project = read_optional(&project_path)?
        .map(|text| parse_settings(&text, &project_path, Column::Project))
        .transpose()?;

    let local_path = workspace.join(PROJECT_DIR).join(PROJECT_LOCAL_FILE);
    let (local_column, local) = match read_optional(&local_path)? {
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
            Some(parse_settings(&text, path, Column::Cli)?)
        }
    };
    let flag_rules = !params.cli_rules.allow.is_empty() || !params.cli_rules.deny.is_empty();
    let cli = if flag_rules {
        let mut settings = cli.unwrap_or_default();
        let permissions = settings.permissions.get_or_insert_with(Default::default);
        for (flag, from, into) in [
            (
                "--allowed-tools",
                params.cli_rules.allow,
                &mut permissions.allow,
            ),
            (
                "--disallowed-tools",
                params.cli_rules.deny,
                &mut permissions.deny,
            ),
        ] {
            for text in from {
                parse_rule(text).map_err(|e| ConfigError::usage(format!("{flag}: {e}")))?;
                into.get_or_insert_with(Vec::new).push(text.clone());
            }
        }
        Some(settings)
    } else {
        cli
    };

    let layers = [
        Layer {
            slot: Slot::Cli,
            column: Column::Cli,
            path: cli_path.map(Path::to_path_buf).unwrap_or_default(),
            settings: cli,
        },
        Layer {
            slot: Slot::ProjectLocal,
            column: local_column,
            path: local_path,
            settings: local,
        },
        Layer {
            slot: Slot::Project,
            column: Column::Project,
            path: project_path,
            settings: project,
        },
        Layer {
            slot: Slot::User,
            column: Column::User,
            path: user_path.clone(),
            settings: user,
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

    check_scanner(&config.guardian, &user_path)?;
    check_screen(&mut config, &user_path)?;
    Ok(config)
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

fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}
