//! Plugin registry operations as library functions: install, update, uninstall, enable and
//! disable, plus the staged-revision swap. They return structured results and never print, so the
//! `fontaine plugin` commands and the in-app `/plugin` command share one code path. Git is shelled
//! out to with fixed argv and no shell.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use crate::config::HandlerKind;
use crate::extensions;
use crate::file_lock;
use crate::plugins::{
    self, Catalog, CatalogPlugin, CatalogSource, InstalledPlugin, MarketplaceEntry, PluginContext,
    PluginError, Registry, checkout_dir, marketplace_dir, registry_path, staged_dir,
    valid_plugin_name,
};
use fontaine_tools::error_chain::chain;

/// A failed operation. The message names what was attempted; the cause, when there is one, is the
/// `source()` and is rendered once with `error_chain::chain` where the error reaches a human.
#[derive(Debug)]
pub struct OpError {
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl OpError {
    pub fn msg(message: impl Into<String>) -> OpError {
        OpError {
            message: message.into(),
            source: None,
        }
    }

    pub fn caused(
        message: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> OpError {
        OpError {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for OpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn Error + 'static))
    }
}

impl From<PluginError> for OpError {
    fn from(e: PluginError) -> OpError {
        OpError::msg(e.to_string())
    }
}

/// The CLI reports errors as one rendered line.
impl From<OpError> for String {
    fn from(e: OpError) -> String {
        chain(&e)
    }
}

pub type Result<T> = std::result::Result<T, OpError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(OpError::msg(message))
}

fn io_error(what: &str, path: &Path, e: std::io::Error) -> OpError {
    OpError::caused(format!("{what} {}", path.display()), e)
}

pub fn lock_registry(config_dir: &Path) -> Result<file_lock::FileLock> {
    file_lock::lock(&registry_path(config_dir))
        .map_err(|e| io_error("cannot lock", &registry_path(config_dir), e))
}

/// Extra environment for a git child.
pub type GitEnv = &'static [(&'static str, &'static str)];

/// The CLI runs on the user's terminal, where git may prompt for credentials.
pub const INTERACTIVE_GIT: GitEnv = &[];

/// The background pass and the interface share the terminal, so git must fail rather than prompt
/// on it.
pub const UNATTENDED_GIT: GitEnv = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitMode {
    Interactive,
    Unattended,
}

impl GitMode {
    pub fn env(self) -> GitEnv {
        match self {
            GitMode::Interactive => INTERACTIVE_GIT,
            GitMode::Unattended => UNATTENDED_GIT,
        }
    }
}

/// The environment lookup the plugin builders expand `${VAR}` against.
pub type EnvRef<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// What the fetch and validation helpers need besides the registry entry, so they run from the
/// CLI, the interface and the background thread alike.
#[derive(Clone, Copy)]
pub struct Fetch<'a> {
    pub config_dir: &'a Path,
    pub env: EnvRef<'a>,
    pub git: GitEnv,
}

impl<'a> Fetch<'a> {
    pub fn new(config_dir: &'a Path, env: EnvRef<'a>, mode: GitMode) -> Fetch<'a> {
        Fetch {
            config_dir,
            env,
            git: mode.env(),
        }
    }
}

/// A git command with fixed argv, no shell. Its trimmed stdout is the result.
pub fn git(cwd: &Path, args: &[&str], env: GitEnv) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|e| OpError::caused("cannot run git", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return fail(format!("git {} failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn require_git() -> Result<()> {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map_err(|_| OpError::msg("git is not on PATH: a git-hosted marketplace needs it"))
        .map(|_| ())
}

/// Which kind of scratch directory a fetch works in. The CLI sweeps stale `.tmp-*` siblings
/// before adding its own; the background pass uses a distinct prefix so that sweep never removes
/// a directory another process is still filling.
#[derive(Clone, Copy)]
pub enum Workdir {
    Cli,
    Auto,
}

pub const AUTO_PREFIX: &str = ".auto-";

pub fn workdir(parent: &Path, kind: Workdir) -> Result<PathBuf> {
    std::fs::create_dir_all(parent).map_err(|e| io_error("cannot create", parent, e))?;
    let prefix = match kind {
        Workdir::Cli => {
            // A killed run leaves its temp directory behind; sweep the stale ones before adding
            // ours.
            for entry in std::fs::read_dir(parent)
                .map_err(|e| io_error("cannot read", parent, e))?
                .flatten()
            {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".tmp-")
                    && entry.file_type().is_ok_and(|t| t.is_dir())
                {
                    remove_dir(&entry.path());
                }
            }
            ".tmp-"
        }
        Workdir::Auto => AUTO_PREFIX,
    };
    let dir = parent.join(format!("{prefix}{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).map_err(|e| io_error("cannot create", &dir, e))?;
    Ok(dir)
}

pub fn marketplaces_parent(config_dir: &Path) -> PathBuf {
    config_dir
        .join(plugins::PLUGINS_DIR)
        .join(plugins::MARKETPLACES_DIR)
}

pub fn installed_parent(config_dir: &Path, marketplace: &str) -> PathBuf {
    config_dir
        .join(plugins::PLUGINS_DIR)
        .join(plugins::INSTALLED_DIR)
        .join(marketplace)
}

/// Best effort: a leftover directory only wastes disk, and the sweeps retry it.
pub fn remove_dir(path: &Path) {
    let _ = crate::owner_only::remove_dir_all_retrying(path);
}

/// Replaces `target` with `temp` atomically enough: the old checkout is kept as a backup until
/// the rename into place has succeeded.
pub fn replace_dir(temp: &Path, target: &Path) -> Result<()> {
    let backup = target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".{}.bak-{}",
            target.file_name().unwrap_or_default().to_string_lossy(),
            Uuid::new_v4()
        ));
    let had = target.exists();
    if had {
        std::fs::rename(target, &backup).map_err(|e| io_error("cannot move aside", target, e))?;
    }
    if let Err(e) = std::fs::rename(temp, target) {
        if had {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(io_error("cannot move into place", target, e));
    }
    if had {
        remove_dir(&backup);
    }
    Ok(())
}

pub fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(|e| io_error("cannot create", dest, e))?;
    let entries = std::fs::read_dir(source).map_err(|e| io_error("cannot read", source, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let target = dest.join(&name);
        let kind = entry
            .file_type()
            .map_err(|e| io_error("cannot stat", &path, e))?;
        if kind.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| io_error("cannot copy", &path, e))?;
        }
    }
    Ok(())
}

pub fn read_catalog(checkout: &Path) -> Result<Catalog> {
    let path = checkout.join(plugins::MARKETPLACE_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| io_error("cannot read", &path, e))?;
    Ok(plugins::parse_catalog(&text, &path)?)
}

pub fn clone_git(url: &str, dest: &Path, env: GitEnv) -> Result<()> {
    require_git()?;
    let dest = dest.to_string_lossy().into_owned();
    git(Path::new("."), &["clone", "--quiet", url, &dest], env)?;
    Ok(())
}

/// Checks out the entry's `sha` or `ref`, or leaves the clone at its current HEAD.
pub fn pin_clone(dest: &Path, source: &CatalogSource, env: GitEnv) -> Result<String> {
    match source {
        CatalogSource::Github { sha: Some(sha), .. }
        | CatalogSource::Url { sha: Some(sha), .. } => {
            let _ = git(dest, &["fetch", "--quiet", "origin"], env);
            git(dest, &["checkout", "--quiet", "--detach", sha], env)?;
        }
        CatalogSource::Github { r#ref: Some(r), .. }
        | CatalogSource::Url { r#ref: Some(r), .. } => {
            git(dest, &["fetch", "--quiet", "origin", r], env)?;
            git(
                dest,
                &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
                env,
            )?;
        }
        _ => {}
    }
    git(dest, &["rev-parse", "HEAD"], env)
}

pub fn clone_url_of(source: &CatalogSource) -> String {
    match source {
        CatalogSource::Github { repo, .. } => format!("https://github.com/{repo}.git"),
        CatalogSource::Url { url, .. } => url.clone(),
        CatalogSource::Path(_) => unreachable!("a path source is copied, not cloned"),
    }
}

fn plugin_context<'a>(env: EnvRef<'a>, project_root: &'a Path) -> PluginContext<'a> {
    PluginContext { project_root, env }
}

/// The revision the remote's HEAD names, without cloning.
pub fn ls_remote_head(url: &str, env: GitEnv) -> Option<String> {
    let output = git(Path::new("."), &["ls-remote", "--symref", url, "HEAD"], env).ok()?;
    head_sha(&output)
}

/// The sha on the `HEAD` line of `git ls-remote --symref` output, which prints the symref
/// resolution (`ref: refs/heads/main`) first and the sha on its own line.
pub fn head_sha(output: &str) -> Option<String> {
    for line in output.lines() {
        let mut fields = line.split('\t');
        let sha = fields.next().unwrap_or("");
        if fields.next() == Some("HEAD") && !sha.starts_with("ref:") && !sha.is_empty() {
            return Some(sha.to_string());
        }
    }
    None
}

/// The commit a branch or tag named `name` points at on the remote, without cloning. An
/// annotated tag resolves to the commit it peels to, as a checkout of it would.
pub fn ls_remote_ref(url: &str, name: &str, env: GitEnv) -> Option<String> {
    let output = git(Path::new("."), &["ls-remote", url, name], env).ok()?;
    let lines: Vec<(&str, &str)> = output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let find = |wanted: String| {
        lines
            .iter()
            .find(|(_, r)| *r == wanted)
            .map(|(sha, _)| sha.to_string())
    };
    find(format!("refs/tags/{name}^{{}}"))
        .or_else(|| find(format!("refs/heads/{name}")))
        .or_else(|| find(format!("refs/tags/{name}")))
        .or_else(|| find(name.to_string()))
}

/// The remote default-branch HEAD of a clone.
pub fn remote_head(checkout: &Path, env: GitEnv) -> Result<String> {
    git(checkout, &["fetch", "--quiet", "origin"], env)?;
    // Ask the remote rather than the local `origin/HEAD`, whose symref stays on the default
    // branch the clone first saw.
    if let Ok(output) = git(checkout, &["ls-remote", "--symref", "origin", "HEAD"], env)
        && let Some(sha) = head_sha(&output)
    {
        return Ok(sha);
    }
    match git(checkout, &["rev-parse", "origin/HEAD"], env) {
        Ok(sha) if !sha.is_empty() => Ok(sha),
        _ => fail("could not resolve the remote's default branch"),
    }
}

/// The marketplace checkout a plugin's catalog entry is read from, and the pin a path source
/// shares.
pub struct PluginSource<'a> {
    pub market: &'a MarketplaceEntry,
    pub checkout: &'a Path,
    pub pin: &'a str,
}

/// A plugin revision copied or cloned into a scratch directory and validated.
pub struct Materialized {
    pub temp: PathBuf,
    pub pin: String,
    pub build: plugins::PluginBuild,
    /// Load-time warnings, for the caller to show where it can.
    pub warnings: Vec<String>,
}

/// Copies or clones the entry into `temp`, validates everything and returns the pin and the
/// validated build. Nothing outside `temp` is touched.
pub fn materialize(
    fx: &Fetch<'_>,
    source: &PluginSource<'_>,
    entry: &CatalogPlugin,
    temp: &Path,
) -> Result<Materialized> {
    let pin = match &entry.source {
        CatalogSource::Path(rel) => {
            let from = source.checkout.join(rel);
            let from = from
                .canonicalize()
                .map_err(|e| io_error("cannot resolve", &from, e))?;
            // Compare canonical to canonical: the config dir itself may sit behind a
            // symlink, and the resolved source never carries one.
            let market_root = source
                .checkout
                .canonicalize()
                .unwrap_or_else(|_| source.checkout.to_path_buf());
            if !from.starts_with(&market_root) {
                return fail(format!(
                    "the source of `{}` points outside its marketplace",
                    entry.name
                ));
            }
            copy_tree(&from, temp)?;
            source.pin.to_string()
        }
        CatalogSource::Github { .. } | CatalogSource::Url { .. } => {
            clone_git(&clone_url_of(&entry.source), temp, fx.git)?;
            pin_clone(temp, &entry.source, fx.git)?
        }
    };
    let ctx = plugin_context(fx.env, fx.config_dir);
    let build = plugins::build_plugin(temp, &entry.name, &ctx)?;
    let warnings = validate_components(temp, &build)?;
    Ok(Materialized {
        temp: temp.to_path_buf(),
        pin,
        build,
        warnings,
    })
}

/// A plugin revision whose pin is known without cloning, when it can be: the catalog's `sha`,
/// the marketplace's pin for a path source, or what the remote's branch or HEAD names.
pub fn would_pin(source: &PluginSource<'_>, entry: &CatalogPlugin, env: GitEnv) -> Option<String> {
    match &entry.source {
        CatalogSource::Path(_) => Some(source.pin.to_string()),
        CatalogSource::Github { sha: Some(sha), .. }
        | CatalogSource::Url { sha: Some(sha), .. } => Some(sha.clone()),
        CatalogSource::Github { r#ref: Some(r), .. }
        | CatalogSource::Url { r#ref: Some(r), .. } => {
            ls_remote_ref(&clone_url_of(&entry.source), r, env)
        }
        CatalogSource::Github { .. } | CatalogSource::Url { .. } => {
            ls_remote_head(&clone_url_of(&entry.source), env)
        }
    }
}

/// Whether `candidate` (possibly an abbreviated catalog sha) names the same revision as `pin`.
pub fn same_pin(pin: &str, candidate: &str) -> bool {
    pin == candidate || (!candidate.is_empty() && pin.starts_with(candidate))
}

/// What a fetch of a plugin found.
pub enum PluginFetch {
    UpToDate,
    Ready(Box<Materialized>),
}

/// Materializes the plugin's upstream revision unless it is already one of the `known` pins.
/// The remote is compared with them before anything is cloned, and the materialized pin after.
pub fn fetch_plugin(
    fx: &Fetch<'_>,
    source: &PluginSource<'_>,
    entry: &CatalogPlugin,
    kind: Workdir,
    known: &[&str],
) -> Result<PluginFetch> {
    let is_known = |pin: &str| known.iter().any(|k| same_pin(k, pin));
    if !known.is_empty() && would_pin(source, entry, fx.git).is_some_and(|pin| is_known(&pin)) {
        return Ok(PluginFetch::UpToDate);
    }
    let temp = workdir(&installed_parent(fx.config_dir, &source.market.name), kind)?;
    match materialize(fx, source, entry, &temp) {
        Ok(m) if is_known(&m.pin) => {
            remove_dir(&temp);
            Ok(PluginFetch::UpToDate)
        }
        Ok(m) => Ok(PluginFetch::Ready(Box::new(m))),
        Err(e) => {
            remove_dir(&temp);
            Err(e)
        }
    }
}

/// Loads the plugin's extension files the way a session will, so install catches a bad name,
/// model, mode or duplicate component before the registry records it. Returns the warnings.
pub fn validate_components(root: &Path, build: &plugins::PluginBuild) -> Result<Vec<String>> {
    let servers: Vec<String> = build.dir.mcp_names.values().cloned().collect();
    let mut warnings = Vec::new();
    // Validate only the plugin's own files: the user and project scopes read `config_dir` and
    // `project_root`, so point them at a path that holds no extension files.
    let none = root.join(".fontaine-plugin-validation");
    extensions::load(&mut extensions::LoadParams {
        config_dir: &none,
        project_root: &none,
        servers: &servers,
        plugins: std::slice::from_ref(&build.dir),
        warnings: &mut warnings,
    })
    .map_err(|e| OpError::msg(e.to_string()))?;
    Ok(warnings)
}

/// A build of the plugin whose `${VAR}` references stay as written, so the approval summary
/// shows what a hook or server will read without printing the value of a secret.
pub fn unexpanded_build(root: &Path, name: &str) -> Option<plugins::PluginBuild> {
    let env = |var: &str| Some(OsString::from(format!("${{{var}}}")));
    let ctx = PluginContext {
        project_root: Path::new("${CLAUDE_PROJECT_DIR}"),
        env: &env,
    };
    plugins::build_plugin(root, name, &ctx).ok()
}

fn with_root_placeholder(text: &str, root: &Path) -> String {
    text.replace(&root.display().to_string(), "${CLAUDE_PLUGIN_ROOT}")
}

fn words(command: &str, args: &Option<Vec<String>>) -> String {
    std::iter::once(command)
        .chain(args.iter().flatten().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn env_pairs(env: &Option<std::collections::BTreeMap<String, String>>) -> String {
    match env {
        Some(env) if !env.is_empty() => format!(
            " (env {})",
            env.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => String::new(),
    }
}

/// The lines of the approval summary: counts, then every hook command, MCP and LSP server
/// definition and skill `!` line the plugin would run, then what the approval fingerprint
/// covers. It shows the revision's whole surface, not a difference from what was approved
/// before: the registry stores only a hash.
pub fn summary_lines(
    entry: &CatalogPlugin,
    build: &plugins::PluginBuild,
    root: &Path,
) -> Vec<String> {
    let s = build.summary;
    let mut lines = vec![format!(
        "plugin `{}` brings {} skill(s), {} agent(s), {} style(s), {} hook(s), {} server(s), {} deny rule(s), {} ask rule(s), {} hard_deny and {} soft_deny guardian entries",
        entry.name,
        s.skills,
        s.agents,
        s.styles,
        s.hooks,
        s.servers,
        s.deny,
        s.ask,
        s.hard_deny,
        s.soft_deny
    )];
    match unexpanded_build(root, &entry.name) {
        Some(shown) => {
            for (event, hooks) in shown.hooks.iter() {
                let event = event.wire_name();
                for hook in hooks {
                    let matcher = hook
                        .matcher
                        .as_deref()
                        .map(|m| format!(" [{m}]"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  hook {event}{matcher}: {}{}",
                        match hook.kind {
                            HandlerKind::Command => String::new(),
                            other => format!("({}) ", other.as_str()),
                        },
                        with_root_placeholder(hook.target(), root)
                    ));
                }
            }
            for (name, server) in &shown.servers {
                let definition = match (&server.command, &server.url) {
                    (Some(command), _) => {
                        format!("{}{}", words(command, &server.args), env_pairs(&server.env))
                    }
                    (None, Some(url)) => url.clone(),
                    (None, None) => String::new(),
                };
                lines.push(format!(
                    "  MCP server {name}: {}",
                    with_root_placeholder(&definition, root)
                ));
            }
            // A language server is a program that runs on every edit, so the approval names each.
            for (name, server) in &shown.lsp_servers {
                lines.push(format!(
                    "  LSP server {name}: {}{}",
                    with_root_placeholder(&words(&server.command, &server.args), root),
                    with_root_placeholder(&env_pairs(&server.env), root)
                ));
            }
            match plugins::skill_commands(&shown.dir) {
                Ok(commands) => {
                    for (file, command) in commands {
                        lines.push(format!("  skill command in {file}: {command}"));
                    }
                }
                Err(e) => lines.push(format!("  skill commands could not be listed: {e}")),
            }
        }
        None => {
            for name in build.dir.mcp_names.values() {
                lines.push(format!("  MCP server {name}"));
            }
            for (name, server) in &build.lsp_servers {
                lines.push(format!("  LSP server {name}: {}", server.command));
            }
        }
    }
    if !build.env_vars.is_empty() {
        // Name the variables, never their values: a reference moves the variable's value into
        // a hook command or a server env/URL, and the install approves that.
        lines.push(format!(
            "  reads environment variables: {}",
            build
                .env_vars
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.push(
        "  approving this records a fingerprint of the hook, MCP and LSP definitions, skill `!` commands and the content of every non-markdown file; it lists this revision's full surface, not a difference from what you approved before".to_string(),
    );
    lines
}

/// The fingerprint of the executable surface the user is about to approve. An entry without one
/// is never auto-updated, so a failure to compute it costs the convenience, not safety; the
/// warning says so.
pub struct Approval {
    pub fingerprint: Option<String>,
    pub warning: Option<String>,
}

pub fn approval_of(root: &Path, name: &str) -> Approval {
    match plugins::approval_fingerprint(root) {
        Ok(fingerprint) => Approval {
            fingerprint: Some(fingerprint),
            warning: None,
        },
        Err(e) => Approval {
            fingerprint: None,
            warning: Some(format!(
                "plugin `{name}` will not auto-update: cannot fingerprint it: {e}"
            )),
        },
    }
}

/// Scratch directory that is removed unless a commit moved it into place.
struct Scratch {
    path: PathBuf,
    armed: bool,
}

impl Scratch {
    fn new(path: PathBuf) -> Scratch {
        Scratch { path, armed: true }
    }

    /// The directory was renamed away; there is nothing left to remove.
    fn moved(&mut self) {
        self.armed = false;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.armed {
            remove_dir(&self.path);
        }
    }
}

/// An install materialized and validated in a scratch directory, with nothing recorded yet. The
/// caller shows the summary, then commits or drops it (dropping removes the scratch directory).
pub struct PreparedInstall {
    pub name: String,
    pub marketplace: String,
    pub entry: CatalogPlugin,
    pub build: plugins::PluginBuild,
    pub pin: String,
    /// Load-time warnings from validating the plugin.
    pub warnings: Vec<String>,
    /// The warning when the approval fingerprint could not be computed.
    pub fingerprint_warning: Option<String>,
    pub fingerprint: Option<String>,
    /// The full executable surface, for the approval.
    pub summary: Vec<String>,
    scratch: Scratch,
}

/// What a committed install recorded.
pub struct Installed {
    pub name: String,
    pub pin: String,
    pub marketplace: String,
    pub fingerprint: Option<String>,
}

/// Fetches and validates `<name>@<marketplace>` into a scratch directory. The registry is only
/// read (under its lock, released on return). `kind` picks the scratch directory's prefix: the
/// CLI's sweeps leftovers of killed runs, which would take a directory another prepared install
/// is still holding, so a session uses the prefix that is only swept when old.
pub fn prepare_install(fx: &Fetch<'_>, spec: &str, kind: Workdir) -> Result<PreparedInstall> {
    let (name, marketplace) = spec
        .split_once('@')
        .filter(|(n, m)| !n.is_empty() && !m.is_empty())
        .ok_or_else(|| OpError::msg("install takes `<name>@<marketplace>`"))?;
    if !valid_plugin_name(name) {
        return fail(format!("`{name}` is not a plugin name"));
    }
    let (market, entry, market_checkout) = {
        let _lock = lock_registry(fx.config_dir)?;
        let registry = plugins::read_registry(fx.config_dir)?;
        let market = registry
            .marketplace
            .iter()
            .find(|m| m.name == marketplace)
            .ok_or_else(|| {
                OpError::msg(format!(
                    "no marketplace named `{marketplace}` is registered"
                ))
            })?
            .clone();
        if registry.plugin.iter().any(|p| p.name == name) {
            return fail(format!(
                "a plugin named `{name}` is already installed; plugin names are unique across marketplaces"
            ));
        }
        let market_checkout = marketplace_dir(fx.config_dir, &market.name);
        let catalog = read_catalog(&market_checkout)?;
        let entry = catalog
            .plugins
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| {
                OpError::msg(format!(
                    "the catalog of `{marketplace}` has no plugin named `{name}`"
                ))
            })?
            .clone();
        (market, entry, market_checkout)
    };

    let temp = workdir(&installed_parent(fx.config_dir, &market.name), kind)?;
    let scratch = Scratch::new(temp.clone());
    let source = PluginSource {
        market: &market,
        checkout: &market_checkout,
        pin: &market.pin,
    };
    let materialized = materialize(fx, &source, &entry, &temp)?;
    let approval = approval_of(&temp, name);
    let summary = summary_lines(&entry, &materialized.build, &temp);
    Ok(PreparedInstall {
        name: name.to_string(),
        marketplace: market.name.clone(),
        entry,
        build: materialized.build,
        pin: materialized.pin,
        warnings: materialized.warnings,
        fingerprint_warning: approval.warning,
        fingerprint: approval.fingerprint,
        summary,
        scratch,
    })
}

/// Records the prepared install and moves its checkout into place. The name must still be free
/// and the marketplace still registered, or the install fails.
pub fn commit_install(fx: &Fetch<'_>, mut prepared: PreparedInstall) -> Result<Installed> {
    let _lock = lock_registry(fx.config_dir)?;
    let mut registry = plugins::read_registry(fx.config_dir)?;
    if registry.plugin.iter().any(|p| p.name == prepared.name) {
        return fail(format!(
            "a plugin named `{}` was installed since it was fetched",
            prepared.name
        ));
    }
    if !registry
        .marketplace
        .iter()
        .any(|m| m.name == prepared.marketplace)
    {
        return fail(format!(
            "the marketplace `{}` was removed since the plugin was fetched",
            prepared.marketplace
        ));
    }
    // Record the install before moving the checkout in: a recorded plugin whose checkout is
    // missing degrades to a warning, while a checkout with no registry entry is an orphan.
    let before = registry.clone();
    registry.plugin.push(InstalledPlugin {
        name: prepared.name.clone(),
        marketplace: prepared.marketplace.clone(),
        source: prepared.entry.source.display(),
        pin: prepared.pin.clone(),
        enabled: true,
        approved: prepared.fingerprint.clone(),
        ..Default::default()
    });
    plugins::write_registry(fx.config_dir, &registry)?;
    let target = checkout_dir(fx.config_dir, &prepared.marketplace, &prepared.name);
    if let Err(e) = replace_dir(&prepared.scratch.path, &target) {
        let _ = plugins::write_registry(fx.config_dir, &before);
        return Err(e);
    }
    prepared.scratch.moved();
    Ok(Installed {
        name: prepared.name.clone(),
        pin: prepared.pin.clone(),
        marketplace: prepared.marketplace.clone(),
        fingerprint: prepared.fingerprint.clone(),
    })
}

/// What removing a plugin took out.
pub struct Uninstalled {
    pub name: String,
    /// Checkout directories that could not be removed (a running process holding it on
    /// Windows); the registry entry is gone regardless.
    pub warnings: Vec<String>,
}

pub fn uninstall(config_dir: &Path, name: &str) -> Result<Uninstalled> {
    let _lock = lock_registry(config_dir)?;
    let mut registry = plugins::read_registry(config_dir)?;
    let at = registry
        .plugin
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| OpError::msg(format!("no plugin named `{name}` is installed")))?;
    let entry = registry.plugin.remove(at);
    let checkout = checkout_dir(config_dir, &entry.marketplace, name);
    let mut warnings = Vec::new();
    for dir in [checkout.clone(), staged_dir(&checkout)] {
        if dir.exists() && crate::owner_only::remove_dir_all_retrying(&dir).is_err() {
            warnings.push(format!(
                "could not remove {}; delete it by hand",
                dir.display()
            ));
        }
    }
    plugins::write_registry(config_dir, &registry)?;
    Ok(Uninstalled {
        name: name.to_string(),
        warnings,
    })
}

/// How one plugin update came out.
pub enum PluginOutcome {
    /// The new pin equals the installed one (for a `sha`-pinned plugin, the catalog's sha does).
    UpToDate {
        pin: String,
    },
    Moved {
        old: String,
        new: String,
    },
    /// A newer revision exists but changes what the user approved, so the old one stays live.
    Held {
        new: String,
        reason: String,
    },
    Failed(String),
}

/// An update materialized and validated in a scratch directory, nothing recorded yet.
pub struct PreparedUpdate {
    pub name: String,
    pub marketplace: String,
    pub entry: CatalogPlugin,
    pub build: plugins::PluginBuild,
    pub old_pin: String,
    pub new_pin: String,
    pub warnings: Vec<String>,
    pub fingerprint_warning: Option<String>,
    pub fingerprint: Option<String>,
    /// What the user approved before; `None` for a plugin installed by an older build.
    pub recorded: Option<String>,
    pub summary: Vec<String>,
    scratch: Scratch,
}

impl PreparedUpdate {
    /// Whether the executable surface may differ from what the user approved: no recorded
    /// approval, no computable fingerprint, or a different one.
    pub fn needs_confirmation(&self) -> bool {
        match (&self.recorded, &self.fingerprint) {
            (Some(recorded), Some(new)) => recorded != new,
            _ => true,
        }
    }
}

/// What a committed update recorded.
pub struct Updated {
    pub name: String,
    pub old_pin: String,
    pub new_pin: String,
}

/// Re-materializes the installed plugin from its marketplace into a scratch directory. The
/// revision is applied by `commit_update` whether or not the pin moved: a plugin installed by an
/// older build gets its fingerprint recorded this way.
pub fn prepare_update(fx: &Fetch<'_>, name: &str, kind: Workdir) -> Result<PreparedUpdate> {
    let (plugin, market, entry, market_checkout) = {
        let _lock = lock_registry(fx.config_dir)?;
        let registry = plugins::read_registry(fx.config_dir)?;
        let plugin = registry
            .plugin
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| OpError::msg(format!("no plugin named `{name}` is installed")))?
            .clone();
        let market = registry
            .marketplace
            .iter()
            .find(|m| m.name == plugin.marketplace)
            .ok_or_else(|| {
                OpError::msg(format!(
                    "the marketplace `{}` of `{name}` is not registered",
                    plugin.marketplace
                ))
            })?
            .clone();
        let market_checkout = marketplace_dir(fx.config_dir, &market.name);
        let catalog = read_catalog(&market_checkout)?;
        let entry = catalog
            .plugins
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| {
                OpError::msg(format!(
                    "the catalog of `{}` no longer has a plugin named `{name}`",
                    market.name
                ))
            })?
            .clone();
        (plugin, market, entry, market_checkout)
    };

    let source = PluginSource {
        market: &market,
        checkout: &market_checkout,
        pin: &market.pin,
    };
    let fetched = match fetch_plugin(fx, &source, &entry, kind, &[]) {
        Ok(PluginFetch::Ready(m)) => *m,
        Ok(PluginFetch::UpToDate) => unreachable!("no known pins were passed"),
        Err(e) => {
            return Err(OpError::caused(
                format!(
                    "the new revision of `{name}` did not validate; the old checkout stays live"
                ),
                e,
            ));
        }
    };
    let scratch = Scratch::new(fetched.temp.clone());
    let approval = approval_of(&fetched.temp, name);
    let summary = summary_lines(&entry, &fetched.build, &fetched.temp);
    Ok(PreparedUpdate {
        name: name.to_string(),
        marketplace: market.name.clone(),
        entry,
        build: fetched.build,
        old_pin: plugin.pin,
        new_pin: fetched.pin,
        warnings: fetched.warnings,
        fingerprint_warning: approval.warning,
        fingerprint: approval.fingerprint,
        recorded: plugin.approved,
        summary,
        scratch,
    })
}

/// Records the move and replaces the checkout. The entry and its pin must be unchanged since
/// `prepare_update`. Any staged or held revision is discarded. A failed replacement puts the
/// old registry back (and `replace_dir` restores the old checkout itself).
pub fn commit_update(fx: &Fetch<'_>, mut prepared: PreparedUpdate) -> Result<Updated> {
    let _lock = lock_registry(fx.config_dir)?;
    let mut registry = plugins::read_registry(fx.config_dir)?;
    let at = registry
        .plugin
        .iter()
        .position(|p| p.name == prepared.name && p.pin == prepared.old_pin)
        .ok_or_else(|| {
            OpError::msg(format!(
                "plugin `{}` changed (or was removed) since the update was fetched; run the update again",
                prepared.name
            ))
        })?;
    let before = registry.clone();
    let record = &mut registry.plugin[at];
    record.pin = prepared.new_pin.clone();
    record.source = prepared.entry.source.display();
    record.approved = prepared.fingerprint.clone();
    record.held_pin = None;
    record.staged_pin = None;
    plugins::write_registry(fx.config_dir, &registry)?;
    let target = checkout_dir(fx.config_dir, &prepared.marketplace, &prepared.name);
    if let Err(e) = replace_dir(&prepared.scratch.path, &target) {
        let _ = plugins::write_registry(fx.config_dir, &before);
        return Err(e);
    }
    prepared.scratch.moved();
    remove_dir(&staged_dir(&target));
    Ok(Updated {
        name: prepared.name.clone(),
        old_pin: prepared.old_pin.clone(),
        new_pin: prepared.new_pin.clone(),
    })
}

pub fn set_enabled(config_dir: &Path, name: &str, enabled: bool) -> Result<()> {
    let _lock = lock_registry(config_dir)?;
    let mut registry = plugins::read_registry(config_dir)?;
    let at = registry
        .plugin
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| OpError::msg(format!("no plugin named `{name}` is installed")))?;
    registry.plugin[at].enabled = enabled;
    plugins::write_registry(config_dir, &registry)?;
    Ok(())
}

/// How long a leftover backup, scratch or staged directory may sit before it is removed.
const LEFTOVER_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What `apply_staged` did, for the caller to report and to reload.
#[derive(Debug, Default)]
pub struct StagedApply {
    /// Warnings for the swaps that could not be applied; those stay staged and are retried.
    pub warnings: Vec<String>,
    /// Plugins whose checkout was swapped.
    pub swapped: Vec<String>,
    /// Plugins whose swap failed after the gate let it through: their old checkout is kept.
    pub failed: Vec<String>,
    /// Plugins with a newer revision held for review because its executable surface differs.
    pub held: Vec<String>,
}

/// The plugins `apply_staged` would swap now: a staged revision that is still on disk, for an
/// entry that has not opted out of auto-update. A live session stops these plugins' processes
/// first, then applies with a gate that lets only those through, so a revision another process
/// staged in between waits for the next pass.
pub fn staged_plugins(config_dir: &Path) -> Vec<String> {
    let Ok(registry) = plugins::read_registry(config_dir) else {
        return Vec::new();
    };
    registry
        .plugin
        .iter()
        .filter(|p| {
            p.staged_pin.is_some()
                && p.auto_update != Some(false)
                && staged_dir(&checkout_dir(config_dir, &p.marketplace, &p.name)).is_dir()
        })
        .map(|p| p.name.clone())
        .collect()
}

/// Applies the revisions earlier passes staged. `may_swap` is asked about each plugin just before
/// its checkout is replaced, after the auto-update and lock checks; `false` leaves the revision
/// staged. Entries that opted out of auto-update keep their staged revision untouched.
pub fn apply_staged(
    config_dir: &Path,
    now: SystemTime,
    may_swap: &mut dyn FnMut(&str) -> bool,
) -> StagedApply {
    let mut out = StagedApply::default();
    let Ok(snapshot) = plugins::read_registry(config_dir) else {
        return out;
    };
    out.held = snapshot
        .plugin
        .iter()
        .filter(|p| p.held_pin.is_some())
        .map(|p| p.name.clone())
        .collect();
    let pending = snapshot.marketplace.iter().any(|m| m.staged_pin.is_some())
        || snapshot.plugin.iter().any(|p| p.staged_pin.is_some());
    let missing = snapshot
        .marketplace
        .iter()
        .any(|m| !marketplace_dir(config_dir, &m.name).is_dir())
        || snapshot
            .plugin
            .iter()
            .any(|p| !checkout_dir(config_dir, &p.marketplace, &p.name).is_dir());
    // Reading the registry is all a start with nothing to do costs: no lock, no directory scan.
    if !pending && !missing {
        return out;
    }
    let lock = match file_lock::lock(&registry_path(config_dir)) {
        Ok(lock) => lock,
        Err(e) => {
            out.warnings.push(format!(
                "staged plugin updates were not applied: cannot lock the registry: {}",
                chain(&e)
            ));
            return out;
        }
    };
    let mut registry = match plugins::read_registry(config_dir) {
        Ok(registry) => registry,
        Err(e) => {
            out.warnings.push(e.to_string());
            return out;
        }
    };
    cleanup_leftovers(config_dir, &registry, now);

    for at in 0..registry.marketplace.len() {
        let entry = registry.marketplace[at].clone();
        let Some(staged_pin) = entry.staged_pin.clone() else {
            continue;
        };
        if entry.auto_update == Some(false) {
            continue;
        }
        match swap_in(
            &marketplace_dir(config_dir, &entry.name),
            &entry.name,
            "marketplace",
        ) {
            Swap::Applied => registry.marketplace[at].pin = staged_pin,
            Swap::Gone => {}
            Swap::Failed(warning) => {
                out.warnings.push(warning);
                continue;
            }
        }
        registry.marketplace[at].staged_pin = None;
        write_back(config_dir, &registry, &mut out.warnings);
    }
    for at in 0..registry.plugin.len() {
        let entry = registry.plugin[at].clone();
        let Some(staged_pin) = entry.staged_pin.clone() else {
            continue;
        };
        if entry.auto_update == Some(false) {
            continue;
        }
        let checkout = checkout_dir(config_dir, &entry.marketplace, &entry.name);
        if staged_dir(&checkout).is_dir() && !may_swap(&entry.name) {
            continue;
        }
        match swap_in(&checkout, &entry.name, "plugin") {
            Swap::Applied => {
                registry.plugin[at].pin = staged_pin;
                registry.plugin[at].held_pin = None;
                out.swapped.push(entry.name.clone());
            }
            Swap::Gone => {}
            Swap::Failed(warning) => {
                out.warnings.push(warning);
                out.failed.push(entry.name.clone());
                continue;
            }
        }
        registry.plugin[at].staged_pin = None;
        write_back(config_dir, &registry, &mut out.warnings);
    }
    drop(lock);
    out.held = registry
        .plugin
        .iter()
        .filter(|p| p.held_pin.is_some())
        .map(|p| p.name.clone())
        .collect();
    out
}

fn write_back(config_dir: &Path, registry: &Registry, warnings: &mut Vec<String>) {
    if let Err(e) = plugins::write_registry(config_dir, registry) {
        warnings.push(format!("the plugin registry was not updated: {e}"));
    }
}

enum Swap {
    Applied,
    /// The staged directory is gone, so the recorded revision is stale.
    Gone,
    /// A rename failed (a running server holding the checkout on Windows); the staged revision
    /// stays for the next attempt.
    Failed(String),
}

/// Renames the checkout aside, the staged revision into place and deletes the backup.
fn swap_in(checkout: &Path, name: &str, what: &str) -> Swap {
    let staged = staged_dir(checkout);
    if !staged.is_dir() {
        return Swap::Gone;
    }
    match replace_dir(&staged, checkout) {
        Ok(()) => Swap::Applied,
        Err(e) => Swap::Failed(format!(
            "the staged update of {what} `{name}` was not applied and will be retried at the next start or /reload-plugins: {}",
            chain(&e)
        )),
    }
}

/// Restores a checkout an interrupted swap left only as a backup, removes backups, scratch
/// directories and staged revisions nothing refers to. Best effort: a leftover only wastes disk.
pub fn cleanup_leftovers(config_dir: &Path, registry: &Registry, now: SystemTime) {
    let market_parent = marketplaces_parent(config_dir);
    let staged_markets: Vec<&str> = registry
        .marketplace
        .iter()
        .filter(|m| m.staged_pin.is_some())
        .map(|m| m.name.as_str())
        .collect();
    sweep(&market_parent, &staged_markets, now);
    for market in &registry.marketplace {
        let staged: Vec<&str> = registry
            .plugin
            .iter()
            .filter(|p| p.marketplace == market.name && p.staged_pin.is_some())
            .map(|p| p.name.as_str())
            .collect();
        sweep(&installed_parent(config_dir, &market.name), &staged, now);
    }
}

fn old_enough(path: &Path, now: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| now.duration_since(at).ok())
        .is_some_and(|age| age >= LEFTOVER_AGE)
}

/// One directory of checkouts: `staged` names the entries whose staged revision is recorded.
fn sweep(parent: &Path, staged: &[&str], now: SystemTime) {
    let Ok(read) = std::fs::read_dir(parent) else {
        return;
    };
    let entries: Vec<PathBuf> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    for path in entries {
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(rest) = file.strip_prefix('.')
            && let Some((name, _)) = rest.split_once(".bak-")
        {
            let target = parent.join(name);
            if !target.exists() {
                // An interrupted swap: the old checkout is all there is. The next start redoes
                // the swap from the recorded staged revision.
                let _ = std::fs::rename(&path, &target);
            } else if old_enough(&path, now) {
                remove_dir(&path);
            }
        } else if file.starts_with(AUTO_PREFIX) {
            if old_enough(&path, now) {
                remove_dir(&path);
            }
        } else if let Some(name) = file.strip_prefix(".staged-")
            && !staged.contains(&name)
        {
            remove_dir(&path);
        }
    }
}

#[cfg(test)]
mod tests;
