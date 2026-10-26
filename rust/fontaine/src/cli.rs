use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use fontaine_core::config::PermissionMode;

#[derive(Parser, Debug, Clone)]
#[command(name = "fontaine", version, about = "A terminal AI coding agent")]
pub struct Cli {
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

    /// The main model as `<account>/<model>`, overriding `[model] model`
    #[arg(long, value_name = "ACCOUNT/MODEL")]
    pub model: Option<String>,

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
}

impl From<ModeArg> for PermissionMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Manual => PermissionMode::Manual,
            ModeArg::AcceptEdits => PermissionMode::AcceptEdits,
            ModeArg::Plan => PermissionMode::Plan,
            ModeArg::Auto => PermissionMode::Auto,
            ModeArg::DontAsk => PermissionMode::DontAsk,
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
