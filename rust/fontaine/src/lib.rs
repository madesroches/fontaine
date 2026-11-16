mod attach_cmd;
mod auto_mode;
mod claude_compat;
pub mod cli;
mod doctor_cmd;
pub mod headless;
mod mcp_cmd;
mod mcp_probe;
mod models_cmd;
mod plugin;
mod plugin_auto_update;
pub mod release_signature;
mod stamp;
pub mod telemetry;
pub mod update;

#[cfg(test)]
#[path = "../build/version.rs"]
mod build_version;

/// `--version`, `fontaine doctor`, `/bug`, `/status` and telemetry all show this one value.
pub const VERSION: &str = env!("FONTAINE_VERSION");

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fontaine_sdk::assemble::{
    self, ConfigInputs, Environment, HeadlessStart, PrepareParams, SessionParams, SetupParams,
};
use fontaine_sdk::core::agent::AgentSetup;
use fontaine_sdk::core::config::{self, CliRules, Config, ConfigError, LoadParams, PermissionMode};
use fontaine_sdk::core::dirs::{self, Dirs};
use fontaine_sdk::core::extensions;
use fontaine_sdk::core::models::{self, ResolveParams};
use fontaine_sdk::core::notice::Notice;
use fontaine_sdk::core::scratch::Scratch;
use fontaine_sdk::core::session;
use fontaine_sdk::core::trust;
use fontaine_sdk::core::workspace;
use fontaine_sdk::error::EmbedError;
use fontaine_tui::accessibility;
use fontaine_tui::theme::Theme;
use rig_core::schemars::Schema;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cli::{Cli, InputFormat, OutputFormat};
use telemetry::{DebugSink, InstallParams};

pub type EnvFn = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;
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
            env: Arc::new(|name| std::env::var_os(name)),
            default_config_dir: ::dirs::config_dir(),
            default_data_dir: ::dirs::data_dir(),
            env_names: Box::new(|| std::env::vars_os().map(|(name, _)| name).collect()),
        })
    }
}

pub const EXIT_CONFIG: u8 = fontaine_sdk::core::config::CONFIG_EXIT_CODE;

fn usage_code(message: impl std::fmt::Display) -> u8 {
    eprintln!("fontaine: {message}");
    EXIT_CONFIG
}

fn usage(message: impl std::fmt::Display) -> ExitCode {
    ExitCode::from(usage_code(message))
}

fn resolve_dirs(host: &Host) -> Result<Dirs, ConfigError> {
    dirs::resolve(
        &*host.env,
        host.default_config_dir.clone(),
        host.default_data_dir.clone(),
    )
}

fn load_params<'a>(cli: &'a Cli, host: &'a Host, dirs: &'a Dirs) -> LoadParams<'a> {
    LoadParams {
        launch_dir: &host.cwd,
        config_dir: &dirs.config,
        data_dir: &dirs.data,
        settings_file: cli.settings.as_deref(),
        cli_rules: CliRules {
            allow: &cli.allowed_tools,
            deny: &cli.disallowed_tools,
            afk_temperature_ceiling: cli.afk_temperature_ceiling,
        },
        env: &*host.env,
    }
}

/// The process the session runs in, as the SDK's assembly takes it.
fn environment(host: &Host) -> Environment {
    Environment {
        cwd: host.cwd.clone(),
        env: host.env.clone(),
        env_names: (host.env_names)(),
    }
}

/// Loads the settings, and the directories they come from.
fn load_config(
    cli: &Cli,
    host: &Host,
    sink: Option<&DebugSink>,
) -> Result<(Config, Dirs), ConfigError> {
    telemetry::with_debug_only(sink, || {
        let dirs = resolve_dirs(host)?;
        let config = assemble::load_config(
            &environment(host),
            &dirs,
            &ConfigInputs {
                settings_file: cli.settings.as_deref(),
                allow: &cli.allowed_tools,
                deny: &cli.disallowed_tools,
                afk_temperature_ceiling: cli.afk_temperature_ceiling,
            },
        )?;
        Ok::<_, ConfigError>((config, dirs))
    })
}

/// The workspace roots: the project directory and the directories added by flag and settings.
fn resolve_roots(cli: &Cli, config: &Config, host: &Host) -> workspace::Roots {
    assemble::resolve_roots(config, &host.cwd, &cli.add_dir)
}

/// The user's home directory, canonical when it can be.
fn home_dir(host: &Host) -> Option<PathBuf> {
    assemble::home_dir(&*host.env)
}

/// Cuts a call off after `limit`; a stalled endpoint must not hang a command that has no
/// timeout of its own.
async fn timeout<T, E: std::error::Error + 'static>(
    limit: std::time::Duration,
    fut: impl Future<Output = Result<T, E>>,
) -> Result<T, Box<dyn std::error::Error>> {
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(Box::new(e)),
        Err(elapsed) => Err(Box::new(elapsed)),
    }
}

/// The name `--name` asks for, validated.
fn session_name(cli: &Cli) -> Result<Option<String>, u8> {
    cli.name
        .as_deref()
        .map(|name| session::validate_name(name).map_err(|e| usage_code(format!("--name: {e}"))))
        .transpose()
}

/// What the session flags ask for.
fn check_session_flags(cli: &Cli) -> Result<(), ExitCode> {
    session_name(cli).map_err(ExitCode::from)?;
    if let Some(text) = cli.session_id.as_deref()
        && let Err(e) = uuid::Uuid::parse_str(text)
    {
        return Err(usage(format!("--session-id must be a UUID: {e}")));
    }
    let continuing = cli.continue_session || cli.resume.is_some();
    if cli.fork_session && !continuing {
        return Err(usage("--fork-session needs --continue or --resume"));
    }
    if continuing && cli.session_id.is_some() && !cli.fork_session {
        return Err(usage(
            "--session-id cannot be combined with --continue or --resume unless --fork-session is also given",
        ));
    }
    Ok(())
}

/// Makes `--cd`'s directory the project directory, for `Host.cwd` and the process alike so that
/// children and relative flag values agree with it.
fn apply_cd(cli: &Cli, host: &mut Host) -> Result<(), u8> {
    let Some(dir) = &cli.cd else {
        return Ok(());
    };
    let change = || -> std::io::Result<PathBuf> {
        let target = host.cwd.join(dir).canonicalize()?;
        if !target.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        std::env::set_current_dir(&target)?;
        Ok(target)
    };
    match change() {
        Ok(target) => {
            host.cwd = target;
            Ok(())
        }
        Err(e) => Err(usage_code(format!(
            "--cd {}: {}",
            dir.display(),
            fontaine_sdk::tools::error_chain::chain(&e)
        ))),
    }
}

/// What the workspace holds that the trust prompt is about.
fn trust_summary(config: &Config) -> fontaine_tui::trust_prompt::Summary {
    let hooks = config
        .hooks
        .iter()
        .filter(|(event, _)| {
            event.project_scope() == fontaine_sdk::core::config::ProjectScope::Loads
        })
        .flat_map(|(_, hooks)| hooks)
        .filter(|hook| hook.origin.is_project())
        // `build()` drops http and prompt hooks from the shared project file. Those in
        // `settings.local.toml` are counted, though a demoted (tracked, no-git or outside-work-tree)
        // one stays untrusted and still has them dropped.
        .filter(|hook| {
            hook.kind == fontaine_sdk::core::config::HandlerKind::Command
                || hook.origin.slot != fontaine_sdk::core::config::Slot::Project
        })
        .count();
    let servers = config
        .mcp_servers
        .keys()
        .filter(|name| {
            config
                .mcp_origins
                .get(*name)
                .is_some_and(|o| o.is_project())
        })
        .cloned()
        .collect();
    let project = config.workspace.join(config::PROJECT_DIR);
    fontaine_tui::trust_prompt::Summary {
        hooks,
        servers,
        skills: project.join(extensions::SKILLS_DIR).is_dir(),
        settings_files: [config::PROJECT_FILE, config::PROJECT_LOCAL_FILE]
            .iter()
            .filter(|name| project.join(name).is_file())
            .map(|name| format!("{}/{name}", config::PROJECT_DIR))
            .collect(),
    }
}

/// Prints startup warnings that never reached the conversation, because the process is about to
/// exit or has no conversation.
fn print_warnings(warnings: &[Notice]) {
    for warning in warnings {
        eprintln!("fontaine: warning: {warning}");
    }
}

/// Whether screen-reader mode is on, from the flag, the environment and the settings, and the
/// warning for an environment value that was ignored. The confirmation is printed here, before
/// the terminal changes mode.
fn screen_reader_mode(
    cli: &Cli,
    config: &Config,
    host: &Host,
) -> (Option<accessibility::Source>, Vec<Notice>) {
    let env = (host.env)(accessibility::ENV_VAR).map(|v| v.to_string_lossy().into_owned());
    let mut warnings = Vec::new();
    if let Some(value) = env.as_deref()
        && !accessibility::env_is_valid(value)
    {
        warnings.push(Notice::warning(
            format!(
                "{}={value} is ignored; use 1 to turn screen-reader mode on or 0 to keep it off",
                accessibility::ENV_VAR
            ),
            None,
        ));
    }
    let source = accessibility::resolve(
        cli.ax_screen_reader,
        env.as_deref(),
        config.tui.screen_reader,
    );
    if let Some(source) = source {
        println!("{}", source.announcement());
    }
    (source, warnings)
}

pub fn run(args: impl IntoIterator<Item = OsString>, mut host: Host) -> ExitCode {
    let args: Vec<OsString> = args.into_iter().collect();
    // The Claude CLI adapter starts this executable as its inert tool inventory; the mode is
    // hidden and takes no part in the command line.
    if let Some(manifest) =
        fontaine_sdk::core::providers::claude_subscription::inventory::from_args(args.clone())
    {
        return match fontaine_sdk::core::providers::claude_subscription::inventory::serve(&manifest)
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "fontaine: the tool inventory failed: {}",
                    fontaine_sdk::tools::error_chain::chain(&e)
                );
                ExitCode::from(1)
            }
        };
    }
    // Claude Agent SDK mode: the shape every SDK launches its executable with. It runs the same
    // session on Fontaine's own flags, so the command line is translated and checked here, before
    // anything is read or any model is contacted.
    let mut sdk = None;
    let args = if claude_compat::cli::is_sdk_invocation(&args) {
        match claude_compat::cli::parse(&args) {
            Ok(options) => {
                let translated = options.fontaine_args.clone();
                sdk = Some(options);
                translated
            }
            Err(refusal) => {
                eprintln!("fontaine: {refusal}");
                return ExitCode::from(EXIT_CONFIG);
            }
        }
    } else {
        args
    };
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // clap exits 0 for --help/--version and 2 for usage errors.
            let code = if e.use_stderr() { EXIT_CONFIG } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    // The detached helper of a Windows update is told everything it needs on its command line.
    if let Some(cli::Command::UpdateHelper(args)) = &cli.command {
        return update::helper::main(&update::helper::HelperArgs {
            target: args.target.clone(),
            receipt: args.receipt.clone(),
            data: args.data.clone(),
        });
    }
    // Attaching needs no config, session or credentials: the ticket or a paired host is
    // everything.
    if let Some(cli::Command::Attach(args)) = &cli.command {
        return attach_cmd::run(args, &host);
    }
    if let Some(cli::Command::Remote(args)) = &cli.command {
        return attach_cmd::run_remote(args, &host);
    }
    if let Err(code) = check_session_flags(&cli) {
        return code;
    }
    if let Err(code) = apply_cd(&cli, &mut host) {
        return ExitCode::from(code);
    }

    // Plugin and marketplace changes run outside any session and need no permission pipeline:
    // the user typed them.
    if let Some(cli::Command::Plugin(args)) = &cli.command {
        return plugin::run(args, &host);
    }

    let sink = match DebugSink::open(cli.debug_file.as_deref(), (host.env)(telemetry::LOG_VAR)) {
        Ok(sink) => sink,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    // `mcp` edits files that may be the very ones that fail to load, so it loads the settings
    // itself, where a command needs them.
    if let Some(cli::Command::Mcp(args)) = &cli.command {
        return mcp_cmd::run(&cli, args, &host, sink.as_ref());
    }

    // Doctor loads the settings itself, so that a failure to load is part of what it reports.
    if let Some(cli::Command::Doctor(args)) = &cli.command {
        return doctor_cmd::run(&cli, args, &host, sink.as_ref());
    }

    let (mut config, dirs) = match load_config(&cli, &host, sink.as_ref()) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(e.exit_code());
        }
    };

    if let Some(sdk) = &sdk {
        claude_compat::apply_to_config(&mut config, sdk);
    }
    if let Some(cli::Command::AutoMode(args)) = &cli.command {
        return auto_mode::run(&cli, args, &config, &dirs, &host);
    }
    if let Some(cli::Command::Update(args)) = &cli.command {
        return update::run(args, &config, &dirs, &host);
    }
    if let Some(cli::Command::Models(args)) = &cli.command {
        return models_cmd::run(&cli, args, &config, &dirs, &host);
    }

    if let Err(code) = check_mode_flags(&cli) {
        return code;
    }
    if cli.remote && cli.print {
        return usage("--remote is interactive only; a headless run cannot be attached to");
    }
    let mode = session_mode(&cli, &config);
    if !cli.print && !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return usage("the interactive UI needs a terminal; use `fontaine -p` for scripted runs");
    }
    let (screen_reader, ax_warnings) = if cli.print {
        (None, Vec::new())
    } else {
        screen_reader_mode(&cli, &config, &host)
    };
    // The interactive UI asks before anything the repository defines may run. A headless run
    // loads it without asking, as Claude Code's `-p` does, unless `[headless] require_trust` is
    // set and the workspace is untrusted.
    let mut project_executables = true;
    if cli.print {
        let (allowed, line) = assemble::headless_project_executables(&config, &dirs.data);
        project_executables = allowed;
        if let Some(line) = line {
            eprintln!("fontaine: {line}");
        }
    }
    if !cli.print && !trust::is_trusted(&dirs.data, &config.workspace) {
        match fontaine_tui::trust_prompt::ask(
            &config.workspace,
            &trust_summary(&config),
            Theme::preset(config.tui.theme),
            screen_reader,
        ) {
            Ok(fontaine_tui::trust_prompt::Choice::Trust) => {
                if let Err(e) = trust::record(&dirs.data, &config.workspace) {
                    print_warnings(&ax_warnings);
                    eprintln!("fontaine: could not save the trust decision: {e}");
                    return ExitCode::from(1);
                }
                // An untracked settings.local.toml now loads with the user's trust.
                match load_config(&cli, &host, sink.as_ref()) {
                    Ok((reloaded, _)) => config = reloaded,
                    Err(e) => {
                        print_warnings(&ax_warnings);
                        eprintln!("fontaine: {e}");
                        return ExitCode::from(e.exit_code());
                    }
                }
            }
            Ok(fontaine_tui::trust_prompt::Choice::DontTrust) => project_executables = false,
            Err(e) => {
                print_warnings(&ax_warnings);
                eprintln!("fontaine: cannot show the trust prompt: {e}");
                return ExitCode::from(1);
            }
        }
    }
    let headless = if cli.print {
        match prepare_headless(&cli) {
            Ok(options) => Some(options),
            Err(code) => return code,
        }
    } else {
        None
    };
    let (mut prep, mut scratch) = match prepare_session(
        &cli,
        &mut config,
        &host,
        &dirs,
        mode,
        project_executables,
        true,
    ) {
        Ok(prepared) => prepared,
        Err(code) => {
            print_warnings(&ax_warnings);
            return ExitCode::from(code);
        }
    };
    if let Some(sdk) = &sdk {
        // Plugins load inside prepare_session, after the first pass of this filter.
        claude_compat::apply_strict_mcp(&mut config, sdk);
    }
    // Headless has no conversation to show them in. Printing now, before models are resolved,
    // keeps them on stderr when a later startup check exits.
    if headless.is_some() {
        print_warnings(&std::mem::take(&mut prep.warnings));
    }
    // The pass runs once per process, in the background, and only on the interactive surface.
    // Its outcomes wait in the feed, which outlives any one session of a `/resume` chain.
    let (plugin_notices, plugin_feed) = fontaine_tui::notice_feed();
    if headless.is_none() && config.plugins.auto_update {
        plugin_auto_update::spawn(dirs.clone(), shared_env(&host), plugin_notices.clone());
    }
    start_self_update(&config, &dirs, &host, headless.is_some(), &plugin_notices);

    let export = telemetry::install(
        sink.as_ref(),
        &config,
        &InstallParams {
            env: &*host.env,
            data_dir: &dirs.data,
        },
    );
    fontaine_tui::set_build_info(fontaine_tui::BuildInfo {
        version: VERSION,
        git_sha: env!("FONTAINE_GIT_SHA"),
        target: env!("FONTAINE_BUILD_TARGET"),
    });
    let mut cli = cli;
    let mut headless = headless;
    let mut ax_warnings = ax_warnings;
    // Declared outside the runtime's scope and the loop: the watchdog of a final quit must
    // outlive the session span, the scratch directory and the runtime's drop, and only
    // disarms once the telemetry has finished.
    let mut watchdog: Option<fontaine_tui::ExitWatchdog> = None;
    let code = match export.runtime() {
        // A `/resume` or `/branch` ends the run and starts another in the same process, so the
        // telemetry guard, the configuration and the trust decision carry across.
        Ok(runtime) => loop {
            let session = tracing::info_span!(
                target: telemetry::span_bridge::TARGET,
                "fontaine.session",
                start_type = prep.start_type,
                headless = cli.print,
            );
            let entered = session.enter();
            let outcome = runtime.block_on(async {
                match headless.take() {
                    Some(options) => Outcome::Done(
                        run_headless(&cli, options, prep, &config, &dirs, &host, sdk.take()).await,
                    ),
                    None => {
                        run_interactive(
                            &cli,
                            prep,
                            &config,
                            &dirs,
                            &host,
                            screen_reader,
                            fontaine_tui::Notices {
                                startup: std::mem::take(&mut ax_warnings),
                                feed: Some(plugin_feed.clone()),
                            },
                            &mut watchdog,
                        )
                        .await
                    }
                }
            });
            drop(entered);
            drop(session);
            // Removed before the next session prepares, which may be a resume of this one.
            drop(scratch.take());
            let target = match outcome {
                Outcome::Done(code) => break code,
                Outcome::Switch(target, added) => {
                    // Accounts the setup screen added belong to the next session too.
                    if let Some(added) = added {
                        config = *added;
                    }
                    // `/theme` and `/config` save the theme to the settings file; the config in
                    // hand predates them.
                    if let Ok((fresh, _)) = load_config(&cli, &host, sink.as_ref()) {
                        config.tui.theme = fresh.tui.theme;
                    }
                    target
                }
            };
            cli = Cli {
                resume: Some(target.source),
                continue_session: false,
                fork_session: false,
                session_id: None,
                name: None,
                ..cli
            };
            (prep, scratch) = match prepare_session(
                &cli,
                &mut config,
                &host,
                &dirs,
                mode,
                project_executables,
                target.record_resume,
            ) {
                Ok(prepared) => prepared,
                Err(code) => {
                    export.finish(code, None);
                    return ExitCode::from(code);
                }
            };
        },
        Err(e) => {
            print_warnings(&ax_warnings);
            print_warnings(&prep.warnings);
            eprintln!("fontaine: cannot start the async runtime: {e}");
            1
        }
    };
    // The runtime is gone by now; a final quit's telemetry spends what is left of its budget.
    export.finish(
        code,
        watchdog.as_ref().map(fontaine_tui::ExitWatchdog::deadline),
    );
    if let Some(watchdog) = watchdog {
        watchdog.disarm();
    }
    ExitCode::from(code)
}

/// How a run of the session ended.
enum Outcome {
    Done(u8),
    /// The interactive session asked for another one to take its place.
    Switch(fontaine_tui::SessionTarget, Option<Box<Config>>),
}

/// The mode the session is asked to start in: `--afk` or `--permission-mode`, then
/// `default_mode`, then what the surface starts in.
fn requested_mode(cli: &Cli, config: &Config) -> PermissionMode {
    let explicit = if cli.afk {
        Some(PermissionMode::Afk)
    } else {
        cli.permission_mode.map(PermissionMode::from)
    };
    assemble::requested_mode(explicit, config, cli.print)
}

/// The mode the session state is built in.
fn session_mode(cli: &Cli, config: &Config) -> PermissionMode {
    assemble::session_mode(requested_mode(cli, config))
}

/// `--afk` is shorthand for `--permission-mode afk`, so any other mode beside it is two answers
/// to one question.
fn check_mode_flags(cli: &Cli) -> Result<(), ExitCode> {
    match cli.permission_mode {
        Some(mode) if cli.afk && PermissionMode::from(mode) != PermissionMode::Afk => Err(usage(
            "two modes given: --afk is --permission-mode afk, so it cannot be combined with another --permission-mode",
        )),
        _ => Ok(()),
    }
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
    let max_usage_wait = match &cli.max_usage_wait {
        None => None,
        Some(text) => match fontaine_sdk::core::duration::parse_duration(text) {
            Some(wait)
                if !wait.is_zero()
                    && wait.as_secs() <= fontaine_sdk::core::config::MAX_USAGE_WAIT_SECS =>
            {
                Some(wait)
            }
            _ => {
                return Err(usage(
                    "--max-usage-wait must be a duration between a second and a year, such as `90m` or `6h`",
                ));
            }
        },
    };
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
        input_format: cli.input_format.into(),
        output_format: cli.output_format.into(),
        include_partial_messages: cli.include_partial_messages,
        max_turns: cli.max_turns,
        max_budget_usd: cli.max_budget_usd,
        on_usage_limit: cli.on_usage_limit.map(Into::into),
        max_usage_wait,
        json_schema,
    })
}

pub use fontaine_sdk::assemble::{SessionPrep, Started};

/// Which session the flags ask for.
fn session_params(cli: &Cli) -> SessionParams {
    SessionParams {
        name: cli.name.clone(),
        id: cli.session_id.clone(),
        resume: cli.resume.clone(),
        continue_session: cli.continue_session,
        fork: cli.fork_session,
    }
}

/// Opens or creates the session the flags ask for and gathers what the first model call needs.
/// `record_resume` is whether starting it appends the `Resumed` record; a copy made by `/branch`
/// has its own already.
fn prepare_session(
    cli: &Cli,
    config: &mut Config,
    host: &Host,
    dirs: &Dirs,
    mode: PermissionMode,
    project_executables: bool,
    record_resume: bool,
) -> Result<(SessionPrep, Option<Scratch>), u8> {
    let mut warnings = Vec::new();
    // Revisions an earlier run staged replace the live checkouts before anything loads them; a
    // headless run neither stages nor swaps, so scripted runs see a fixed set of plugins.
    if !cli.print && config.plugins.auto_update {
        warnings.extend(
            plugin_auto_update::apply_staged(&dirs.config, std::time::SystemTime::now())
                .into_iter()
                .map(|warning| Notice::warning(warning, None)),
        );
    }
    assemble::prepare_session(
        &PrepareParams {
            env: &environment(host),
            dirs,
            add_dirs: &cli.add_dir,
            session: &session_params(cli),
            mode,
            project_executables,
            record_resume,
        },
        config,
        &mut warnings,
    )
    .map_err(|e| {
        // The caller only gets the warnings back on success, so a failure prints them here.
        print_warnings(&warnings);
        usage_failure(&e)
    })
}

/// Prints a failed assembly and returns the exit code it ends the process with.
fn usage_failure(e: &EmbedError) -> u8 {
    // The sign-in line is the whole message; the model error under it only repeats it.
    match e {
        EmbedError::Credentials(d) => eprintln!("fontaine: {}", d.message()),
        _ => eprintln!("fontaine: {}", fontaine_sdk::tools::error_chain::chain(e)),
    }
    e.exit_code()
}

/// Tells the user how the last background update went, then starts this run's pass. Both stay
/// silent for an install no receipt describes.
fn start_self_update(
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    headless: bool,
    notices: &tokio::sync::mpsc::UnboundedSender<Notice>,
) {
    let receipt_path = update::receipt::receipt_path(&*host.env);
    let current_exe = std::env::current_exe().ok();
    if !update::auto::should_start(
        headless,
        config.updates.auto_update,
        receipt_path.as_deref(),
        current_exe.as_deref(),
    ) {
        return;
    }
    let (Some(receipt_path), Some(current_exe)) = (receipt_path, current_exe) else {
        return;
    };
    for notice in update::auto::startup_notices(&receipt_path, &current_exe, &dirs.data) {
        let _ = notices.send(notice);
    }
    match config.loaded_network(None) {
        Ok(network) => {
            update::auto::spawn(
                dirs.data.clone(),
                receipt_path,
                current_exe,
                network,
                notices.clone(),
            );
        }
        Err(e) => {
            let _ = notices.send(Notice::warning(
                format!(
                    "automatic update did not start: {}",
                    fontaine_sdk::tools::error_chain::chain(&e)
                ),
                None,
            ));
        }
    }
}

/// The environment, looked up live on every call: the Claude CLI adapter checks for conflicting
/// variables before each request, so a variable set after startup must be seen.
fn shared_env(host: &Host) -> fontaine_sdk::core::agent::EnvLookup {
    host.env.clone()
}

fn setup_params(
    cli: &Cli,
    config: &Config,
    headless: bool,
    json_schema: Option<Schema>,
) -> SetupParams {
    SetupParams {
        in_container: cli.in_container,
        effort: cli.effort,
        max_usage_wait: cli
            .max_usage_wait
            .as_deref()
            .and_then(fontaine_sdk::core::duration::parse_duration),
        on_usage_limit: cli.on_usage_limit.map(Into::into),
        max_budget_usd: cli.max_budget_usd,
        system_prompt: cli.system_prompt.clone(),
        append_system_prompt: cli.append_system_prompt.clone(),
        json_schema,
        headless,
        afk: requested_mode(cli, config) == PermissionMode::Afk,
        warn_no_search: !NO_SEARCH_WARNED.swap(true, Ordering::Relaxed),
    }
}

static NO_SEARCH_WARNED: AtomicBool = AtomicBool::new(false);

async fn run_headless(
    cli: &Cli,
    options: headless::Options,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    sdk: Option<claude_compat::cli::Options>,
) -> u8 {
    if let Some(sdk) = sdk {
        return claude_compat::run(sdk, cli, options, prep, config, dirs, host).await;
    }
    let started = match start_headless(cli, &options, prep, config, dirs, host, |_| {}).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    headless::run(options, started.runtime, &dirs.data, started.guardian_login).await
}

/// Resolves the models, starts the scanner, builds the runtime of a headless session and connects
/// its MCP servers. `tweak` adjusts the setup before the runtime is built.
async fn start_headless(
    cli: &Cli,
    options: &headless::Options,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    tweak: impl FnOnce(&mut AgentSetup),
) -> Result<Started, u8> {
    let env = shared_env(host);
    assemble::start_headless(
        HeadlessStart {
            config,
            dirs,
            env: &env,
            model_override: cli.model.as_deref(),
            setup: setup_params(cli, config, true, options.json_schema.clone()),
            scanner_notice: None,
            budget_name: "--max-budget-usd",
        },
        prep,
        tweak,
    )
    .await
    .map_err(|e| usage_failure(&e))
}

/// Whether adding an account, signing in or picking a model could fix the failure.
fn setup_can_help(error: &models::ModelError) -> bool {
    matches!(
        error,
        models::ModelError::Provider { .. } | models::ModelError::Config(ConfigError::Model { .. })
    )
}

/// The main model's account and the login that signs it in, when `error` is that account's
/// external program having nobody signed in.
fn signed_out_login(
    config: &Config,
    error: &models::ModelError,
    model_override: Option<&str>,
    host: &Host,
) -> Option<(String, fontaine_sdk::core::providers::ExternalLogin)> {
    assemble::signed_out_login(config, error, model_override, &shared_env(host))
}

#[allow(clippy::too_many_arguments)] // the watchdog slot belongs to the caller, past this session
async fn run_interactive(
    cli: &Cli,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    screen_reader: Option<accessibility::Source>,
    notices: fontaine_tui::Notices,
    watchdog: &mut Option<fontaine_tui::ExitWatchdog>,
) -> Outcome {
    let fontaine_tui::Notices {
        startup: ax_warnings,
        feed,
    } = notices;
    // Every exit before the screen draws leaves the warnings nowhere else to go.
    let bail = |prep: &SessionPrep| {
        print_warnings(&ax_warnings);
        print_warnings(&prep.warnings);
    };
    let resolved = models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: shared_env(host),
        model_override: cli.model.as_deref(),
        session_id: Some(prep.session.id.clone()),
    })
    .await;
    // The setup screen may add accounts, so the session starts from the configuration it returns.
    let (models, added) = match resolved {
        Ok(models) => (models, None),
        Err(error) if setup_can_help(&error) => {
            let params = fontaine_tui::setup::SetupParams {
                config: config.clone(),
                dirs: dirs.clone(),
                env: shared_env(host),
                error: error.to_string(),
                model_override: cli.model.clone(),
                session_id: prep.session.id.clone(),
                user_file: dirs.config.join(config::USER_FILE),
                screen_reader,
                not_signed_in: signed_out_login(config, &error, cli.model.as_deref(), host),
            };
            match fontaine_tui::setup::run(params).await {
                Ok(Some((models, config))) => (models, Some(config)),
                Ok(None) => {
                    bail(&prep);
                    eprintln!("fontaine: {error}");
                    return Outcome::Done(error.exit_code());
                }
                Err(e) => {
                    bail(&prep);
                    eprintln!(
                        "fontaine: cannot show the setup screen: {}",
                        fontaine_sdk::tools::error_chain::chain(&e)
                    );
                    eprintln!("fontaine: {error}");
                    return Outcome::Done(error.exit_code());
                }
            }
        }
        Err(error) => {
            bail(&prep);
            eprintln!("fontaine: {error}");
            return Outcome::Done(error.exit_code());
        }
    };
    let effective = added.as_ref().unwrap_or(config);
    let env = shared_env(host);
    let scanner =
        assemble::start_scanner(effective, &dirs.data, &prep.scrub_env, &env, &models, None).await;
    let pending = (ax_warnings.clone(), prep.warnings.clone());
    let setup = assemble::agent_setup(
        effective,
        dirs,
        &env,
        prep,
        models,
        scanner,
        setup_params(cli, effective, false, None),
    );
    match fontaine_tui::run(
        setup,
        host.cwd.clone(),
        screen_reader,
        fontaine_tui::Notices {
            startup: ax_warnings,
            feed,
        },
        // Enabled for this launch only: a switch to another session (`/resume`, `/branch`)
        // starts with remote control off, and `--resume` never turns it back on.
        cli.remote && cli.resume.is_none() && !cli.continue_session && !cli.fork_session,
    )
    .await
    {
        Ok(ended) => match ended.outcome {
            fontaine_tui::DriveOutcome::Exit => {
                // Still armed: the caller disarms it after the runtime and the telemetry are
                // done with.
                *watchdog = ended.watchdog;
                Outcome::Done(0)
            }
            fontaine_tui::DriveOutcome::Switch(target) => {
                Outcome::Switch(target, added.map(Box::new))
            }
        },
        Err(message) => {
            print_warnings(&pending.0);
            print_warnings(&pending.1);
            eprintln!("fontaine: {message}");
            Outcome::Done(1)
        }
    }
}

#[cfg(test)]
mod trust_tests {
    use super::*;

    #[test]
    fn project_team_hooks_count_in_the_trust_summary() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let project = work.join(config::PROJECT_DIR);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(config::PROJECT_FILE),
            "[[hooks.pre_tool_use]]\ncommand = \"a.sh\"\n\
             [[hooks.task_completed]]\ncommand = \"b.sh\"\n\
             [[hooks.teammate_idle]]\ncommand = \"c.sh\"\n",
        )
        .unwrap();
        let env = |_: &str| None;
        let config = config::load(&LoadParams {
            launch_dir: &work,
            config_dir: &dir.path().join("config"),
            data_dir: &dir.path().join("data"),
            settings_file: None,
            cli_rules: Default::default(),
            env: &env,
        })
        .unwrap();
        assert_eq!(trust_summary(&config).hooks, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stalled_call_is_cut_off() {
        let stalled = std::future::pending::<Result<(), std::fmt::Error>>();
        let result = timeout(std::time::Duration::from_millis(10), stalled).await;
        assert!(result.is_err());
    }
}
