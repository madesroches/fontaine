pub mod telemetry;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fontaine_core::config::{self, ConfigError, LoadParams};
use fontaine_core::dirs;

use telemetry::DebugSink;

#[derive(Parser, Debug)]
#[command(name = "fontaine", version, about = "A terminal AI coding agent")]
struct Cli {
    /// TOML settings file layered over the project and user settings for this session
    #[arg(long, value_name = "FILE")]
    settings: Option<PathBuf>,

    /// Write debug-level diagnostics to this file (owner-only)
    #[arg(long, value_name = "PATH")]
    debug_file: Option<PathBuf>,
}

pub type EnvFn = Box<dyn Fn(&str) -> Option<OsString>>;

/// The process environment, injected so only `main` touches the real one.
pub struct Host {
    pub cwd: PathBuf,
    pub env: EnvFn,
    pub default_config_dir: Option<PathBuf>,
    pub default_data_dir: Option<PathBuf>,
}

impl Host {
    pub fn from_process() -> std::io::Result<Host> {
        Ok(Host {
            cwd: std::env::current_dir()?,
            env: Box::new(|name| std::env::var_os(name)),
            default_config_dir: ::dirs::config_dir(),
            default_data_dir: ::dirs::data_dir(),
        })
    }
}

pub const EXIT_CONFIG: u8 = fontaine_core::config::CONFIG_EXIT_CODE;

pub fn run(args: impl IntoIterator<Item = OsString>, host: Host) -> ExitCode {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // clap exits 0 for --help/--version and 2 for usage errors.
            let code = if e.use_stderr() { EXIT_CONFIG } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };

    let sink = match DebugSink::open(cli.debug_file.as_deref(), (host.env)(telemetry::LOG_VAR)) {
        Ok(sink) => sink,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let loaded = telemetry::with_debug_only(sink.as_ref(), || {
        let dirs = dirs::resolve(
            &*host.env,
            host.default_config_dir.clone(),
            host.default_data_dir.clone(),
        )?;
        let config = config::load(&LoadParams {
            launch_dir: &host.cwd,
            config_dir: &dirs.config,
            data_dir: &dirs.data,
            settings_file: cli.settings.as_deref(),
            env: &*host.env,
        })?;
        let stack: Vec<String> = config.scopes.iter().map(ToString::to_string).collect();
        tracing::debug!(scope_stack = ?stack, workspace = %config.workspace.display(), "config loaded");
        Ok::<_, ConfigError>(config)
    });
    let config = match loaded {
        Ok(config) => config,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(e.exit_code());
        }
    };

    telemetry::install(sink.as_ref(), &config);
    let session = tracing::info_span!("fontaine.session");
    let _entered = session.enter();
    ExitCode::SUCCESS
}
