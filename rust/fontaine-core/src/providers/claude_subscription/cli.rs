//! The Claude CLI as a program: finding it, checking its version and login, launching it with a
//! rebuilt environment, and the child process guard that kills its whole tree.
//!
//! Every launch uses a fixed argument vector and no shell, starts from an empty environment and
//! keeps the CLI's output out of logs and errors.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::OnceCell;

use super::context::Verdicts;
use super::protocol::{Event, Failure, Reader};
use super::transport::{next, rejected};
use super::{ClaudeError, ErrorKind, USAGE_SETTINGS_URL, claude_error};
use crate::models::SharedEnv;
use crate::owner_only;
use crate::providers::ExternalLogin;
use crate::providers::derived_facts::Capabilities;
use crate::providers::warn::Warn;
use fontaine_tools::lock::lock_recovering;
use fontaine_tools::procgroup;

/// The oldest CLI accepted. This is a minimum only: newer versions are accepted unqualified (no
/// version is live-qualified yet), and a protocol shape the adapter does not know still fails
/// closed.
pub const MIN_VERSION: (u32, u32, u32) = (2, 1, 318);

/// Variables that, inherited, would change which account, backend or model the CLI uses. They
/// are refused by name rather than scrubbed, so the required change is visible and the billing
/// cannot switch mid-session.
pub const CONFLICTING_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_UNIX_SOCKET",
    "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_BETAS",
    "CLAUDE_CODE_EXTRA_BODY",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "ANTHROPIC_VERTEX_BASE_URL",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_CONFIG_DIR",
    "ANTHROPIC_IDENTITY_TOKEN",
    "ANTHROPIC_IDENTITY_TOKEN_FILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_EFFORT_LEVEL",
    "CLAUDE_CODE_FALLBACK_MODEL",
    "ANTHROPIC_FALLBACK_MODEL",
];

/// The inherited variables the CLI needs to run, authenticate with its own stored login and reach
/// the network: identity, paths, locale, proxy and TLS settings and the CLI's own opt-outs.
const PASSTHROUGH_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USER",
    "LOGNAME",
    "USERNAME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "CLAUDE_CONFIG_DIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "all_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CLAUDE_CODE_CLIENT_CERT",
    "CLAUDE_CODE_CLIENT_KEY",
    "CLAUDE_CODE_CLIENT_KEY_PASSPHRASE",
    "CLAUDE_CODE_CERT_STORE",
    "DISABLE_TELEMETRY",
    "DISABLE_ERROR_REPORTING",
];

/// What the CLI is always told, whatever the user's environment says: no native memory,
/// instructions, compaction, retries, fallbacks, background work, updates or telemetry, and no
/// identity but ours.
const FIXED_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENTRYPOINT", "fontaine"),
    ("CLAUDE_CODE_MCP_ALLOWLIST_ENV", "1"),
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
    ("DISABLE_AUTO_COMPACT", "1"),
    ("DISABLE_COMPACT", "1"),
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("DISABLE_ERROR_REPORTING", "1"),
    ("DISABLE_FEEDBACK_COMMAND", "1"),
    ("DISABLE_BUG_COMMAND", "1"),
    ("DISABLE_AUTOUPDATER", "1"),
    ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
    ("CLAUDE_CODE_DISABLE_CRON", "1"),
    ("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1"),
    ("CLAUDE_CODE_DISABLE_ARTIFACT", "1"),
    ("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK", "1"),
    ("CLAUDE_CODE_MAX_RETRIES", "0"),
    ("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION", "false"),
    ("ENABLE_TOOL_SEARCH", "false"),
    ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
    // The CLI's own breakpoint sits on a trailing date/budget block that is never replayed, so
    // it is never read back; 5 min makes that unread write cheaper. The adapter's 1 h marker is
    // the one that is read: on the final message in the native layout, on the last replayed
    // user message otherwise.
    ("CLAUDE_CODE_MESSAGES_PROMPT_CACHE_TTL", "5m"),
];

/// Every inherited variable this adapter looks at: the ones it forwards and the ones it refuses.
/// A caller that owns its environment (the eval harness) carries exactly these over.
pub fn inherited_env_names() -> Vec<&'static str> {
    PASSTHROUGH_ENV
        .iter()
        .chain(CONFLICTING_ENV)
        .copied()
        .collect()
}

/// Bounds for the processes this adapter runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// How long a process may be silent before it is given up on; any valid output resets it.
    pub read_idle: Duration,
    /// The outer bound of the version probe.
    pub version: Duration,
    /// The outer bound of the login probe.
    pub auth: Duration,
    /// The outer bound of model discovery, and of the request-time model listing on a request's
    /// own process.
    pub discovery: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            read_idle: Duration::from_secs(180),
            version: Duration::from_secs(30),
            auth: Duration::from_secs(60),
            discovery: Duration::from_secs(120),
        }
    }
}

impl Timeouts {
    /// The defaults, with the test knobs applied: `FONTAINE_CLAUDE_READ_IDLE_MS` for the idle
    /// deadline and `FONTAINE_CLAUDE_PROBE_MS` for the three outer bounds. They exist so a
    /// hang can be exercised in milliseconds and change nothing about routing or billing.
    pub fn from_env(env: &dyn Fn(&str) -> Option<OsString>) -> Self {
        let millis = |name: &str| {
            env(name)
                .and_then(|v| v.into_string().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|ms| *ms > 0)
                .map(Duration::from_millis)
        };
        let mut timeouts = Timeouts::default();
        if let Some(idle) = millis("FONTAINE_CLAUDE_READ_IDLE_MS") {
            timeouts.read_idle = idle;
        }
        if let Some(probe) = millis("FONTAINE_CLAUDE_PROBE_MS") {
            timeouts.version = probe;
            timeouts.auth = probe;
            timeouts.discovery = probe;
        }
        timeouts
    }
}

/// One model the CLI's `initialize` answer lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    /// The id a user selects (`sonnet`).
    pub id: String,
    /// The id both rig and the CLI receive (`claude-sonnet-5-5`).
    pub canonical: String,
    /// Whether the row says the model draws from paid usage credits.
    pub credits: bool,
    /// What the row says about the model's reasoning controls.
    pub capabilities: Capabilities,
}

/// The directory, inside Fontaine's data directory, every CLI process runs in.
pub const WORKDIR_NAME: &str = "claude-cli-cwd";

/// Entries that would put instructions into the CLI's prompt.
const INSTRUCTION_ENTRIES: [&str; 3] = ["CLAUDE.md", "AGENTS.md", ".claude"];

/// The command that signs a Claude account in, built from the same program and config directory
/// the probes use. `env` supplies the directory when the account sets none, as the probes read
/// it.
pub fn login_command(
    cli_path: Option<&Path>,
    config_dir: Option<&Path>,
    env: &SharedEnv,
) -> ExternalLogin {
    let program = cli_path.map_or_else(|| PathBuf::from("claude"), Path::to_path_buf);
    let dir = config_dir
        .map(|dir| dir.as_os_str().to_os_string())
        .or_else(|| env("CLAUDE_CONFIG_DIR"));
    let args: Vec<OsString> = ["auth", "login", "--claudeai"].map(OsString::from).into();
    let mut display = String::new();
    let mut vars = Vec::new();
    if let Some(dir) = dir {
        display.push_str(&format!("CLAUDE_CONFIG_DIR={} ", dir.to_string_lossy()));
        vars.push((OsString::from("CLAUDE_CONFIG_DIR"), dir));
    }
    display.push_str(&format!("{} auth login --claudeai", program.display()));
    ExternalLogin {
        program,
        args,
        env: vars,
        display,
    }
}

pub struct Cli {
    program: PathBuf,
    config_dir: Option<PathBuf>,
    data_dir: PathBuf,
    workdir: PathBuf,
    env: SharedEnv,
    pub timeouts: Timeouts,
    version_ok: OnceCell<()>,
    warn: Warn,
    verdicts: Arc<Verdicts>,
}

impl Cli {
    pub fn new(
        cli_path: Option<PathBuf>,
        config_dir: Option<PathBuf>,
        data_dir: &Path,
        env: SharedEnv,
        timeouts: Timeouts,
        warn: Warn,
    ) -> Self {
        let verdicts = Verdicts::shared(data_dir, &warn);
        Cli {
            program: cli_path.unwrap_or_else(|| PathBuf::from("claude")),
            config_dir,
            data_dir: data_dir.to_path_buf(),
            workdir: data_dir.join(WORKDIR_NAME),
            env,
            timeouts,
            version_ok: OnceCell::new(),
            warn,
            verdicts,
        }
    }

    pub(super) fn verdicts(&self) -> &Arc<Verdicts> {
        &self.verdicts
    }

    pub(crate) fn warn(&self) -> &Warn {
        &self.warn
    }

    /// The one directory every CLI process runs in. The CLI writes its working directory into
    /// the prompt, so a stable, empty one keeps the prompt prefix the same from request to
    /// request.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn refuse_instruction_files(&self) -> Result<(), ClaudeError> {
        let unreadable = |e: std::io::Error| {
            ClaudeError::new(
                ErrorKind::Cli,
                format!(
                    "could not read the Claude CLI's working directory {}",
                    self.workdir.display()
                ),
            )
            .with_source(e)
        };
        for entry in std::fs::read_dir(&self.workdir).map_err(unreadable)? {
            let entry = entry.map_err(unreadable)?;
            let name = entry.file_name();
            if INSTRUCTION_ENTRIES
                .iter()
                .any(|held| name.eq_ignore_ascii_case(held))
            {
                return Err(ClaudeError::new(
                    ErrorKind::Cli,
                    format!(
                        "the Claude CLI's working directory {} holds {}, which would put instructions into the model's prompt; empty the directory",
                        self.workdir.display(),
                        name.to_string_lossy()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Refuses when an inherited variable would change the account or backend that bills. Only
    /// presence is checked, through the live lookup, so a variable set after connecting is seen
    /// and no value is ever read.
    pub fn check_environment(&self) -> Result<(), ClaudeError> {
        let present: Vec<&str> = CONFLICTING_ENV
            .iter()
            .copied()
            .filter(|name| (self.env)(name).is_some())
            .collect();
        if present.is_empty() {
            return Ok(());
        }
        Err(ClaudeError::new(
            ErrorKind::Environment,
            format!(
                "the environment sets {}, which would change how the Claude CLI authenticates or bills; unset {} and start Fontaine again",
                present.join(", "),
                if present.len() == 1 { "it" } else { "them" }
            ),
        ))
    }

    /// The command that signs this account in, for a caller that owns the terminal.
    pub fn login(&self) -> ExternalLogin {
        login_command(Some(&self.program), self.config_dir.as_deref(), &self.env)
    }

    /// `claude --version`, once per connected client.
    pub async fn check_version(&self) -> Result<(), ClaudeError> {
        self.version_ok
            .get_or_try_init(|| async {
                let out = self
                    .probe(&["--version"], self.timeouts.version, ErrorKind::Version)
                    .await?;
                let text = String::from_utf8_lossy(&out.stdout);
                let version = parse_version(&text).ok_or_else(|| {
                    ClaudeError::new(
                        ErrorKind::Version,
                        "the Claude CLI did not report a version Fontaine understands; is `cli_path` the official `claude` executable?",
                    )
                })?;
                if !out.success {
                    return Err(ClaudeError::new(
                        ErrorKind::Version,
                        "the Claude CLI's version probe failed",
                    ));
                }
                if version < MIN_VERSION {
                    return Err(ClaudeError::new(
                        ErrorKind::Version,
                        format!(
                            "Claude CLI {}.{}.{} is older than the supported minimum {}.{}.{}; update it with `claude update`",
                            version.0, version.1, version.2, MIN_VERSION.0, MIN_VERSION.1, MIN_VERSION.2
                        ),
                    ));
                }
                Ok(())
            })
            .await
            .map(|_| ())
    }

    /// `claude auth status`: signed in with `claude.ai` on a Max or Ultra plan. Only three
    /// fields are read. A failure says what is wrong and nothing of what to run: the surface
    /// that shows it knows how the user signs in from there.
    pub async fn check_login(&self) -> Result<(), ClaudeError> {
        self.check_login_with(false).await
    }

    /// [`Cli::check_login`] for a request already under way, where no surface adds a hint:
    /// the failure names the login command itself.
    pub async fn check_login_hinted(&self) -> Result<(), ClaudeError> {
        self.check_login_with(true).await
    }

    async fn check_login_with(&self, hinted: bool) -> Result<(), ClaudeError> {
        let out = self
            .probe(
                &["auth", "status"],
                self.timeouts.auth,
                ErrorKind::SetupTimeout,
            )
            .await?;
        let hint = || {
            if hinted {
                format!("; run `{}`", self.login().display)
            } else {
                String::new()
            }
        };
        let status: Option<Status> = serde_json::from_slice(&out.stdout).ok();
        let Some(status) = status else {
            return Err(ClaudeError::new(
                ErrorKind::NotSignedIn,
                format!("the Claude CLI's login status could not be read{}", hint()),
            ));
        };
        if status.logged_in != Some(true) {
            return Err(ClaudeError::new(
                ErrorKind::NotSignedIn,
                format!("the Claude CLI is not signed in{}", hint()),
            ));
        }
        if status.auth_method.as_deref() != Some("claude.ai") {
            return Err(ClaudeError::new(
                ErrorKind::Plan,
                format!(
                    "the Claude CLI is not signed in with a Claude subscription (claude.ai); sign in again with `{}`",
                    self.login().display
                ),
            ));
        }
        let plan = status.subscription_type.unwrap_or_default().to_lowercase();
        if !(plan.contains("max") || plan.contains("ultra")) {
            return Err(ClaudeError::new(
                ErrorKind::Plan,
                "the Claude CLI's plan is not Max or Ultra, the only plans this experimental adapter accepts",
            ));
        }
        Ok(())
    }

    /// The checks every model-facing operation starts with.
    pub async fn preflight(&self) -> Result<(), ClaudeError> {
        self.check_environment()?;
        self.check_version().await?;
        self.check_login().await
    }

    /// Preflight, then an `initialize` control request and nothing else: no turn, no inference.
    pub async fn discover(&self) -> Result<Vec<ModelRow>, ClaudeError> {
        self.preflight().await?;
        let deadline = self.timeouts.discovery;
        match tokio::time::timeout(deadline, self.initialize()).await {
            Ok(result) => result,
            Err(_) => Err(ClaudeError::new(
                ErrorKind::SetupTimeout,
                "the Claude CLI did not list its models in time",
            )),
        }
    }

    async fn initialize(&self) -> Result<Vec<ModelRow>, ClaudeError> {
        let scratch = Scratch::new(self.warn.clone()).map_err(scratch_error)?;
        let files = ChildFiles::empty(&scratch).map_err(scratch_error)?;
        let args = fixed_args(&files, None, None, None);
        let mut child = self.launch(scratch, args, true)?;
        let request_id = initialize_id();
        child
            .write_line(&initialize_request(&request_id), self.timeouts.read_idle)
            .await?;
        child.close_stdin();
        let listing = child
            .list_models(&request_id, self.timeouts.read_idle, None)
            .await
            .map_err(|failure| {
                claude_error(&failure)
                    .cloned()
                    .expect("a listing without a reader fails only with Claude errors")
            })?;
        child.stop().await;
        Ok(listing.rows)
    }

    /// The environment a launch starts from, rebuilt: nothing is inherited that is not named
    /// here.
    fn environment(&self) -> Vec<(OsString, OsString)> {
        let mut vars: Vec<(OsString, OsString)> = Vec::new();
        for name in PASSTHROUGH_ENV {
            if let Some(value) = (self.env)(name) {
                vars.push((OsString::from(name), value));
            }
        }
        if let Some(dir) = &self.config_dir {
            vars.retain(|(name, _)| name != "CLAUDE_CONFIG_DIR");
            vars.push((
                OsString::from("CLAUDE_CONFIG_DIR"),
                dir.clone().into_os_string(),
            ));
        }
        for (name, value) in FIXED_ENV {
            vars.retain(|(existing, _)| existing != name);
            vars.push((OsString::from(name), OsString::from(value)));
        }
        // Git discovery from the working directory stops here, so an enclosing work tree (a
        // dotfiles repository over $HOME) never reaches the prompt.
        vars.retain(|(name, _)| name != "GIT_CEILING_DIRECTORIES");
        vars.push((
            OsString::from("GIT_CEILING_DIRECTORIES"),
            self.data_dir.clone().into_os_string(),
        ));
        vars
    }

    fn command(&self, args: Vec<OsString>, cwd: &Path) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .env_clear()
            .envs(self.environment())
            .current_dir(cwd)
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(0x0000_0200);
        command
    }

    fn start_error(&self, error: std::io::Error) -> ClaudeError {
        ClaudeError::new(
            ErrorKind::Cli,
            format!(
                "the Claude CLI `{}` could not be started; install it, or set `cli_path` in the account",
                self.program.display()
            ),
        )
        .with_source(error)
    }

    /// Starts the CLI in the stable working directory. `scratch` holds the per-launch files named
    /// in `args`, and the child guard owns it from here on.
    pub(crate) fn launch(
        &self,
        scratch: Scratch,
        args: Vec<OsString>,
        stdin: bool,
    ) -> Result<CliChild, ClaudeError> {
        self.refuse_instruction_files()?;
        let mut command = self.command(args, &self.workdir);
        command
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| self.start_error(e))?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let stdin = child.stdin.take();
        let tail = Arc::new(Mutex::new(Vec::new()));
        let drain = tokio::spawn(drain_stderr(stderr, tail.clone()));
        Ok(CliChild {
            child: Some(child),
            pid,
            stdin,
            stdout: LineReader::new(stdout),
            stderr: tail,
            drain: Some(drain),
            _scratch: scratch,
        })
    }

    /// Runs a short probe to completion and returns its output. Its output is kept off every
    /// log and error.
    async fn probe(
        &self,
        args: &[&str],
        deadline: Duration,
        timeout_kind: ErrorKind,
    ) -> Result<ProbeOutput, ClaudeError> {
        let scratch = Scratch::new(self.warn.clone()).map_err(scratch_error)?;
        let version = args.first() == Some(&"--version");
        let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut child = self.launch(scratch, argv, false)?;
        let run = async {
            let mut stdout = Vec::new();
            (&mut child.stdout.inner)
                .take(PROBE_OUTPUT_CAP)
                .read_to_end(&mut stdout)
                .await
                .ok();
            let status = child
                .child
                .as_mut()
                .expect("the child lives until stopped")
                .wait()
                .await;
            (stdout, status)
        };
        match tokio::time::timeout(deadline, run).await {
            Ok((stdout, status)) => {
                child.stop().await;
                Ok(ProbeOutput {
                    stdout,
                    success: status.is_ok_and(|s| s.success()),
                })
            }
            Err(_) => {
                child.stop().await;
                Err(ClaudeError::new(
                    timeout_kind,
                    format!(
                        "the Claude CLI's `{}` probe produced nothing in time",
                        if version { "--version" } else { "auth status" }
                    ),
                ))
            }
        }
    }
}

const PROBE_OUTPUT_CAP: u64 = 1024 * 1024;

struct ProbeOutput {
    stdout: Vec<u8>,
    success: bool,
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "loggedIn")]
    logged_in: Option<bool>,
    #[serde(rename = "authMethod")]
    auth_method: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

fn protocol(message: &str) -> ClaudeError {
    ClaudeError::new(ErrorKind::Protocol, message)
}

pub(crate) fn initialize_id() -> String {
    format!("fontaine-{}", uuid::Uuid::new_v4())
}

pub(crate) fn initialize_request(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "initialize" },
    })
}

pub(crate) fn scratch_error(error: std::io::Error) -> ClaudeError {
    ClaudeError::new(
        ErrorKind::Cli,
        "could not prepare a private working directory for the Claude CLI",
    )
    .with_source(error)
}

/// The first token of the CLI's version line, as three numbers.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let token = text.split_whitespace().next()?;
    let mut parts = token.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

/// What an `initialize` answer says about the account and the CLI. Missing or malformed fields
/// are `None`, never an error: they only select a cache layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliContext {
    pub email: Option<String>,
    pub version: Option<(u32, u32, u32)>,
}

/// The rows of an `initialize` answer with the context it carried.
#[derive(Debug)]
pub struct Listing {
    pub rows: Vec<ModelRow>,
    pub context: CliContext,
}

pub fn cli_context(response: &Value) -> CliContext {
    CliContext {
        email: response
            .get("account")
            .and_then(|account| account.get("email"))
            .and_then(Value::as_str)
            .filter(|email| !email.is_empty())
            .map(str::to_string),
        version: response
            .get("claude_code_version")
            .and_then(Value::as_str)
            .and_then(parse_version),
    }
}

/// The rows of an `initialize` answer. A credit marker that is anything but `false` counts as a
/// credit-billed model, so an unfamiliar marker fails closed.
pub fn model_rows(response: &Value) -> Result<Vec<ModelRow>, ClaudeError> {
    let models = response
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol("the Claude CLI's model listing has no models"))?;
    let mut rows = Vec::new();
    for model in models {
        let id = model
            .get("value")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| protocol("a model in the Claude CLI's listing has no id"))?;
        let canonical = ["resolvedModel", "resolved_model"]
            .iter()
            .find_map(|key| model.get(*key).and_then(Value::as_str))
            .filter(|c| !c.is_empty())
            .unwrap_or(id);
        let credits = [
            "usesUsageCredits",
            "usageCredits",
            "usage_credits",
            "billedToCredits",
        ]
        .iter()
        .filter_map(|key| model.get(*key))
        .any(|marker| !matches!(marker, Value::Null | Value::Bool(false)));
        rows.push(ModelRow {
            id: id.to_string(),
            canonical: canonical.to_string(),
            credits,
            capabilities: capabilities(model),
        });
    }
    Ok(rows)
}

/// The capability flags of one row; a field of the wrong type reads as absent, because the flags
/// are only an overlay on a listed sibling's facts.
fn capabilities(model: &Value) -> Capabilities {
    Capabilities {
        effort_levels: model
            .get("supportedEffortLevels")
            .and_then(Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| level.as_str().map(str::to_string))
                    .collect()
            }),
        adaptive_thinking: model
            .get("supportsAdaptiveThinking")
            .and_then(Value::as_bool),
        thinking_off: model.get("supportsThinkingOff").and_then(Value::as_bool),
    }
}

/// The model's settings, prompt and tool manifest, written inside the scratch directory.
pub(crate) struct ChildFiles {
    pub settings: PathBuf,
    pub mcp_config: PathBuf,
    pub system_prompt: PathBuf,
}

impl ChildFiles {
    /// The files of a process that only lists models: no tools, an empty prompt.
    fn empty(scratch: &Scratch) -> std::io::Result<Self> {
        Ok(ChildFiles {
            settings: scratch.write("settings.json", &serde_json::to_vec(&base_settings())?)?,
            mcp_config: scratch.write("mcp.json", br#"{"mcpServers":{}}"#)?,
            system_prompt: scratch.write("system.txt", b"")?,
        })
    }
}

/// Private settings that switch off the CLI's own memory, compaction, hooks and updates.
pub(crate) fn base_settings() -> Value {
    json!({
        "autoMemoryEnabled": false,
        "autoCompactEnabled": false,
        "disableAllHooks": true,
        "includeCoAuthoredBy": false,
        "autoUpdates": false,
        "env": {},
    })
}

/// The fixed invocation. `--bare` is absent on purpose: it disables subscription OAuth.
pub(crate) fn fixed_args(
    files: &ChildFiles,
    model: Option<&str>,
    session: Option<&str>,
    effort: Option<&str>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |value: &str| args.push(OsString::from(value));
    push("-p");
    if let Some(model) = model {
        push("--model");
        push(model);
    }
    for value in [
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--prompt-suggestions",
        "false",
        "--include-partial-messages",
        "--tools",
        "",
        "--system-prompt-file",
    ] {
        push(value);
    }
    args.push(files.system_prompt.clone().into_os_string());
    args.push(OsString::from("--settings"));
    args.push(files.settings.clone().into_os_string());
    for value in [
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--max-turns",
        "1",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--no-session-persistence",
    ] {
        args.push(OsString::from(value));
    }
    if let Some(session) = session {
        args.push(OsString::from("--session-id"));
        args.push(OsString::from(session));
    }
    if let Some(effort) = effort {
        args.push(OsString::from("--effort"));
        args.push(OsString::from(effort));
    }
    args.push(OsString::from("--mcp-config"));
    args.push(files.mcp_config.clone().into_os_string());
    args
}

/// An owner-only temporary directory, removed when dropped.
pub(crate) struct Scratch {
    pub path: PathBuf,
    warn: Warn,
}

impl Scratch {
    pub fn new(warn: Warn) -> std::io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("fontaine-claude-")
            .tempdir()?;
        Ok(Scratch {
            path: dir.keep(),
            warn,
        })
    }

    /// Writes `name` into the directory, readable only by the current user.
    pub fn write(&self, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        use std::io::Write;
        let path = self.path.join(name);
        owner_only::create_new(&path)?.write_all(bytes)?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = owner_only::remove_dir_all_retrying(&self.path) {
            self.warn.emit(&format!(
                "could not remove the Claude CLI's temporary directory {}: {}",
                self.path.display(),
                fontaine_tools::error_chain::chain(&e)
            ));
        }
    }
}

const STDERR_CAP: usize = 64 * 1024;

async fn drain_stderr(mut stderr: tokio::process::ChildStderr, tail: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                // The pipe is always drained so the child never blocks; only a prefix is kept.
                // Advisory text used only to classify a failure.
                let mut kept = lock_recovering(&tail);
                let room = STDERR_CAP.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
}

/// Why a read produced no line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadFail {
    Idle,
    TooLong,
    Io,
}

/// Splits a byte stream into lines, however it was chunked: `\n` or `\r\n` ends a line, and a
/// line longer than the cap is an error rather than unbounded memory.
#[derive(Debug)]
pub(crate) struct LineBuffer {
    pending: Vec<u8>,
    cap: usize,
}

impl LineBuffer {
    pub fn new(cap: usize) -> Self {
        LineBuffer {
            pending: Vec::new(),
            cap,
        }
    }

    /// Appends `chunk` and returns the lines it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, ReadFail> {
        let mut lines = Vec::new();
        for part in chunk.split_inclusive(|b| *b == b'\n') {
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                let mut line = std::mem::take(&mut self.pending);
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > self.cap {
                    return Err(ReadFail::TooLong);
                }
                if !line.is_empty() {
                    lines.push(line);
                }
            } else if self.pending.len() > self.cap {
                return Err(ReadFail::TooLong);
            }
        }
        Ok(lines)
    }

    /// The final line when the stream ended without a newline.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let mut line = std::mem::take(&mut self.pending);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        (!line.is_empty()).then_some(line)
    }
}

/// The longest stdout line accepted.
pub(crate) const LINE_CAP: usize = 16 * 1024 * 1024;

pub(crate) struct LineReader {
    pub(crate) inner: ChildStdout,
    buffer: LineBuffer,
    ready: std::collections::VecDeque<Vec<u8>>,
    ended: bool,
}

impl LineReader {
    fn new(stdout: ChildStdout) -> Self {
        LineReader {
            inner: stdout,
            buffer: LineBuffer::new(LINE_CAP),
            ready: Default::default(),
            ended: false,
        }
    }
}

/// A running CLI process, its pipes and its working directory. Dropping it, whether by
/// cancellation, a timeout, a failure or normal completion, kills the whole process tree and
/// removes the directory.
pub(crate) struct CliChild {
    child: Option<Child>,
    pid: Option<u32>,
    stdin: Option<ChildStdin>,
    stdout: LineReader,
    stderr: Arc<Mutex<Vec<u8>>>,
    drain: Option<tokio::task::JoinHandle<()>>,
    // Dropped after the process is signalled, so the directory outlives the CLI's use of it.
    _scratch: Scratch,
}

const STOP_GRACE: Duration = Duration::from_millis(500);

impl CliChild {
    /// Writes one JSON line to the CLI's stdin.
    pub async fn write_line(&mut self, value: &Value, idle: Duration) -> Result<(), ClaudeError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(protocol("the Claude CLI's input was already closed"));
        };
        let mut bytes = serde_json::to_vec(value).expect("a JSON value serializes");
        bytes.push(b'\n');
        let write = async {
            stdin.write_all(&bytes).await?;
            stdin.flush().await
        };
        match tokio::time::timeout(idle, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(protocol("the Claude CLI stopped reading its input")),
            Err(_) => Err(ClaudeError::new(
                ErrorKind::SetupTimeout,
                "the Claude CLI did not accept its input in time",
            )),
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Reads until the `initialize` answer to the already-written `request_id` and returns its
    /// model rows. Without a `reader` (a process that only lists) other lines are skipped; with
    /// one (a request's own process) every other line goes through the reader and any event
    /// but nothing is a protocol error, since no frame has asked for one yet.
    pub async fn list_models(
        &mut self,
        request_id: &str,
        idle: Duration,
        mut reader: Option<&mut Reader>,
    ) -> Result<Listing, Failure> {
        let fail = |message: &str| -> Failure { protocol(message).boxed() };
        loop {
            let line = match reader.as_deref() {
                Some(reader) => next(self, reader, idle).await?,
                None => match self.next_line(idle).await {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        return Err(fail("the Claude CLI ended before it listed its models"));
                    }
                    Err(ReadFail::Idle) => {
                        return Err(ClaudeError::new(
                            ErrorKind::SetupTimeout,
                            "the Claude CLI did not list its models in time",
                        )
                        .boxed());
                    }
                    Err(ReadFail::TooLong | ReadFail::Io) => {
                        return Err(fail("the Claude CLI's model listing was unreadable"));
                    }
                },
            };
            let value: Option<Value> = serde_json::from_slice(&line).ok();
            let answer = value.as_ref().and_then(|value| {
                let response = value.get("response")?;
                (value.get("type").and_then(Value::as_str) == Some("control_response")
                    && response.get("request_id").and_then(Value::as_str) == Some(request_id))
                .then_some(response)
            });
            let Some(response) = answer else {
                match reader.as_deref_mut() {
                    Some(reader) => {
                        let event = reader
                            .accept(&line)
                            .inspect_err(|e| rejected(&line, reader, e))?;
                        if !matches!(event, Event::Nothing) {
                            return Err(fail(
                                "the Claude CLI answered before it listed its models",
                            ));
                        }
                    }
                    None if value.is_none() => {
                        return Err(fail("the Claude CLI's model listing was not JSON"));
                    }
                    None => {}
                }
                continue;
            };
            if response.get("subtype").and_then(Value::as_str) != Some("success") {
                return Err(fail("the Claude CLI refused to list its models"));
            }
            let answer = response.get("response").unwrap_or(&Value::Null);
            return model_rows(answer)
                .map(|rows| Listing {
                    rows,
                    context: cli_context(answer),
                })
                .map_err(ClaudeError::boxed);
        }
    }

    /// The next stdout line; `None` at the end of the stream. `idle` bounds the wait.
    pub async fn next_line(&mut self, idle: Duration) -> Result<Option<Vec<u8>>, ReadFail> {
        let reader = &mut self.stdout;
        loop {
            if let Some(line) = reader.ready.pop_front() {
                return Ok(Some(line));
            }
            if reader.ended {
                return Ok(reader.buffer.finish());
            }
            let mut chunk = vec![0u8; 16 * 1024];
            match tokio::time::timeout(idle, reader.inner.read(&mut chunk)).await {
                Err(_) => return Err(ReadFail::Idle),
                Ok(Err(_)) => return Err(ReadFail::Io),
                Ok(Ok(0)) => reader.ended = true,
                Ok(Ok(n)) => reader.ready.extend(reader.buffer.push(&chunk[..n])?),
            }
        }
    }

    /// The exit status when the process has already ended.
    pub async fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        let child = self.child.as_mut()?;
        tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .ok()?
            .ok()
    }

    /// Waits briefly for the stderr drain to reach EOF so the tail is complete; a grandchild
    /// that holds the pipe open must not stall the failure.
    pub async fn settle_stderr(&mut self) {
        let Some(mut drain) = self.drain.take() else {
            return;
        };
        if tokio::time::timeout(Duration::from_millis(500), &mut drain)
            .await
            .is_err()
        {
            drain.abort();
        }
    }

    /// What the CLI said on stderr, for classifying a failure. Never shown or logged.
    pub fn stderr_text(&self) -> String {
        // Advisory text used only to classify a failure.
        let kept = lock_recovering(&self.stderr);
        String::from_utf8_lossy(&kept).into_owned()
    }

    /// Kills the process tree and waits for it.
    pub async fn stop(&mut self) {
        self.stdin = None;
        let Some(mut child) = self.child.take() else {
            return;
        };
        kill_tree(&mut child, self.pid, true).await;
        if let Some(drain) = self.drain.take() {
            drain.abort();
        }
    }
}

impl Drop for CliChild {
    fn drop(&mut self) {
        if let Some(drain) = self.drain.take() {
            drain.abort();
        }
        let Some(child) = self.child.take() else {
            return;
        };
        detached_kill(child, self.pid);
    }
}

#[cfg(unix)]
async fn kill_tree(child: &mut Child, pid: Option<u32>, wait: bool) {
    if wait {
        procgroup::stop(child, pid, STOP_GRACE).await;
    } else {
        procgroup::terminate(pid);
    }
}

#[cfg(unix)]
fn detached_kill(child: Child, pid: Option<u32>) {
    procgroup::terminate(pid);
    procgroup::detached_settle(child, pid, STOP_GRACE);
}

/// `taskkill /T /F` ends the whole tree, which a Windows process group alone cannot.
#[cfg(windows)]
async fn kill_tree(child: &mut Child, pid: Option<u32>, _wait: bool) {
    taskkill(pid);
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.start_kill();
    }
    let _ = child.wait().await;
}

#[cfg(windows)]
fn detached_kill(mut child: Child, pid: Option<u32>) {
    taskkill(pid);
    let _ = child.start_kill();
    procgroup::detached_settle(child, pid, STOP_GRACE);
}

#[cfg(windows)]
fn taskkill(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    // Best effort: the tree may already be gone.
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The usage-credit text shown whenever a plan limit or billing failure is reported.
pub(crate) fn limit_error() -> ClaudeError {
    ClaudeError::new(
        ErrorKind::Limit,
        format!(
            "the Claude CLI reported a plan limit, allowance or billing failure; wait for it to reset or check your account at {USAGE_SETTINGS_URL}"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_from_the_first_token() {
        assert_eq!(parse_version("2.1.318 (Claude Code)\n"), Some((2, 1, 318)));
        assert_eq!(parse_version("2.1"), None);
        assert_eq!(parse_version("2.1.3.4"), None);
        assert_eq!(parse_version("claude 2.1.318"), None);
        assert_eq!(parse_version(""), None);
        assert!((2, 1, 317) < MIN_VERSION && (2, 1, 318) >= MIN_VERSION);
    }

    #[test]
    fn lines_split_across_chunks_and_crlf() {
        let mut buffer = LineBuffer::new(64);
        assert!(buffer.push(b"{\"a\":").unwrap().is_empty());
        assert_eq!(
            buffer.push(b"1}\r\n{\"b\":2}\n\n{\"c\"").unwrap(),
            vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]
        );
        assert_eq!(buffer.finish(), Some(b"{\"c\"".to_vec()));
    }

    #[test]
    fn an_overlong_line_is_refused() {
        let mut buffer = LineBuffer::new(8);
        assert_eq!(buffer.push(b"0123456789"), Err(ReadFail::TooLong));
    }

    #[test]
    fn credit_markers_fail_closed() {
        let rows = model_rows(&json!({"models": [
            {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
            {"value": "opus", "resolvedModel": "claude-opus-5", "usesUsageCredits": false},
            {"value": "fast", "resolvedModel": "claude-opus-5-fast", "usesUsageCredits": true},
            {"value": "odd", "usageCredits": "yes"},
        ]}))
        .unwrap();
        let credits: Vec<bool> = rows.iter().map(|r| r.credits).collect();
        assert_eq!(credits, [false, false, true, true]);
        assert_eq!(rows[3].canonical, "odd");
    }

    #[test]
    fn capability_flags_are_read_and_wrong_types_read_as_absent() {
        let rows = model_rows(&json!({"models": [
            {"value": "a", "supportedEffortLevels": ["low", 3, "max"],
             "supportsAdaptiveThinking": true, "supportsThinkingOff": false},
            {"value": "b"},
            {"value": "c", "supportedEffortLevels": "high",
             "supportsAdaptiveThinking": "yes", "supportsThinkingOff": 1},
        ]}))
        .unwrap();
        assert_eq!(
            rows[0].capabilities,
            Capabilities {
                effort_levels: Some(vec!["low".into(), "max".into()]),
                adaptive_thinking: Some(true),
                thinking_off: Some(false),
            }
        );
        assert_eq!(rows[1].capabilities, Capabilities::default());
        assert_eq!(rows[2].capabilities, Capabilities::default());
    }

    #[test]
    fn a_listing_without_models_is_a_protocol_error() {
        assert!(model_rows(&json!({})).is_err());
        assert!(model_rows(&json!({"models": [{"nothing": 1}]})).is_err());
    }

    #[test]
    fn the_email_is_in_no_listing_error_text() {
        let email = "someone@private.example";
        for answer in [
            json!({"account": {"email": email}}),
            json!({"account": {"email": email}, "models": [{"nothing": 1}]}),
            json!({"account": {"email": email}, "models": "none"}),
        ] {
            let error = model_rows(&answer).unwrap_err();
            assert!(!error.to_string().contains(email), "{error}");
            assert!(!format!("{error:?}").contains(email), "{error:?}");
        }
    }

    #[cfg(unix)]
    async fn listing_with(script: &str) -> Result<Listing, ClaudeError> {
        let data = tempfile::tempdir().unwrap();
        let cli = Cli::new(
            Some(PathBuf::from("/bin/sh")),
            None,
            data.path(),
            Arc::new(|_| None),
            Timeouts::default(),
            Warn::process(),
        );
        std::fs::create_dir(cli.workdir()).unwrap();
        let scratch = Scratch::new(Warn::process()).unwrap();
        let mut child = cli
            .launch(scratch, vec!["-c".into(), script.into()], true)
            .unwrap();
        child
            .list_models("req", Duration::from_secs(5), None)
            .await
            .map_err(|failure| claude_error(&failure).cloned().unwrap())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_listing_is_a_protocol_error() {
        let error = listing_with(
            r#"echo '{"type":"control_response","response":{"request_id":"req","subtype":"error"}}'"#,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert_eq!(
            error.to_string(),
            "the Claude CLI refused to list its models"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_end_before_the_answer_is_a_protocol_error() {
        let error = listing_with(
            r#"echo '{"type":"system"}'; echo '{"type":"control_response","response":{"request_id":"other","subtype":"success"}}'"#,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert_eq!(
            error.to_string(),
            "the Claude CLI ended before it listed its models"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_answer_to_the_request_is_read_past_other_lines() {
        let listing = listing_with(
            r#"echo '{"type":"system"}'; echo '{"type":"control_response","response":{"request_id":"req","subtype":"success","response":{"models":[{"value":"sonnet","resolvedModel":"claude-sonnet-5-5"}],"account":{"email":"a@b.c"},"claude_code_version":"2.1.319"}}}'"#,
        )
        .await
        .unwrap();
        assert_eq!(listing.rows.len(), 1);
        assert_eq!(listing.rows[0].id, "sonnet");
        assert_eq!(listing.context.email.as_deref(), Some("a@b.c"));
        assert_eq!(listing.context.version, Some((2, 1, 319)));
    }

    #[test]
    fn the_context_is_read_and_missing_or_malformed_fields_give_none() {
        let context = cli_context(&json!({
            "account": {"email": "a@b.c"},
            "claude_code_version": "2.1.319",
        }));
        assert_eq!(context.email.as_deref(), Some("a@b.c"));
        assert_eq!(context.version, Some((2, 1, 319)));

        for account in [json!({}), json!({"email": ""}), json!({"email": 7})] {
            assert_eq!(cli_context(&json!({"account": account})).email, None);
        }
        assert_eq!(cli_context(&json!({})).email, None);
        for version in [json!(null), json!(7), json!("2.1"), json!("x.y.z")] {
            assert_eq!(
                cli_context(&json!({"claude_code_version": version})).version,
                None
            );
        }
        assert_eq!(cli_context(&json!({})).version, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_scratch_that_cannot_be_removed_warns_with_the_error_chain() {
        use std::os::unix::fs::PermissionsExt;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let scratch =
            Scratch::new(Warn::to(move |m| sink.lock().unwrap().push(m.to_owned()))).unwrap();
        let path = scratch.path.clone();
        std::fs::create_dir(path.join("inner")).unwrap();
        std::fs::write(path.join("inner").join("f"), b"x").unwrap();
        std::fs::set_permissions(path.join("inner"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        // Permission bits do not bind root, so the removal cannot fail there.
        let bound = std::fs::write(path.join("inner").join("probe"), b"x").is_err();
        drop(scratch);
        std::fs::set_permissions(path.join("inner"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let _ = std::fs::remove_dir_all(&path);
        if !bound {
            return;
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].contains("temporary directory"), "{seen:?}");
        assert!(
            seen[0].contains("denied") || seen[0].contains("ermission"),
            "{seen:?}"
        );
    }
}
