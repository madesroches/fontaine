//! Session assembly: what happens between "here are the settings and the flags" and a built
//! [`AgentRuntime`]. The `fontaine` binary and [`SessionBuilder`](crate::SessionBuilder) both run
//! these functions, so a CLI session and an embedded one are set up the same way. Nothing here
//! prints: warnings come back as data and failures as [`EmbedError`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rig_core::schemars::Schema;
use uuid::Uuid;

use crate::core::agent::{AgentRuntime, AgentSetup, EnvLookup, SessionOrigin};
use crate::core::config::{
    self, CliRules, Config, ConfigError, LoadParams, OnUsageLimit, PermissionMode,
};
use crate::core::containment;
use crate::core::dirs::Dirs;
use crate::core::effort::Effort;
use crate::core::extensions::{self, Extensions};
use crate::core::memory;
use crate::core::models::{self, ModelError, Models, ResolveParams, Switcher};
use crate::core::notice::Notice;
use crate::core::permissions::{DenyAll, HumanChannel};
use crate::core::plugins;
use crate::core::providers::ExternalLogin;
use crate::core::scanner_stage::{self, ScannerStart};
use crate::core::scratch::{self, Scratch};
use crate::core::session::{self, Session};
use crate::core::tools;
use crate::core::trust;
use crate::core::workspace::{self, RootSource};
use crate::error::EmbedError;
use crate::tools::error_chain::chain;
use crate::tools::git_bash::GitBash;
use crate::tools::powershell::PowerShellSettings;

/// The process the session runs in, injected so only the program's entry point touches the real
/// one.
#[derive(Clone)]
pub struct Environment {
    /// The launch directory: where the project settings are looked up from.
    pub cwd: PathBuf,
    pub env: EnvLookup,
    /// The names present in the environment, for the variables found by prefix rather than by
    /// name. An [`EnvLookup`] cannot list them.
    pub env_names: Vec<OsString>,
}

impl Environment {
    /// The real process environment.
    pub fn from_process(cwd: PathBuf) -> Environment {
        Environment {
            cwd,
            env: Arc::new(|name| std::env::var_os(name)),
            env_names: std::env::vars_os().map(|(name, _)| name).collect(),
        }
    }
}

/// What the command line or builder asks of the settings load.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfigInputs<'a> {
    pub settings_file: Option<&'a Path>,
    pub allow: &'a [String],
    pub deny: &'a [String],
    pub afk_temperature_ceiling: Option<f64>,
}

/// Loads the settings from the scopes under `dirs` and the project of `env.cwd`.
pub fn load_config(
    env: &Environment,
    dirs: &Dirs,
    inputs: &ConfigInputs<'_>,
) -> Result<Config, ConfigError> {
    let config = config::load(&LoadParams {
        launch_dir: &env.cwd,
        config_dir: &dirs.config,
        data_dir: &dirs.data,
        settings_file: inputs.settings_file,
        cli_rules: CliRules {
            allow: inputs.allow,
            deny: inputs.deny,
            afk_temperature_ceiling: inputs.afk_temperature_ceiling,
        },
        env: &*env.env,
    })?;
    let stack: Vec<String> = config.scopes.iter().map(ToString::to_string).collect();
    tracing::debug!(scope_stack = ?stack, workspace = %config.workspace.display(), "config loaded");
    Ok(config)
}

/// The workspace roots: the project directory and the directories added by flag and settings.
pub fn resolve_roots(config: &Config, launch_dir: &Path, add_dirs: &[PathBuf]) -> workspace::Roots {
    let added = add_dirs
        .iter()
        .map(|dir| (RootSource::Flag, dir.clone()))
        .chain(
            config
                .permissions
                .additional_directories
                .iter()
                .map(|dir| (RootSource::Settings, PathBuf::from(dir))),
        );
    workspace::resolve(&config.workspace, launch_dir, added)
}

/// The user's home directory, canonical when it can be.
pub fn home_dir(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    env(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(::dirs::home_dir)
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
}

/// The mode the session is asked to start in: the explicit one (`--afk`, `--permission-mode`),
/// then `default_mode`, then what the surface starts in. The interactive UI starts in `auto`; a
/// headless run starts in `manual`, as Claude Code's `-p` starts in `default`.
pub fn requested_mode(
    explicit: Option<PermissionMode>,
    config: &Config,
    headless: bool,
) -> PermissionMode {
    explicit
        .or(config.permissions.default_mode)
        .unwrap_or(if headless {
            PermissionMode::Manual
        } else {
            PermissionMode::Auto
        })
}

/// The mode the session state is built in. AFK is entered once the state exists, so that its
/// refusal checks and ledger record run; a refusal leaves the session in `auto`.
pub fn session_mode(requested: PermissionMode) -> PermissionMode {
    match requested {
        PermissionMode::Afk => PermissionMode::Auto,
        mode => mode,
    }
}

/// Whether a headless run may start the repository's hooks, MCP servers and skill commands, and
/// the line that says why it may not: `[headless] require_trust` with an untrusted workspace.
pub fn headless_project_executables(config: &Config, data_dir: &Path) -> (bool, Option<String>) {
    if config.headless.require_trust && !trust::is_trusted(data_dir, &config.workspace) {
        return (
            false,
            Some(
                "workspace is not trusted; project hooks, MCP servers and project skill commands are skipped ([headless] require_trust = true)"
                    .to_string(),
            ),
        );
    }
    (true, None)
}

/// Which session a start asks for.
#[derive(Clone, Debug, Default)]
pub struct SessionParams {
    pub name: Option<String>,
    /// The id the new session gets, a UUID.
    pub id: Option<String>,
    /// The session to resume: an id or a name.
    pub resume: Option<String>,
    /// Resume the latest session of this workspace; a fresh start when there is none.
    pub continue_session: bool,
    /// Copy the resumed session instead of reopening it.
    pub fork: bool,
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
    pub plugins: plugins::PluginContributions,
    pub origin: SessionOrigin,
    /// Whether repository-defined executables may start.
    pub project_executables: bool,
    /// Claude Code's `start_type`: `fresh`, `resume` or `continue`.
    pub start_type: &'static str,
    /// Non-fatal warnings from reading the configuration. Headless surfaces report them as they
    /// are; the interactive one shows them in the conversation.
    pub warnings: Vec<Notice>,
}

/// The shells found at startup.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shells {
    pub git_bash: Option<GitBash>,
    pub powershell: Option<PowerShellSettings>,
}

/// Windows runs commands in Git Bash when it is found and in PowerShell when it is not. A
/// `FONTAINE_GIT_BASH_PATH` that names something else is an error rather than a reason to switch
/// shells: the user asked for a specific bash. `powershell_override` is the non-empty
/// `FONTAINE_POWERSHELL_PATH`, which is why discovery found no PowerShell when one is set.
#[cfg(any(windows, test))]
pub fn choose_shells(
    git_bash: Result<GitBash, crate::tools::git_bash::GitBashError>,
    powershell: Option<PowerShellSettings>,
    powershell_override: Option<OsString>,
) -> Result<Shells, String> {
    use crate::tools::git_bash::{GitBashError, OVERRIDE_VAR as GIT_BASH_VAR};
    use crate::tools::powershell::OVERRIDE_VAR as POWERSHELL_VAR;
    match (git_bash, powershell) {
        (Ok(git_bash), powershell) => Ok(Shells {
            git_bash: Some(git_bash),
            powershell,
        }),
        (Err(e @ GitBashError::BadOverride(_)), _) => Err(e.to_string()),
        (Err(GitBashError::NotFound), Some(powershell)) => Ok(Shells {
            git_bash: None,
            powershell: Some(powershell),
        }),
        (Err(GitBashError::NotFound), None) => Err(match powershell_override {
            Some(value) => format!(
                "no shell found: install Git for Windows, or set {GIT_BASH_VAR}; {POWERSHELL_VAR}={} is not a file",
                value.to_string_lossy()
            ),
            None => format!(
                "no shell found: install Git for Windows, or set {GIT_BASH_VAR} or {POWERSHELL_VAR}"
            ),
        }),
    }
}

/// Git Bash and PowerShell on Windows; elsewhere neither, since `sh` is native.
#[cfg(windows)]
fn shells(env: &Environment) -> Result<Shells, EmbedError> {
    let git_bash = crate::tools::git_bash::discover(&crate::tools::git_bash::Probe {
        env: &*env.env,
        is_file: &|path| path.is_file(),
        read_file: &|path| std::fs::read_to_string(path).ok(),
    });
    let powershell = crate::tools::powershell::discover(&crate::tools::powershell::Probe {
        env: &*env.env,
        is_file: &|path| path.is_file(),
    });
    let powershell_override =
        (env.env)(crate::tools::powershell::OVERRIDE_VAR).filter(|value| !value.is_empty());
    choose_shells(git_bash, powershell, powershell_override).map_err(EmbedError::config)
}

#[cfg(not(windows))]
fn shells(_env: &Environment) -> Result<Shells, EmbedError> {
    Ok(Shells::default())
}

/// Opens, copies or creates the session `params` asks for. `record_resume` is whether starting
/// it appends the `Resumed` record; a copy made by `/branch` has its own already.
pub fn open_session(
    params: &SessionParams,
    config: &Config,
    dirs: &Dirs,
    record_resume: bool,
    warnings: &mut Vec<Notice>,
) -> Result<(Session, SessionOrigin, &'static str), EmbedError> {
    let fail = |e: session::SessionError| {
        let code = e.exit_code();
        EmbedError::session_from(e, code)
    };
    let name = params
        .name
        .as_deref()
        .map(|name| session::validate_name(name).map_err(EmbedError::config_from))
        .transpose()?;
    let mut name_it = |session: &Session| {
        if let Some(name) = &name
            && let Err(e) = session.rename(name)
        {
            warnings.push(Notice::warning(
                format!("the session was not named: {}", chain(&e)),
                None,
            ));
        }
    };
    let requested = match params.id.as_deref() {
        None => Uuid::new_v4(),
        Some(text) => Uuid::parse_str(text)
            .map_err(|e| EmbedError::config_caused("the session id must be a UUID", e))?,
    }
    .hyphenated()
    .to_string();
    let (source, start_type) = match (&params.resume, params.continue_session) {
        (Some(text), _) => {
            let id = session::resolve(&dirs.data, &config.workspace, text).map_err(|e| {
                let code = e.exit_code();
                EmbedError::session_from(e, code)
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
    if params.fork {
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

/// What [`prepare_session`] needs beside the settings.
pub struct PrepareParams<'a> {
    pub env: &'a Environment,
    pub dirs: &'a Dirs,
    /// `--add-dir` and its builder twin.
    pub add_dirs: &'a [PathBuf],
    pub session: &'a SessionParams,
    pub mode: PermissionMode,
    pub project_executables: bool,
    pub record_resume: bool,
}

/// Opens the session and gathers what the first model call needs: the roots, the scratch and
/// memory directories, the installed plugins and the extension files.
///
/// `warnings` holds whatever the caller already has, and the ones found here are appended to it,
/// so a failure still leaves them with the caller; on success they also move into
/// [`SessionPrep::warnings`]. The scratch guard goes back to the caller, which drops it when the
/// run ends; the root list only carries its path.
pub fn prepare_session(
    p: &PrepareParams<'_>,
    config: &mut Config,
    warnings: &mut Vec<Notice>,
) -> Result<(SessionPrep, Option<Scratch>), EmbedError> {
    let Shells {
        git_bash,
        powershell,
    } = shells(p.env)?;
    let env = &*p.env.env;
    let mut roots = resolve_roots(config, &p.env.cwd, p.add_dirs);
    warnings.extend(
        roots
            .warnings
            .iter()
            .map(|warning| Notice::warning(warning.clone(), None)),
    );
    let (session, origin, start_type) =
        open_session(p.session, config, p.dirs, p.record_resume, warnings)?;
    // Failing to make a scratch directory leaves the confinement as it was.
    warnings.extend(
        scratch::sweep_stale(env)
            .into_iter()
            .map(|warning| Notice::warning(warning, None)),
    );
    let protected = [p.dirs.config.clone(), p.dirs.data.clone()];
    let scratch = match Scratch::create(env, &protected, &session.id) {
        Ok(scratch) => Some(scratch),
        Err(e) => {
            warnings.push(Notice::warning(
                format!("no scratch directory for this session: {}", chain(&e)),
                None,
            ));
            None
        }
    };
    let scratch_path = scratch.as_ref().map(|s| s.path().to_path_buf());
    let memory_root = if config.memory.auto {
        match memory::ensure_dir(&memory::dir(&p.dirs.data, &roots.roots[0])) {
            Ok(dir) => {
                roots.roots.push(dir.clone());
                Some(dir)
            }
            Err(e) => {
                warnings.push(Notice::warning(
                    format!("auto memory is off for this session: {}", chain(&e)),
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
    // Enabled plugins are third-party but user-installed: they load in the TUI and headless
    // alike, and install is the approval that lets their hooks, servers and skill commands run
    // without the workspace-trust gate.
    let contributions = plugins::load_installed(
        &p.dirs.config,
        &plugins::PluginContext {
            project_root: &config.workspace,
            env,
        },
    )
    .map_err(EmbedError::config_from)?;
    config
        .apply_plugins(&contributions)
        .map_err(EmbedError::config_from)?;
    for text in config
        .warnings
        .iter()
        .chain(&contributions.warnings)
        .chain(&config.lsp_table().warnings)
    {
        warnings.push(Notice::warning(text.clone(), None));
    }
    // Declined trust keeps a repository's servers from starting, but its agent files may still
    // name their tools: toolset cutting drops them.
    let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
    let mut extension_warnings = Vec::new();
    let extensions = extensions::load(&mut extensions::LoadParams {
        config_dir: &p.dirs.config,
        project_root: &config.workspace,
        servers: &servers,
        plugins: &contributions.dirs,
        warnings: &mut extension_warnings,
    })
    .map_err(EmbedError::config_from)?;
    warnings.extend(
        extension_warnings
            .into_iter()
            .map(|warning| Notice::warning(warning, None)),
    );
    let prep = SessionPrep {
        session,
        roots: roots.roots,
        scratch: scratch_path,
        memory_root,
        mode: p.mode,
        scrub_env: tools::scrubbed_env(config, p.env.env_names.clone()),
        git_bash,
        powershell,
        home: home_dir(env),
        extensions,
        plugins: contributions,
        origin,
        project_executables: p.project_executables,
        start_type,
        warnings: std::mem::take(warnings),
    };
    Ok((prep, scratch))
}

/// The main model's account and the login that signs it in, when `error` is that account's
/// external program having nobody signed in.
pub fn signed_out_login(
    config: &Config,
    error: &ModelError,
    model_override: Option<&str>,
    env: &EnvLookup,
) -> Option<(String, ExternalLogin)> {
    if !error.is_not_signed_in() {
        return None;
    }
    let account = models::main_reference(config, model_override)?.account;
    let login = config
        .accounts
        .get(&account)
        .and_then(|a| a.kind.provider().external_login(a, env))?;
    Some((account, login))
}

/// Resolves the main and guardian models.
pub async fn resolve_models(
    config: &Config,
    dirs: &Dirs,
    env: &EnvLookup,
    model_override: Option<&str>,
    session_id: &str,
) -> Result<Models, EmbedError> {
    models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: env.clone(),
        model_override,
        session_id: Some(session_id.to_string()),
    })
    .await
    .map_err(|e| {
        let code = e.exit_code();
        match signed_out_login(config, &e, model_override, env) {
            Some((account, login)) => EmbedError::credentials_from(
                format!(
                    "account `{account}` is not signed in; run `{}`, then retry",
                    login.display
                ),
                e,
                code,
            ),
            None => EmbedError::model_from(e, code),
        }
    })
}

/// Loads the tool-result scanner, which for `pg2` may download its model, before the session
/// starts. `notice` receives the download line; with none it goes to stderr.
pub async fn start_scanner(
    config: &Config,
    data_dir: &Path,
    scrub_env: &[OsString],
    env: &EnvLookup,
    models: &Models,
    notice: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> ScannerStart {
    scanner_stage::start(&scanner_stage::StartParams {
        config,
        data_dir,
        scrub_env,
        env: &**env,
        guardian: models.guardian.as_ref(),
        model_base_url: None,
        notice,
    })
    .await
}

/// What shapes the [`AgentSetup`] beside the settings and the prepared session.
#[derive(Clone, Default)]
pub struct SetupParams {
    /// `--in-container`: the user's assertion for the guardian's `minimal` profile.
    pub in_container: bool,
    pub effort: Option<Effort>,
    pub max_usage_wait: Option<Duration>,
    pub on_usage_limit: Option<OnUsageLimit>,
    /// The headless budget cap; ignored by an interactive session.
    pub max_budget_usd: Option<f64>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub json_schema: Option<Schema>,
    pub headless: bool,
    /// The session asked to start in AFK mode.
    pub afk: bool,
    /// Whether to say that web search is unavailable. A `/resume` or `/branch` builds another
    /// runtime in the same process, so the launcher turns this off after the first.
    pub warn_no_search: bool,
}

/// The [`AgentSetup`] of a session, with the approver denying every prompt: the caller swaps in
/// its own.
pub fn agent_setup(
    config: &Config,
    dirs: &Dirs,
    env: &EnvLookup,
    prep: SessionPrep,
    models: Models,
    scanner: ScannerStart,
    p: SetupParams,
) -> AgentSetup {
    let resolved = containment::resolve(
        config.guardian.profile,
        p.in_container,
        containment::detect(),
    );
    let mut notices = models.notices.clone();
    notices.extend(resolved.notice);
    let switcher = Switcher::new(
        config.clone(),
        dirs.data.clone(),
        env.clone(),
        Some(prep.session.id.clone()),
    );
    let mut config = config.clone();
    config.effort = p.effort.or(config.effort);
    if let Some(wait) = p.max_usage_wait {
        config.headless.max_usage_wait_secs = wait.as_secs();
    }
    if let Some(action) = p.on_usage_limit {
        config.headless.on_usage_limit = action;
    }
    let explicit_budget_cap = p.headless && p.max_budget_usd.is_some();
    if p.headless
        && let Some(cap) = p.max_budget_usd
    {
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
        system_prompt: p.system_prompt,
        append_system_prompt: p.append_system_prompt,
        json_schema: p.json_schema,
        headless: p.headless,
        human_channel: if p.headless {
            HumanChannel::Absent
        } else {
            HumanChannel::Present
        },
        callback_hooks: Vec::new(),
        session_agents: Vec::new(),
        host_mcp: None,
        host_tools: Vec::new(),
        root_tools: None,
        afk: p.afk,
        approver: Arc::new(DenyAll),
        home: prep.home,
        user_config: Some(dirs.config.join(config::USER_FILE)),
        guardian_profile: resolved.profile,
        notices,
        startup_warnings: prep.warnings,
        scanner,
        switcher: Some(switcher),
        extensions: prep.extensions,
        plugins: prep.plugins,
        project_executables: prep.project_executables,
        confirm_project_servers: !p.headless,
        scheduling: !p.headless,
        origin: prep.origin,
        env: env.clone(),
        warn_no_search: p.warn_no_search,
        explicit_budget_cap,
    }
}

/// Receives a line the assembly would otherwise print.
pub type NoticeFn = Arc<dyn Fn(&str) + Send + Sync>;

/// What [`start_headless`] needs beside the prepared session.
pub struct HeadlessStart<'a> {
    pub config: &'a Config,
    pub dirs: &'a Dirs,
    pub env: &'a EnvLookup,
    /// `--model`, a reference or a role name.
    pub model_override: Option<&'a str>,
    pub setup: SetupParams,
    /// Receives the PromptGuard download line; with none it goes to stderr.
    pub scanner_notice: Option<NoticeFn>,
    /// What the budget cap is called where the caller set it, for the refusal that names it.
    pub budget_name: &'a str,
}

/// A headless session built and ready to run.
pub struct Started {
    pub runtime: AgentRuntime,
    /// The pinned guardian's account and the login that signs it in, when that account has nobody
    /// signed in: a run that consults the guardian must stop before it starts.
    pub guardian_login: Option<(String, ExternalLogin)>,
}

/// Resolves the models, starts the scanner, builds the runtime of a headless session and connects
/// its MCP servers; servers that fail to start are reported through the approver and skipped.
/// `tweak` adjusts the setup before the runtime is built.
pub async fn start_headless(
    p: HeadlessStart<'_>,
    prep: SessionPrep,
    tweak: impl FnOnce(&mut AgentSetup),
) -> Result<Started, EmbedError> {
    let models =
        resolve_models(p.config, p.dirs, p.env, p.model_override, &prep.session.id).await?;
    if p.setup.max_budget_usd.is_some() && !models.main.priced() {
        return Err(EmbedError::config(format!(
            "{} needs a known price, and rig's catalog has none for `{}`",
            p.budget_name, models.main.reference
        )));
    }
    let guardian_login = match &models.guardian {
        Some(models::GuardianModel::Unavailable {
            reference,
            not_signed_in: true,
            ..
        }) => p
            .config
            .accounts
            .get(&reference.account)
            .and_then(|a| a.kind.provider().external_login(a, p.env))
            .map(|login| (reference.account.clone(), login)),
        _ => None,
    };
    let scanner = start_scanner(
        p.config,
        &p.dirs.data,
        &prep.scrub_env,
        p.env,
        &models,
        p.scanner_notice.as_deref(),
    )
    .await;
    let mut setup = agent_setup(p.config, p.dirs, p.env, prep, models, scanner, p.setup);
    tweak(&mut setup);
    let mut runtime = AgentRuntime::new(setup).map_err(EmbedError::runtime_from)?;
    runtime.connect_mcp(&[]).await;
    Ok(Started {
        runtime,
        guardian_login,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::git_bash::GitBashError;

    fn git() -> GitBash {
        GitBash {
            bin: PathBuf::from("/g/bin"),
            usr_bin: PathBuf::from("/g/usr/bin"),
        }
    }

    fn pwsh() -> PowerShellSettings {
        PowerShellSettings {
            path: PathBuf::from("pwsh.exe"),
        }
    }

    #[test]
    fn git_bash_found_starts_as_before() {
        for powershell in [None, Some(pwsh())] {
            let chosen = choose_shells(Ok(git()), powershell.clone(), None).unwrap();
            assert_eq!(
                chosen,
                Shells {
                    git_bash: Some(git()),
                    powershell
                }
            );
        }
    }

    #[test]
    fn without_git_bash_powershell_starts() {
        let chosen = choose_shells(Err(GitBashError::NotFound), Some(pwsh()), None).unwrap();
        assert_eq!(
            chosen,
            Shells {
                git_bash: None,
                powershell: Some(pwsh())
            }
        );
    }

    #[test]
    fn a_bad_git_bash_override_is_an_error_even_with_powershell() {
        let bad = GitBashError::BadOverride(PathBuf::from("/x/bash.exe"));
        let message = choose_shells(Err(bad), Some(pwsh()), None).unwrap_err();
        assert!(message.contains("FONTAINE_GIT_BASH_PATH"), "{message}");
        assert!(message.contains("/x/bash.exe"), "{message}");
    }

    #[test]
    fn no_shell_at_all_says_what_to_install_or_set() {
        let message = choose_shells(Err(GitBashError::NotFound), None, None).unwrap_err();
        assert_eq!(
            message,
            "no shell found: install Git for Windows, or set FONTAINE_GIT_BASH_PATH or FONTAINE_POWERSHELL_PATH"
        );
    }

    #[test]
    fn a_bad_powershell_override_is_named_instead_of_suggested() {
        let message = choose_shells(
            Err(GitBashError::NotFound),
            None,
            Some(OsString::from("C:/nope/pwsh.exe")),
        )
        .unwrap_err();
        assert_eq!(
            message,
            "no shell found: install Git for Windows, or set FONTAINE_GIT_BASH_PATH; FONTAINE_POWERSHELL_PATH=C:/nope/pwsh.exe is not a file"
        );
    }

    #[test]
    fn afk_starts_in_auto_and_the_surface_picks_the_default_mode() {
        let config = Config::defaults(PathBuf::from("."));
        assert_eq!(
            requested_mode(Some(PermissionMode::Afk), &config, true),
            PermissionMode::Afk
        );
        assert_eq!(session_mode(PermissionMode::Afk), PermissionMode::Auto);
        assert_eq!(session_mode(PermissionMode::Manual), PermissionMode::Manual);
        assert_eq!(requested_mode(None, &config, true), PermissionMode::Manual);
        assert_eq!(requested_mode(None, &config, false), PermissionMode::Auto);
    }
}
