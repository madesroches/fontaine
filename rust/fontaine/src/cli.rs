use std::path::PathBuf;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use fontaine_core::config::PermissionMode;

#[derive(Parser, Debug)]
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

    /// Replace the base system prompt
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Append to the system prompt after all other layers
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Option<String>,

    /// The task for a headless run
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage stored credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Store an account's API key, or the telemetry ingestion key, in the credentials file
    #[command(group(ArgGroup::new("target").required(true).args(["account", "telemetry"])))]
    Login {
        /// The account, which must exist in the loaded configuration
        account: Option<String>,
        /// Store the telemetry ingestion key instead
        #[arg(long)]
        telemetry: bool,
    },
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
