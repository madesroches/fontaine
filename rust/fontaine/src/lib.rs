mod auth;
pub mod cli;
mod echo;
pub mod headless;
pub mod telemetry;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fontaine_core::config::{self, Config, ConfigError, LoadParams, PermissionMode};
use fontaine_core::dirs::{self, Dirs};
use fontaine_core::models::{self, ResolveParams};
use rig_core::schemars::Schema;

use cli::{AuthCommand, Cli, Command, InputFormat, OutputFormat};
use telemetry::{DebugSink, InstallParams};

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

fn usage(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("fontaine: {message}");
    ExitCode::from(EXIT_CONFIG)
}

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
        let params = LoadParams {
            launch_dir: &host.cwd,
            config_dir: &dirs.config,
            data_dir: &dirs.data,
            settings_file: cli.settings.as_deref(),
            env: &*host.env,
        };
        let config = if matches!(cli.command, Some(Command::Auth { .. })) {
            config::load_unresolved(&params)?
        } else {
            config::load(&params)?
        };
        let stack: Vec<String> = config.scopes.iter().map(ToString::to_string).collect();
        tracing::debug!(scope_stack = ?stack, workspace = %config.workspace.display(), "config loaded");
        Ok::<_, ConfigError>((config, dirs))
    });
    let (config, dirs) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(e.exit_code());
        }
    };

    if let Some(Command::Auth {
        command: AuthCommand::Login { account, telemetry },
    }) = &cli.command
    {
        return match auth::login(account.as_deref(), *telemetry, &config, &dirs.data) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => usage(e),
        };
    }

    let headless = match prepare_headless(&cli, &config, &host) {
        Ok(options) => options,
        Err(code) => return code,
    };

    let export = telemetry::install(
        sink.as_ref(),
        &config,
        &InstallParams {
            env: &*host.env,
            data_dir: &dirs.data,
        },
    );
    let session = tracing::info_span!(
        target: telemetry::span_bridge::TARGET,
        "fontaine.session",
        start_type = if cli.print { "headless" } else { "interactive" },
    );
    let entered = session.enter();

    let Some(options) = headless else {
        drop(entered);
        drop(session);
        export.finish(0);
        return ExitCode::SUCCESS;
    };

    let code = match export.runtime() {
        Ok(runtime) => runtime.block_on(run_headless(&cli, options, &config, &dirs, &host)),
        Err(e) => {
            eprintln!("fontaine: cannot start the async runtime: {e}");
            1
        }
    };
    drop(entered);
    drop(session);
    export.finish(code);
    ExitCode::from(code)
}

/// Validates everything about a headless invocation that needs no network, so a usage error
/// never costs a provider call.
fn prepare_headless(
    cli: &Cli,
    config: &Config,
    host: &Host,
) -> Result<Option<headless::Options>, ExitCode> {
    if !cli.print {
        return Ok(None);
    }
    let mode = cli
        .permission_mode
        .map_or(config.permissions.default_mode, PermissionMode::from);
    if mode == PermissionMode::Auto {
        return Err(usage(
            "`--permission-mode auto` is not available until the guardian ships; \
             use manual, acceptEdits, plan or dontAsk",
        ));
    }
    if cli.prompt.is_some() && cli.input_format == InputFormat::StreamJson {
        return Err(usage(
            "a prompt argument cannot be combined with --input-format stream-json",
        ));
    }
    if cli.include_partial_messages && cli.output_format != OutputFormat::StreamJson {
        return Err(usage(
            "--include-partial-messages requires --output-format stream-json",
        ));
    }
    if cli.max_turns == Some(0) {
        return Err(usage("--max-turns must be at least 1"));
    }
    if let Some(budget) = cli.max_budget_usd
        && !(budget.is_finite() && budget >= 0.0)
    {
        return Err(usage(
            "--max-budget-usd must be a finite number, zero or more",
        ));
    }
    let json_schema = match &cli.json_schema {
        None => None,
        Some(text) => {
            let value: serde_json::Value = serde_json::from_str(text)
                .map_err(|e| usage(format!("--json-schema is not valid JSON: {e}")))?;
            let schema = Schema::try_from(value)
                .map_err(|_| usage("--json-schema must be a JSON Schema object"))?;
            Some(schema)
        }
    };
    let echo_tool = (host.env)(echo::ENABLE_VAR).is_some_and(|v| v == "1");
    Ok(Some(headless::Options {
        prompt: cli.prompt.clone(),
        input_format: cli.input_format,
        output_format: cli.output_format,
        include_partial_messages: cli.include_partial_messages,
        max_turns: cli.max_turns,
        max_budget_usd: cli.max_budget_usd,
        json_schema,
        system_prompt: cli.system_prompt.clone(),
        append_system_prompt: cli.append_system_prompt.clone(),
        mode,
        echo_tool,
    }))
}

async fn run_headless(
    cli: &Cli,
    options: headless::Options,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
) -> u8 {
    let resolved = models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: &*host.env,
        model_override: cli.model.as_deref(),
    })
    .await;
    let models = match resolved {
        Ok(models) => models,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return e.exit_code();
        }
    };
    if options.max_budget_usd.is_some() && !models.main.priced() {
        eprintln!(
            "fontaine: --max-budget-usd needs a known price, and rig's catalog has none for `{}`",
            models.main.reference
        );
        return EXIT_CONFIG;
    }
    headless::run(
        options,
        headless::Context {
            config,
            models,
            data_dir: &dirs.data,
        },
    )
    .await
}
