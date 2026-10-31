mod auto_mode;
pub mod cli;
pub mod headless;
mod plugin;
mod plugin_auto_update;
pub mod telemetry;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fontaine_core::agent::SessionOrigin;
use fontaine_core::agent::{AgentRuntime, AgentSetup};
use fontaine_core::config::{self, CliRules, Config, ConfigError, LoadParams, PermissionMode};
use fontaine_core::containment;
use fontaine_core::dirs::{self, Dirs};
use fontaine_core::extensions::{self, Extensions};
use fontaine_core::memory;
use fontaine_core::models::{self, Models, ResolveParams, Switcher};
use fontaine_core::notice::Notice;
use fontaine_core::permissions::DenyAll;
use fontaine_core::plugins;
use fontaine_core::scanner_stage::{self, ScannerStart};
use fontaine_core::scratch::{self, Scratch};
use fontaine_core::session::{self, Session, SessionError};
use fontaine_core::tools;
use fontaine_core::trust;
use fontaine_core::workspace::{self, RootSource};
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::powershell::PowerShellSettings;
use fontaine_tui::accessibility;
use fontaine_tui::theme::Theme;
use rig_core::schemars::Schema;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

use cli::{Cli, InputFormat, OutputFormat};
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

fn usage_code(message: impl std::fmt::Display) -> u8 {
    eprintln!("fontaine: {message}");
    EXIT_CONFIG
}

fn usage(message: impl std::fmt::Display) -> ExitCode {
    ExitCode::from(usage_code(message))
}

/// Loads the settings, and the directories they come from.
fn load_config(
    cli: &Cli,
    host: &Host,
    sink: Option<&DebugSink>,
) -> Result<(Config, Dirs), ConfigError> {
    telemetry::with_debug_only(sink, || {
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
        let config = config::load(&params)?;
        let stack: Vec<String> = config.scopes.iter().map(ToString::to_string).collect();
        tracing::debug!(scope_stack = ?stack, workspace = %config.workspace.display(), "config loaded");
        Ok::<_, ConfigError>((config, dirs))
    })
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
            fontaine_tools::error_chain::chain(&e)
        ))),
    }
}

/// What the workspace holds that the trust prompt is about.
fn trust_summary(config: &Config) -> fontaine_tui::trust_prompt::Summary {
    let hooks = config
        .hooks
        .pre_tool_use
        .iter()
        .chain(&config.hooks.post_tool_use)
        .chain(&config.hooks.permission_denied)
        .filter(|hook| hook.origin.is_project())
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
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // clap exits 0 for --help/--version and 2 for usage errors.
            let code = if e.use_stderr() { EXIT_CONFIG } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
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

    let (mut config, dirs) = match load_config(&cli, &host, sink.as_ref()) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(e.exit_code());
        }
    };

    if let Some(cli::Command::AutoMode(args)) = &cli.command {
        return auto_mode::run(&cli, args, &config, &dirs, &host);
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
    // loads it without asking, as Claude Code's `-p` does.
    let mut project_executables = true;
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
    // Headless has no conversation to show them in. Printing now, before models are resolved,
    // keeps them on stderr when a later startup check exits.
    if headless.is_some() {
        print_warnings(&std::mem::take(&mut prep.warnings));
    }
    // The pass runs once per process, in the background, and only on the interactive surface.
    // Its outcomes wait in the feed, which outlives any one session of a `/resume` chain.
    let (plugin_notices, plugin_feed) = fontaine_tui::notice_feed();
    if headless.is_none() && config.plugins.auto_update {
        plugin_auto_update::spawn(dirs.clone(), shared_env(&host), plugin_notices);
    }

    let export = telemetry::install(
        sink.as_ref(),
        &config,
        &InstallParams {
            env: &*host.env,
            data_dir: &dirs.data,
        },
    );
    let mut cli = cli;
    let mut headless = headless;
    let mut ax_warnings = ax_warnings;
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
                        run_headless(&cli, options, prep, &config, &dirs, &host).await,
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
                    export.finish(code);
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
    export.finish(code);
    ExitCode::from(code)
}

/// How a run of the session ended.
enum Outcome {
    Done(u8),
    /// The interactive session asked for another one to take its place.
    Switch(fontaine_tui::SessionTarget, Option<Box<Config>>),
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
    /// The session's scratch root, which is also the last of `roots`.
    pub scratch: Option<PathBuf>,
    /// The auto-memory store, also one of `roots`, when `[memory] auto` is on and it could be
    /// created.
    pub memory_root: Option<PathBuf>,
    pub mode: PermissionMode,
    pub scrub_env: Vec<OsString>,
    pub git_bash: Option<GitBash>,
    pub powershell: Option<PowerShellSettings>,
    pub home: Option<PathBuf>,
    pub extensions: Extensions,
    pub origin: SessionOrigin,
    /// Whether repository-defined executables may start.
    pub project_executables: bool,
    /// Claude Code's `start_type`: `fresh`, `resume` or `continue`.
    pub start_type: &'static str,
    /// Non-fatal warnings from reading the configuration. Headless prints them to stderr; the
    /// interactive surface shows them in the conversation.
    pub warnings: Vec<Notice>,
}

/// Opens, copies or creates the session the flags ask for. `record_resume` is whether starting
/// it appends the `Resumed` record; a copy made by `/branch` has its own already.
fn open_session(
    cli: &Cli,
    config: &Config,
    dirs: &Dirs,
    record_resume: bool,
    warnings: &mut Vec<Notice>,
) -> Result<(Session, SessionOrigin, &'static str), u8> {
    let fail = |e: SessionError| {
        eprintln!("fontaine: {e}");
        e.exit_code()
    };
    let name = session_name(cli)?;
    let mut name_it = |session: &Session| {
        if let Some(name) = &name
            && let Err(e) = session.rename(name)
        {
            warnings.push(Notice::warning(
                format!(
                    "the session was not named: {}",
                    fontaine_tools::error_chain::chain(&e)
                ),
                None,
            ));
        }
    };
    let requested = match cli.session_id.as_deref() {
        None => Uuid::new_v4(),
        Some(text) => Uuid::parse_str(text)
            .map_err(|e| usage_code(format!("--session-id must be a UUID: {e}")))?,
    }
    .hyphenated()
    .to_string();
    let (source, start_type) = match (&cli.resume, cli.continue_session) {
        (Some(text), _) => {
            let id = session::resolve(&dirs.data, &config.workspace, text).map_err(|e| {
                eprintln!("fontaine: {e}");
                e.exit_code()
            })?;
            (Some(id), "resume")
        }
        (None, true) => (
            session::latest_for_root(&dirs.data, &config.workspace),
            "continue",
        ),
        (None, false) => (None, "fresh"),
    };
    let Some(source) = source else {
        // `--continue` with nothing to continue is a fresh start.
        let session = Session::create(&dirs.data, &requested).map_err(fail)?;
        name_it(&session);
        return Ok((session, SessionOrigin::Fresh, "fresh"));
    };
    if cli.fork_session {
        let session =
            Session::fork_named(&dirs.data, &source, &requested, name.as_deref()).map_err(fail)?;
        let how = format!("fork ({start_type})");
        return Ok((
            session,
            SessionOrigin::Continued {
                source,
                how,
                record_resume,
            },
            start_type,
        ));
    }
    let session = Session::open(&dirs.data, &source).map_err(fail)?;
    name_it(&session);
    let how = if record_resume { start_type } else { "branch" }.to_string();
    Ok((
        session,
        SessionOrigin::Continued {
            source,
            how,
            record_resume,
        },
        start_type,
    ))
}

fn prepare_session(
    cli: &Cli,
    config: &mut Config,
    host: &Host,
    dirs: &Dirs,
    mode: PermissionMode,
    project_executables: bool,
    record_resume: bool,
) -> Result<(SessionPrep, Option<Scratch>), u8> {
    let git_bash = git_bash(host)?;
    let powershell = powershell(host);

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
    let mut roots = workspace::resolve(&config.workspace, &host.cwd, added);
    let mut warnings: Vec<Notice> = roots
        .warnings
        .iter()
        .map(|warning| Notice::warning(warning.clone(), None))
        .collect();

    // The caller only gets the warnings back on success, so a failure prints them here.
    let (session, origin, start_type) =
        open_session(cli, config, dirs, record_resume, &mut warnings).inspect_err(|_| {
            print_warnings(&warnings);
        })?;
    // The guard goes back to the caller, which drops it when the run ends; the root list only
    // carries the path. Failing to make one leaves the confinement as it was.
    warnings.extend(
        scratch::sweep_stale(&*host.env)
            .into_iter()
            .map(|warning| Notice::warning(warning, None)),
    );
    let protected = [dirs.config.clone(), dirs.data.clone()];
    let scratch = match Scratch::create(&*host.env, &protected, &session.id) {
        Ok(scratch) => Some(scratch),
        Err(e) => {
            warnings.push(Notice::warning(
                format!(
                    "no scratch directory for this session: {}",
                    fontaine_tools::error_chain::chain(&e)
                ),
                None,
            ));
            None
        }
    };
    let scratch_path = scratch.as_ref().map(|s| s.path().to_path_buf());
    let memory_root = if config.memory.auto {
        match memory::ensure_dir(&memory::dir(&dirs.data, &roots.roots[0])) {
            Ok(dir) => {
                roots.roots.push(dir.clone());
                Some(dir)
            }
            Err(e) => {
                warnings.push(Notice::warning(
                    format!(
                        "auto memory is off for this session: {}",
                        fontaine_tools::error_chain::chain(&e)
                    ),
                    None,
                ));
                None
            }
        }
    } else {
        None
    };
    // Last, so the scratch root stays the final root.
    roots.roots.extend(scratch_path.clone());
    // Revisions an earlier run staged replace the live checkouts before anything loads them; a
    // headless run neither stages nor swaps, so scripted runs see a fixed set of plugins.
    if !cli.print && config.plugins.auto_update {
        warnings.extend(
            plugin_auto_update::apply_staged(&dirs.config, std::time::SystemTime::now())
                .into_iter()
                .map(|warning| Notice::warning(warning, None)),
        );
    }
    // Enabled plugins are third-party but user-installed: they load in the TUI and headless
    // alike, and install is the approval that lets their hooks, servers and skill commands run
    // without the workspace-trust gate.
    let contributions = plugins::load_installed(
        &dirs.config,
        &plugins::PluginContext {
            project_root: &config.workspace,
            env: &*host.env,
        },
    )
    .map_err(|e| {
        print_warnings(&warnings);
        eprintln!("fontaine: {e}");
        e.exit_code()
    })?;
    config.apply_plugins(&contributions).map_err(|e| {
        print_warnings(&warnings);
        eprintln!("fontaine: {e}");
        e.exit_code()
    })?;
    warnings.extend(
        contributions
            .warnings
            .iter()
            .map(|warning| Notice::warning(warning.clone(), None)),
    );
    warnings.extend(
        config
            .lsp_table()
            .warnings
            .into_iter()
            .map(|warning| Notice::warning(warning, None)),
    );
    // Declined trust keeps a repository's servers from starting, but its agent files may still
    // name their tools: toolset cutting drops them.
    let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
    let mut extension_warnings = Vec::new();
    let extensions = extensions::load(&mut extensions::LoadParams {
        config_dir: &dirs.config,
        project_root: &config.workspace,
        servers: &servers,
        plugins: &contributions.dirs,
        warnings: &mut extension_warnings,
    })
    .map_err(|e| {
        print_warnings(&warnings);
        eprintln!("fontaine: {e}");
        e.exit_code()
    })?;
    warnings.extend(
        extension_warnings
            .iter()
            .map(|warning| Notice::warning(warning.clone(), None)),
    );
    let home = (host.env)(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(::dirs::home_dir)
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    let prep = SessionPrep {
        session,
        roots: roots.roots,
        scratch: scratch_path,
        memory_root,
        mode,
        scrub_env: tools::scrubbed_env(config, (host.env_names)()),
        git_bash,
        powershell,
        home,
        extensions,
        origin,
        project_executables,
        start_type,
        warnings,
    };
    Ok((prep, scratch))
}

/// The shell tool runs through Git Bash on Windows; elsewhere it needs none.
#[cfg(windows)]
fn git_bash(host: &Host) -> Result<Option<GitBash>, u8> {
    let probe = fontaine_tools::git_bash::Probe {
        env: &*host.env,
        is_file: &|path| path.is_file(),
    };
    fontaine_tools::git_bash::discover(&probe)
        .map(Some)
        .map_err(usage_code)
}

#[cfg(not(windows))]
fn git_bash(_host: &Host) -> Result<Option<GitBash>, u8> {
    Ok(None)
}

/// The `powershell` tool exists on Windows when an interpreter is found; elsewhere never.
#[cfg(windows)]
fn powershell(host: &Host) -> Option<PowerShellSettings> {
    fontaine_tools::powershell::discover(&fontaine_tools::powershell::Probe {
        env: &*host.env,
        is_file: &|path| path.is_file(),
    })
}

#[cfg(not(windows))]
fn powershell(_host: &Host) -> Option<PowerShellSettings> {
    None
}

async fn resolve_models(
    cli: &Cli,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    session_id: &str,
) -> Result<Models, u8> {
    models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: &*host.env,
        model_override: cli.model.as_deref(),
        session_id: Some(session_id.to_string()),
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

/// Loads the tool-result scanner, which for `pg2` may download its model, before the session
/// starts.
async fn start_scanner(launch: &Launch<'_>, prep: &SessionPrep, models: &Models) -> ScannerStart {
    scanner_stage::start(&scanner_stage::StartParams {
        config: launch.config,
        data_dir: &launch.dirs.data,
        scrub_env: &prep.scrub_env,
        env: &*launch.host.env,
        guardian: models.guardian.as_ref(),
        model_base_url: None,
    })
    .await
}

/// The environment as the session started, for resolving models again later.
fn shared_env(host: &Host) -> fontaine_core::agent::EnvLookup {
    let env: std::collections::HashMap<String, OsString> = (host.env_names)()
        .into_iter()
        .filter_map(|name| {
            let name = name.into_string().ok()?;
            let value = (host.env)(&name)?;
            Some((name, value))
        })
        .collect();
    Arc::new(move |name: &str| env.get(name).cloned())
}

fn agent_setup(
    launch: &Launch<'_>,
    prep: SessionPrep,
    models: Models,
    scanner: ScannerStart,
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
    let lookup = shared_env(host);
    let switcher = Switcher::new(
        config.clone(),
        dirs.data.clone(),
        lookup.clone(),
        Some(prep.session.id.clone()),
    );
    let mut config = config.clone();
    if headless && let Some(cap) = cli.max_budget_usd {
        // The tree's session ceiling counts root, guardian and child spend alike, so a running
        // child is stopped at the headless cap instead of finishing its own budget first.
        let ceiling = &mut config.agents.session_cost_budget_usd;
        *ceiling = Some(ceiling.map_or(cap, |c| c.min(cap)));
    }
    AgentSetup {
        config,
        config_dir: dirs.config.clone(),
        data_dir: dirs.data.clone(),
        models,
        session: prep.session,
        roots: prep.roots,
        scratch: prep.scratch,
        memory_root: prep.memory_root,
        mode: prep.mode,
        scrub_env: prep.scrub_env,
        git_bash: prep.git_bash,
        powershell: prep.powershell,
        system_prompt: cli.system_prompt.clone(),
        append_system_prompt: cli.append_system_prompt.clone(),
        json_schema,
        headless,
        approver: Arc::new(DenyAll),
        home: prep.home,
        user_config: Some(dirs.config.join(config::USER_FILE)),
        guardian_profile: resolved.profile,
        notices,
        startup_warnings: prep.warnings,
        scanner,
        switcher: Some(switcher),
        extensions: prep.extensions,
        project_executables: prep.project_executables,
        confirm_project_servers: !headless,
        scheduling: !headless,
        origin: prep.origin,
        env: lookup,
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
) -> u8 {
    let models = match resolve_models(cli, config, dirs, host, &prep.session.id).await {
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
    let scanner = start_scanner(&launch, &prep, &models).await;
    let setup = agent_setup(
        &launch,
        prep,
        models,
        scanner,
        true,
        options.json_schema.clone(),
    );
    let mut runtime = match AgentRuntime::new(setup) {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("fontaine: {}", fontaine_tools::error_chain::chain(&e));
            return 1;
        }
    };
    // Servers that fail to start are reported on stderr and skipped.
    runtime.connect_mcp(&[]).await;
    headless::run(options, runtime, &dirs.data).await
}

/// Whether adding an account, signing in or picking a model could fix the failure.
fn setup_can_help(error: &models::ModelError) -> bool {
    matches!(
        error,
        models::ModelError::Provider(_) | models::ModelError::Config(ConfigError::Model { .. })
    )
}

async fn run_interactive(
    cli: &Cli,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    screen_reader: Option<accessibility::Source>,
    notices: fontaine_tui::Notices,
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
        env: &*host.env,
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
                        fontaine_tools::error_chain::chain(&e)
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
    let launch = Launch {
        cli,
        config: effective,
        dirs,
        host,
    };
    let scanner = start_scanner(&launch, &prep, &models).await;
    let pending = (ax_warnings.clone(), prep.warnings.clone());
    let setup = agent_setup(&launch, prep, models, scanner, false, None);
    match fontaine_tui::run(
        setup,
        host.cwd.clone(),
        screen_reader,
        fontaine_tui::Notices {
            startup: ax_warnings,
            feed,
        },
    )
    .await
    {
        Ok(fontaine_tui::DriveOutcome::Exit) => Outcome::Done(0),
        Ok(fontaine_tui::DriveOutcome::Switch(target)) => {
            Outcome::Switch(target, added.map(Box::new))
        }
        Err(message) => {
            print_warnings(&pending.0);
            print_warnings(&pending.1);
            eprintln!("fontaine: {message}");
            Outcome::Done(1)
        }
    }
}
