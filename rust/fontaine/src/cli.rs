use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fontaine_core::config::PermissionMode;
use fontaine_core::effort::Effort;

#[derive(Parser, Debug, Clone)]
#[command(name = "fontaine", version = crate::VERSION, about = "A terminal AI coding agent")]
pub struct Cli {
    /// Start in this directory instead of the launch directory: it becomes the project root, and
    /// relative `--settings`, `--add-dir` and `--debug-file` paths resolve against it
    #[arg(short = 'C', long = "cd", value_name = "DIR", global = true)]
    pub cd: Option<PathBuf>,

    /// TOML settings file layered over the project and user settings for this session
    #[arg(long, value_name = "FILE", global = true)]
    pub settings: Option<PathBuf>,

    /// Write debug-level diagnostics to this file (owner-only)
    #[arg(long, value_name = "PATH", global = true)]
    pub debug_file: Option<PathBuf>,

    /// Run non-interactively: the prompt is the positional argument (stdin when absent) and
    /// the result goes to stdout
    #[arg(short = 'p', long = "print")]
    pub print: bool,

    /// Session permission mode, overriding `[permissions] default_mode`
    #[arg(long, value_enum, value_name = "MODE")]
    pub permission_mode: Option<ModeArg>,

    /// State that this process runs inside a container. With `[guardian] profile = "minimal"`
    /// it lets the minimal guardian load, after a runtime probe also finds a container; the
    /// flag alone is never enough
    #[arg(long)]
    pub in_container: bool,

    /// Allow rule in `Tool(specifier)` syntax for this session, such as `shell(npm test)`.
    /// Repeatable; it loads as the CLI scope's `[permissions] allow`
    #[arg(long = "allowed-tools", value_name = "RULE")]
    pub allowed_tools: Vec<String>,

    /// Deny rule in `Tool(specifier)` syntax for this session, such as `read(.env)`.
    /// Repeatable; it loads as the CLI scope's `[permissions] deny`
    #[arg(long = "disallowed-tools", value_name = "RULE")]
    pub disallowed_tools: Vec<String>,

    /// Shorthand for `--permission-mode afk`: AFK mode decides as `auto` does, but nothing that
    /// would ask the user is asked; each such call is denied with guidance to finish the allowed
    /// work or stop and report. A temperature ceiling stops a session that keeps hitting denials
    #[arg(long)]
    pub afk: bool,

    /// The AFK session temperature at which the session is stopped, overriding
    /// `[afk] temperature_ceiling`; a finite number above zero
    #[arg(long, value_name = "TEMPERATURE")]
    pub afk_temperature_ceiling: Option<f64>,

    /// The main model as `<account>/<model>` or a role name, overriding `[model] model`
    #[arg(long, value_name = "ACCOUNT/MODEL")]
    pub model: Option<String>,

    /// The reasoning effort of the main agent: off, low, medium, high or max. Overrides
    /// `[model] effort`
    #[arg(long, value_name = "LEVEL")]
    pub effort: Option<Effort>,

    /// How the headless run reports its result
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, value_name = "FORMAT")]
    pub output_format: OutputFormat,

    /// How the headless run reads its prompts; `stream-json` takes one user message per line
    #[arg(long, value_enum, default_value_t = InputFormat::Text, value_name = "FORMAT")]
    pub input_format: InputFormat,

    /// With `--output-format stream-json`, also emit `assistant_delta` events
    #[arg(long)]
    pub include_partial_messages: bool,

    /// Stop each turn after this many model calls
    #[arg(long, value_name = "N")]
    pub max_turns: Option<usize>,

    /// Stop the run once the main model's spend exceeds this many US dollars
    #[arg(long, value_name = "USD")]
    pub max_budget_usd: Option<f64>,

    /// What a headless run does when an account's usage window is spent: `wait` holds the whole
    /// run until the window resets, up to --max-usage-wait; `exit` ends it at once with exit
    /// code 3. Overrides `[headless] on_usage_limit`
    #[arg(long, value_enum, value_name = "ACTION")]
    pub on_usage_limit: Option<UsageLimitAction>,

    /// The longest a headless run waits for a spent usage window, such as `90m` or `6h`; a bare
    /// number is seconds. Overrides `[headless] max_usage_wait_secs`
    #[arg(long, value_name = "DURATION")]
    pub max_usage_wait: Option<String>,

    /// JSON Schema (inline) the final answer must follow; the answer lands in
    /// `result.structured_output`
    #[arg(long, value_name = "SCHEMA")]
    pub json_schema: Option<String>,

    /// Add a directory to the workspace roots; file tools may reach it. Repeatable
    #[arg(long = "add-dir", value_name = "PATH")]
    pub add_dir: Vec<PathBuf>,

    /// Use this UUID as the session id instead of a fresh one; it must not name an existing
    /// session. With --continue or --resume it is accepted only together with --fork-session,
    /// where it names the copy
    #[arg(long, value_name = "UUID")]
    pub session_id: Option<String>,

    /// Continue the most recently updated session that was started in this directory; a fresh
    /// session starts when there is none
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    pub continue_session: bool,

    /// Resume the session with this ID or name (a name is looked up among the sessions started
    /// in this directory): its conversation and ledger come back, in the default permission mode
    /// and without any grants
    #[arg(short = 'r', long, value_name = "ID|NAME")]
    pub resume: Option<String>,

    /// With --continue or --resume, start a new session that is a copy of the one it would
    /// have continued
    #[arg(long)]
    pub fork_session: bool,

    /// Name the session, so --resume can find it by name. With --fork-session it names the
    /// copy; with --continue or --resume alone it renames the session reopened
    #[arg(short = 'n', long, value_name = "NAME")]
    pub name: Option<String>,

    /// Replace the base system prompt
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Append to the system prompt after all other layers
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Option<String>,

    /// Describe the interface as plain labelled lines for a screen reader, instead of drawing
    /// it. Ignored by a headless run
    #[arg(long)]
    pub ax_screen_reader: bool,

    /// The task for a headless run
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Inspect the rules the guardian judges against
    #[command(name = "auto-mode")]
    AutoMode(AutoModeArgs),
    /// Install and manage plugins and marketplaces
    Plugin(PluginArgs),
    /// List the models each configured account offers
    Models(ModelsArgs),
    /// Check the setup: config layers, trust, models, MCP servers and memory
    Doctor(DoctorArgs),
    /// Add, list, show and remove MCP servers
    Mcp(McpArgs),
    /// Update this installation to the newest build on the channel it was installed from
    Update,
}

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Add an MCP server. A stdio server's own flags go after `--`:
    /// `fontaine mcp add foo -- npx -y some-server --flag`
    Add(McpAddArgs),
    /// List the configured servers and whether each connects
    List(McpListArgs),
    /// Show one server's definition, where it is declared and whether it connects
    Get(McpGetArgs),
    /// Remove a server from the scope that declares it
    Remove(McpRemoveArgs),
    /// Sign in to a streamable HTTP server with OAuth and store the token
    Login(McpLoginArgs),
    /// Delete the stored OAuth credentials of a streamable HTTP server
    Logout(McpLogoutArgs),
}

/// What `--on-usage-limit` accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum UsageLimitAction {
    Wait,
    Exit,
}

impl From<UsageLimitAction> for fontaine_core::config::OnUsageLimit {
    fn from(action: UsageLimitAction) -> Self {
        match action {
            UsageLimitAction::Wait => Self::Wait,
            UsageLimitAction::Exit => Self::Exit,
        }
    }
}

/// Where a server is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum McpScopeArg {
    /// Private to you, in this workspace (`mcp-local.toml` in the config directory)
    Local,
    /// Shared with the repository (`.mcp.json`)
    Project,
    /// Every workspace (`[mcp.servers]` in `config.toml`)
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTransportArg {
    Stdio,
    Http,
}

fn parse_transport(text: &str) -> Result<McpTransportArg, String> {
    match text {
        "stdio" => Ok(McpTransportArg::Stdio),
        "http" => Ok(McpTransportArg::Http),
        "sse" | "ws" => Err(format!(
            "the `{text}` transport is not supported yet (see issue #36); use stdio or http"
        )),
        other => Err(format!("`{other}` is not a transport; use stdio or http")),
    }
}

fn parse_env_pair(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err("expected KEY=value".to_string()),
    }
}

fn parse_env_name(text: &str) -> Result<String, String> {
    if text.is_empty() || text.contains('=') || text.contains('\0') {
        Err("expected a variable name".to_string())
    } else {
        Ok(text.to_string())
    }
}

#[derive(Args, Debug, Clone)]
pub struct McpAddArgs {
    /// Where to declare the server
    #[arg(short = 's', long, value_enum, default_value_t = McpScopeArg::Local, value_name = "SCOPE")]
    pub scope: McpScopeArg,
    /// The transport: stdio (a command) or http (a URL)
    #[arg(short = 't', long, value_parser = parse_transport, default_value = "stdio", value_name = "TRANSPORT")]
    pub transport: McpTransportArg,
    /// A variable set on the server process, as KEY=value. Repeatable; stdio only
    #[arg(short = 'e', long = "env", value_parser = parse_env_pair, value_name = "KEY=value")]
    pub env: Vec<(String, String)>,
    /// A variable passed through from Fontaine's environment by name, which Fontaine otherwise
    /// scrubs. Repeatable; stdio only
    #[arg(long = "env-var", value_parser = parse_env_name, value_name = "NAME")]
    pub env_var: Vec<String>,
    /// A header sent on the server's protocol requests, as KEY=VALUE. Repeatable; http only
    #[arg(short = 'H', long = "header", value_parser = parse_env_pair, value_name = "KEY=value")]
    pub header: Vec<(String, String)>,
    /// The server's name, as it appears in tool names
    #[arg(value_name = "NAME")]
    pub name: String,
    /// The command to run, or the URL with `-t http`
    #[arg(value_name = "COMMAND_OR_URL")]
    pub command_or_url: String,
    /// The command's arguments
    #[arg(value_name = "ARGS", trailing_var_arg = true)]
    pub args: Vec<String>,
}

#[derive(Args, Debug, Clone)]
pub struct McpListArgs {
    /// Start each server the session would start and count its tools; `--connect=no` skips
    /// the probe
    #[arg(long, value_enum, default_value_t = Toggle::Yes, value_name = "yes|no")]
    pub connect: Toggle,
}

#[derive(Args, Debug, Clone)]
pub struct McpGetArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Start the server and count its tools; `--connect=no` skips the probe
    #[arg(long, value_enum, default_value_t = Toggle::Yes, value_name = "yes|no")]
    pub connect: Toggle,
}

#[derive(Args, Debug, Clone)]
pub struct McpRemoveArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
    /// The scope to remove it from; needed only when several declare the name
    #[arg(short = 's', long, value_enum, value_name = "SCOPE")]
    pub scope: Option<McpScopeArg>,
}

#[derive(Args, Debug, Clone)]
pub struct McpLoginArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Args, Debug, Clone)]
pub struct McpLogoutArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    #[command(flatten)]
    pub format: FormatArgs,
    /// Start each MCP server the session would start and count its tools; `--connect=no`
    /// skips the probe
    #[arg(long, value_enum, default_value_t = Toggle::Yes, value_name = "yes|no")]
    pub connect: Toggle,
    /// Also list the settings no layer set
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ModelsArgs {
    /// List only this account
    #[arg(value_name = "ACCOUNT")]
    pub account: Option<String>,
    #[command(flatten)]
    pub format: FormatArgs,
}

#[derive(Args, Debug, Clone)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub action: PluginAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PluginAction {
    /// Add, list, update or remove a marketplace
    Marketplace(MarketplaceArgs),
    /// Install a plugin: `fontaine plugin install <name>@<marketplace>`
    Install(NameArgs),
    /// Remove an installed plugin
    Uninstall(NameArgs),
    /// Update an installed plugin to its source's new revision
    Update(NameArgs),
    /// Enable an installed plugin
    Enable(NameArgs),
    /// Disable an installed plugin (it stays on disk and contributes nothing)
    Disable(NameArgs),
    /// Turn automatic updates of one installed plugin on or off
    AutoUpdate(AutoUpdateArgs),
    /// List installed plugins
    List(FormatArgs),
    /// Honour a skill's allowed-tools: `<plugin>:<skill>`, a skill in your skills directory, or a plugin name for every skill it has now
    Trust(TrustArgs),
    /// Stop honouring a trusted skill's allowed-tools (or every trusted skill of a plugin)
    Untrust(NameArgs),
}

#[derive(Args, Debug, Clone)]
pub struct TrustArgs {
    /// A skill name as `/skills` shows it, or an installed plugin's name
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Record without asking; the details are still printed
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug, Clone)]
pub struct MarketplaceArgs {
    #[command(subcommand)]
    pub action: MarketplaceAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum MarketplaceAction {
    /// Register a marketplace from a git URL or local directory
    Add(MarketplaceAddArgs),
    /// List registered marketplaces
    List,
    /// Move a marketplace's pin to the remote default-branch HEAD
    Update { name: Option<String> },
    /// Remove a marketplace (refused while a plugin names it)
    Remove { name: String },
    /// Turn automatic updates of one marketplace on or off
    AutoUpdate {
        name: String,
        #[arg(value_enum)]
        setting: Toggle,
    },
}

#[derive(Args, Debug, Clone)]
pub struct AutoUpdateArgs {
    /// An installed plugin's name
    #[arg(value_name = "NAME")]
    pub name: String,
    #[arg(value_enum)]
    pub setting: Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    Yes,
    No,
}

#[derive(Args, Debug, Clone)]
pub struct MarketplaceAddArgs {
    /// A git URL or a local directory
    #[arg(value_name = "URL|PATH")]
    pub source: String,

    /// Override the registry key; the catalog keeps its own name
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct NameArgs {
    /// A plugin name, `name@marketplace` for install, or a skill name for untrust
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Args, Debug, Clone)]
pub struct AutoModeArgs {
    #[command(subcommand)]
    pub action: AutoModeAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AutoModeAction {
    /// Print the shipped environment and rule lists
    Defaults(FormatArgs),
    /// Print the lists a session would hand the guardian, and where each came from
    Config(FormatArgs),
    /// Ask the guardian's model to review your custom entries
    Critique(FormatArgs),
    /// Collect this project's environment facts once, for auto mode's guardian
    Setup(SetupArgs),
}

#[derive(Args, Debug, Clone)]
pub struct SetupArgs {
    /// Skip the model scan of the repository
    #[arg(long)]
    pub no_model: bool,
    /// Skip the offer to set up the guardian composition's allow screen
    #[arg(long)]
    pub no_composition: bool,
    #[arg(long, value_enum, default_value_t = ReportFormat::Text, value_name = "FORMAT")]
    pub format: ReportFormat,
}

#[derive(Args, Debug, Clone)]
pub struct FormatArgs {
    #[arg(long, value_enum, default_value_t = ReportFormat::Text, value_name = "FORMAT")]
    pub format: ReportFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    #[value(name = "manual")]
    Manual,
    #[value(name = "acceptEdits")]
    AcceptEdits,
    #[value(name = "plan")]
    Plan,
    #[value(name = "auto")]
    Auto,
    #[value(name = "dontAsk")]
    DontAsk,
    #[value(name = "afk")]
    Afk,
}

impl From<ModeArg> for PermissionMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Manual => PermissionMode::Manual,
            ModeArg::AcceptEdits => PermissionMode::AcceptEdits,
            ModeArg::Plan => PermissionMode::Plan,
            ModeArg::Auto => PermissionMode::Auto,
            ModeArg::DontAsk => PermissionMode::DontAsk,
            ModeArg::Afk => PermissionMode::Afk,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    Text,
    StreamJson,
}
