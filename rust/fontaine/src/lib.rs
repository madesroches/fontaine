mod auth;
pub mod cli;
pub mod headless;
pub mod telemetry;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fontaine_core::agent::{AgentRuntime, AgentSetup};
use fontaine_core::config::{self, CliRules, Config, ConfigError, LoadParams, PermissionMode};
use fontaine_core::containment;
use fontaine_core::dirs::{self, Dirs};
use fontaine_core::models::{self, Models, ResolveParams, Switcher};
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::Session;
use fontaine_core::tools;
use fontaine_core::workspace::{self, RootSource};
use fontaine_tools::git_bash::GitBash;
use rig_core::schemars::Schema;
use std::io::IsTerminal;
use std::sync::Arc;
use uuid::Uuid;

use cli::{AuthCommand, Cli, Command, InputFormat, OutputFormat};
use telemetry::{DebugSink, InstallParams};

pub type EnvFn = Box<dyn Fn(&str) -> Option<OsString>>;
pub type EnvNamesFn = Box<dyn Fn() -> Vec<OsString>>;

/// The process environment, injected so only `main` touches the real one.
pub struct Host {
    pub cwd: PathBuf,
    pub env: EnvFn,
    pub default_config_dir: Option<PathBuf>,
    pub default_data_dir: Option<PathBuf>,
    /// The names present in the environment, for the variables found by prefix rather than by
    /// name.
    pub env_names: EnvNamesFn,
}

impl Host {
    pub fn from_process() -> std::io::Result<Host> {
        Ok(Host {
            cwd: std::env::current_dir()?,
            env: Box::new(|name| std::env::var_os(name)),
            default_config_dir: ::dirs::config_dir(),
            default_data_dir: ::dirs::data_dir(),
            env_names: Box::new(|| std::env::vars_os().map(|(name, _)| name).collect()),
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
            cli_rules: CliRules {
                allow: &cli.allowed_tools,
                deny: &cli.disallowed_tools,
            },
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

    let mode = session_mode(&cli, &config);
    if !cli.print && !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return usage("the interactive UI needs a terminal; use `fontaine -p` for scripted runs");
    }
    let headless = if cli.print {
        match prepare_headless(&cli) {
            Ok(options) => Some(options),
            Err(code) => return code,
        }
    } else {
        None
    };
    let prep = match prepare_session(&cli, &config, &host, &dirs, mode) {
        Ok(prep) => prep,
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

    let code = match export.runtime() {
        Ok(runtime) => runtime.block_on(async {
            match headless {
                Some(options) => run_headless(&cli, options, prep, &config, &dirs, &host).await,
                None => run_interactive(&cli, prep, &config, &dirs, &host).await,
            }
        }),
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

/// The session's permission mode: `--permission-mode`, then `default_mode`, then what the
/// surface starts in. The interactive UI starts in `auto`; a headless run starts in `manual`,
/// as Claude Code's `-p` starts in `default`.
fn session_mode(cli: &Cli, config: &Config) -> PermissionMode {
    cli.permission_mode
        .map(PermissionMode::from)
        .or(config.permissions.default_mode)
        .unwrap_or(if cli.print {
            PermissionMode::Manual
        } else {
            PermissionMode::Auto
        })
}

/// Validates everything about a headless invocation that needs no network, so a usage error
/// never costs a provider call.
fn prepare_headless(cli: &Cli) -> Result<headless::Options, ExitCode> {
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
            if !value.is_object() {
                return Err(usage("--json-schema must be a JSON Schema object"));
            }
            let schema = Schema::try_from(value)
                .map_err(|_| usage("--json-schema must be a JSON Schema object"))?;
            Some(schema)
        }
    };
    Ok(headless::Options {
        prompt: cli.prompt.clone(),
        input_format: cli.input_format,
        output_format: cli.output_format,
        include_partial_messages: cli.include_partial_messages,
        max_turns: cli.max_turns,
        max_budget_usd: cli.max_budget_usd,
        json_schema,
    })
}

/// What both surfaces need before the first model call: the session, its roots and the
/// environment the shell tool runs in.
pub struct SessionPrep {
    pub session: Session,
    /// The workspace roots, primary first, canonical.
    pub roots: Vec<PathBuf>,
    pub mode: PermissionMode,
    pub scrub_env: Vec<OsString>,
    pub git_bash: Option<GitBash>,
    pub home: Option<PathBuf>,
}

fn prepare_session(
    cli: &Cli,
    config: &Config,
    host: &Host,
    dirs: &Dirs,
    mode: PermissionMode,
) -> Result<SessionPrep, ExitCode> {
    let session_id = match cli.session_id.as_deref() {
        None => Uuid::new_v4(),
        Some(text) => {
            Uuid::parse_str(text).map_err(|e| usage(format!("--session-id must be a UUID: {e}")))?
        }
    }
    .hyphenated()
    .to_string();
    let git_bash = git_bash(host)?;

    let added = cli
        .add_dir
        .iter()
        .map(|dir| (RootSource::Flag, dir.clone()))
        .chain(
            config
                .permissions
                .additional_directories
                .iter()
                .map(|dir| (RootSource::Settings, PathBuf::from(dir))),
        );
    let roots = workspace::resolve(&config.workspace, &host.cwd, added);
    for warning in &roots.warnings {
        eprintln!("fontaine: warning: {warning}");
    }

    let session = Session::create(&dirs.data, &session_id).map_err(|e| {
        eprintln!("fontaine: {e}");
        ExitCode::from(e.exit_code())
    })?;
    let home = (host.env)(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(::dirs::home_dir)
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    Ok(SessionPrep {
        session,
        roots: roots.roots,
        mode,
        scrub_env: tools::scrubbed_env(config, (host.env_names)()),
        git_bash,
        home,
    })
}

/// The shell tool runs through Git Bash on Windows; elsewhere it needs none.
#[cfg(windows)]
fn git_bash(host: &Host) -> Result<Option<GitBash>, ExitCode> {
    let probe = fontaine_tools::git_bash::Probe {
        env: &*host.env,
        is_file: &|path| path.is_file(),
    };
    fontaine_tools::git_bash::discover(&probe)
        .map(Some)
        .map_err(usage)
}

#[cfg(not(windows))]
fn git_bash(_host: &Host) -> Result<Option<GitBash>, ExitCode> {
    Ok(None)
}

async fn resolve_models(
    cli: &Cli,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
) -> Result<Models, u8> {
    models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: &*host.env,
        model_override: cli.model.as_deref(),
    })
    .await
    .map_err(|e| {
        eprintln!("fontaine: {e}");
        e.exit_code()
    })
}

/// What the session was started from.
struct Launch<'a> {
    cli: &'a Cli,
    config: &'a Config,
    dirs: &'a Dirs,
    host: &'a Host,
}

fn agent_setup(
    launch: &Launch<'_>,
    prep: SessionPrep,
    models: Models,
    headless: bool,
    json_schema: Option<Schema>,
) -> AgentSetup {
    let Launch {
        cli,
        config,
        dirs,
        host,
    } = *launch;
    let resolved = containment::resolve(
        config.guardian.profile,
        cli.in_container,
        containment::detect(),
    );
    let mut notices = models.notices.clone();
    notices.extend(resolved.notice);
    // The environment as the session started, for resolving models again later.
    let env: std::collections::HashMap<String, OsString> = (host.env_names)()
        .into_iter()
        .filter_map(|name| {
            let name = name.into_string().ok()?;
            let value = (host.env)(&name)?;
            Some((name, value))
        })
        .collect();
    let switcher = Switcher::new(
        config.clone(),
        dirs.data.clone(),
        Arc::new(move |name: &str| env.get(name).cloned()),
    );
    AgentSetup {
        config: config.clone(),
        config_dir: dirs.config.clone(),
        data_dir: dirs.data.clone(),
        models,
        session: prep.session,
        roots: prep.roots,
        mode: prep.mode,
        scrub_env: prep.scrub_env,
        git_bash: prep.git_bash,
        system_prompt: cli.system_prompt.clone(),
        append_system_prompt: cli.append_system_prompt.clone(),
        json_schema,
        headless,
        approver: Arc::new(DenyAll),
        home: prep.home,
        user_config: Some(dirs.config.join(config::USER_FILE)),
        guardian_profile: resolved.profile,
        notices,
        switcher: Some(switcher),
    }
}

async fn run_headless(
    cli: &Cli,
    options: headless::Options,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
) -> u8 {
    let models = match resolve_models(cli, config, dirs, host).await {
        Ok(models) => models,
        Err(code) => return code,
    };
    if options.max_budget_usd.is_some() && !models.main.priced() {
        eprintln!(
            "fontaine: --max-budget-usd needs a known price, and rig's catalog has none for `{}`",
            models.main.reference
        );
        return EXIT_CONFIG;
    }
    let launch = Launch {
        cli,
        config,
        dirs,
        host,
    };
    let setup = agent_setup(&launch, prep, models, true, options.json_schema.clone());
    let runtime = match AgentRuntime::new(setup) {
        Ok(runtime) => runtime,
        Err(message) => {
            eprintln!("fontaine: {message}");
            return 1;
        }
    };
    headless::run(options, runtime, &dirs.data).await
}

async fn run_interactive(
    cli: &Cli,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
) -> u8 {
    let models = match resolve_models(cli, config, dirs, host).await {
        Ok(models) => models,
        Err(code) => return code,
    };
    let launch = Launch {
        cli,
        config,
        dirs,
        host,
    };
    let setup = agent_setup(&launch, prep, models, false, None);
    match fontaine_tui::run(setup, host.cwd.clone()).await {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("fontaine: {message}");
            1
        }
    }
}
