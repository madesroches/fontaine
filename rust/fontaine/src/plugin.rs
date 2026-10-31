//! `fontaine plugin`: installing and managing plugins and marketplaces. The commands run outside
//! any session, need no permission pipeline, and keep installs out of a model-reachable surface.
//! The only mutation that runs inside a session process is the staged background pass in
//! `plugin_auto_update`, which reuses the fetch and validation helpers here. Git is shelled out
//! to with fixed argv and no shell.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fontaine_core::dirs::{self, Dirs};
use fontaine_core::extensions;
use fontaine_core::notice::Notice;
use fontaine_core::plugins::{
    self, Catalog, CatalogPlugin, CatalogSource, InstalledPlugin, MarketplaceEntry, PluginContext,
    Registry, checkout_dir, content_hash, marketplace_dir, registry_path, staged_dir,
    valid_plugin_name,
};
use uuid::Uuid;

use crate::cli::{
    MarketplaceAction, MarketplaceAddArgs, PluginAction, PluginArgs, ReportFormat, Toggle,
};
use crate::{EXIT_CONFIG, Host, print_warnings};

pub fn run(args: &PluginArgs, host: &Host) -> ExitCode {
    let dirs = match dirs::resolve(
        &*host.env,
        host.default_config_dir.clone(),
        host.default_data_dir.clone(),
    ) {
        Ok(dirs) => dirs,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(e.exit_code());
        }
    };
    let result = match &args.action {
        PluginAction::Marketplace(marketplace) => match &marketplace.action {
            MarketplaceAction::Add(add) => marketplace_add(&dirs, add),
            MarketplaceAction::List => marketplace_list(&dirs),
            MarketplaceAction::Update { name } => marketplace_update(&dirs, name.as_deref()),
            MarketplaceAction::Remove { name } => marketplace_remove(&dirs, name),
            MarketplaceAction::AutoUpdate { name, setting } => {
                set_marketplace_auto_update(&dirs, name, *setting)
            }
        },
        PluginAction::Install(args) => install(&dirs, host, &args.name),
        PluginAction::Uninstall(args) => uninstall(&dirs, &args.name),
        PluginAction::Update(args) => update(&dirs, host, &args.name),
        PluginAction::Enable(args) => set_enabled(&dirs, &args.name, true),
        PluginAction::Disable(args) => set_enabled(&dirs, &args.name, false),
        PluginAction::AutoUpdate(args) => set_plugin_auto_update(&dirs, &args.name, args.setting),
        PluginAction::List(format) => list(&dirs, host, format.format),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("fontaine: {message}");
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

fn fail<T>(message: impl std::fmt::Display) -> Result<T, String> {
    Err(message.to_string())
}

fn lock_registry(config_dir: &Path) -> Result<fontaine_core::file_lock::FileLock, String> {
    fontaine_core::file_lock::lock(&registry_path(config_dir)).map_err(|e| {
        format!(
            "cannot lock {}: {}",
            registry_path(config_dir).display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })
}

fn read_under_lock(config_dir: &Path) -> Result<Registry, String> {
    plugins::read_registry(config_dir).map_err(|e| e.to_string())
}

fn write_under_lock(config_dir: &Path, registry: &Registry) -> Result<(), String> {
    plugins::write_registry(config_dir, registry).map_err(|e| e.to_string())
}

/// Extra environment for a git child.
pub(crate) type GitEnv = &'static [(&'static str, &'static str)];

/// The CLI runs on the user's terminal, where git may prompt for credentials.
pub(crate) const INTERACTIVE_GIT: GitEnv = &[];

/// The background pass shares the terminal with the interface, so git must fail rather than
/// prompt on it.
pub(crate) const UNATTENDED_GIT: GitEnv = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"),
];

/// The environment lookup the plugin builders expand `${VAR}` against.
pub(crate) type EnvRef<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// What the fetch and validation helpers need besides the registry entry, so they run from the
/// CLI (a `Host`) and from the background thread (a shared environment) alike.
#[derive(Clone, Copy)]
pub(crate) struct Fetch<'a> {
    pub config_dir: &'a Path,
    pub env: EnvRef<'a>,
    pub git: GitEnv,
}

/// A git command with fixed argv, no shell. Its trimmed stdout is the result.
fn git(cwd: &Path, args: &[&str], env: GitEnv) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("cannot run git: {}", fontaine_tools::error_chain::chain(&e)))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn require_git() -> Result<(), String> {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map_err(|_| "git is not on PATH: a git-hosted marketplace needs it".to_string())
        .map(|_| ())
}

/// Which kind of scratch directory a fetch works in. The CLI sweeps stale `.tmp-*` siblings
/// before adding its own; the background pass uses a distinct prefix so that sweep never removes
/// a directory another process is still filling.
#[derive(Clone, Copy)]
pub(crate) enum Workdir {
    Cli,
    Auto,
}

pub(crate) fn workdir(parent: &Path, kind: Workdir) -> Result<PathBuf, String> {
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "cannot create {}: {}",
            parent.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    let prefix = match kind {
        Workdir::Cli => {
            // A killed run leaves its temp directory behind; sweep the stale ones before adding
            // ours.
            for entry in std::fs::read_dir(parent)
                .map_err(|e| format!("cannot read {}: {}", parent.display(), e))?
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
    std::fs::create_dir(&dir).map_err(|e| {
        format!(
            "cannot create {}: {}",
            dir.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    Ok(dir)
}

pub(crate) const AUTO_PREFIX: &str = ".auto-";

pub(crate) fn marketplaces_parent(config_dir: &Path) -> PathBuf {
    config_dir
        .join(plugins::PLUGINS_DIR)
        .join(plugins::MARKETPLACES_DIR)
}

pub(crate) fn installed_parent(config_dir: &Path, marketplace: &str) -> PathBuf {
    config_dir
        .join(plugins::PLUGINS_DIR)
        .join(plugins::INSTALLED_DIR)
        .join(marketplace)
}

pub(crate) fn remove_dir(path: &Path) {
    let _ = fontaine_core::owner_only::remove_dir_all_retrying(path);
}

/// Replaces `target` with `temp` atomically enough: the old checkout is kept as a backup until
/// the rename into place has succeeded.
pub(crate) fn replace_dir(temp: &Path, target: &Path) -> Result<(), String> {
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
        std::fs::rename(target, &backup).map_err(|e| {
            format!(
                "cannot move {} aside: {}",
                target.display(),
                fontaine_tools::error_chain::chain(&e)
            )
        })?;
    }
    if let Err(e) = std::fs::rename(temp, target) {
        if had {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(format!(
            "cannot move {} into place: {}",
            target.display(),
            fontaine_tools::error_chain::chain(&e)
        ));
    }
    if had {
        remove_dir(&backup);
    }
    Ok(())
}

pub(crate) fn copy_tree(source: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| {
        format!(
            "cannot create {}: {}",
            dest.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    let entries = std::fs::read_dir(source).map_err(|e| {
        format!(
            "cannot read {}: {}",
            source.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let target = dest.join(&name);
        let kind = entry
            .file_type()
            .map_err(|e| fontaine_tools::error_chain::chain(&e))?;
        if kind.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| {
                format!(
                    "cannot copy {}: {}",
                    path.display(),
                    fontaine_tools::error_chain::chain(&e)
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn read_catalog(checkout: &Path) -> Result<Catalog, String> {
    let path = checkout.join(plugins::MARKETPLACE_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {}: {}",
            path.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    plugins::parse_catalog(&text, &path).map_err(|e| e.to_string())
}

fn clone_git(url: &str, dest: &Path, env: GitEnv) -> Result<(), String> {
    require_git()?;
    let dest = dest.to_string_lossy().into_owned();
    git(Path::new("."), &["clone", "--quiet", url, &dest], env)?;
    Ok(())
}

/// Expands a leading `~` (or `~/`) to the home directory, so a tilde path names the same
/// directory it would in a shell. Git URLs never start with `~`, so other sources are left
/// alone.
fn expand_tilde(source: &str) -> String {
    let Some(home) = ::dirs::home_dir() else {
        return source.to_string();
    };
    if source == "~" {
        return home.to_string_lossy().into_owned();
    }
    if let Some(rest) = source.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().into_owned();
    }
    source.to_string()
}

/// Checks out the entry's `sha` or `ref`, or leaves the clone at its current HEAD.
fn pin_clone(dest: &Path, source: &CatalogSource, env: GitEnv) -> Result<String, String> {
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

fn clone_url_of(source: &CatalogSource) -> String {
    match source {
        CatalogSource::Github { repo, .. } => format!("https://github.com/{repo}.git"),
        CatalogSource::Url { url, .. } => url.clone(),
        CatalogSource::Path(_) => unreachable!("a path source is copied, not cloned"),
    }
}

fn plugin_context<'a>(env: EnvRef<'a>, project_root: &'a Path) -> PluginContext<'a> {
    PluginContext { project_root, env }
}

fn host_fetch<'a>(dirs: &'a Dirs, host: &'a Host) -> Fetch<'a> {
    Fetch {
        config_dir: &dirs.config,
        env: &*host.env,
        git: INTERACTIVE_GIT,
    }
}

pub(crate) fn marketplace_add(dirs: &Dirs, add: &MarketplaceAddArgs) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let source = expand_tilde(&add.source);
    let source_path = Path::new(&source);
    // A source that names a path (not a git URL) must exist, so a typo fails here instead of
    // surfacing as a confusing git error.
    let looks_like_path = source.starts_with('.')
        || source.starts_with('/')
        || (source.len() > 2 && source.as_bytes()[1] == b':') // a Windows drive letter
        || (!source.contains("://") && !source.contains('@') && source.contains('/'));
    if looks_like_path && !source_path.is_dir() {
        return fail(format!("{} is not a directory", source_path.display()));
    }
    let local = source_path.is_dir();
    // A bare relative name carries no remote markers, so it is a path, not a git URL; git
    // reads it the same way, and a typo fails with the same message as an absolute path.
    if !local && !source.contains("://") && !source.contains('@') && !source.contains(':') {
        return fail(format!("{} is not a directory", source_path.display()));
    }
    // A local directory is stored by its resolved absolute path, so a later `marketplace
    // update` finds it from any working directory.
    let stored_source = if local {
        source_path
            .canonicalize()
            .map_err(|e| {
                format!(
                    "cannot resolve {}: {}",
                    source_path.display(),
                    fontaine_tools::error_chain::chain(&e)
                )
            })?
            .to_string_lossy()
            .into_owned()
    } else {
        source.clone()
    };
    let temp = workdir(&marketplaces_parent(&dirs.config), Workdir::Cli)?;
    let cleanup = |temp: &Path| remove_dir(temp);

    if local {
        copy_tree(source_path, &temp).inspect_err(|_e| {
            cleanup(&temp);
        })?;
    } else {
        if let Err(e) = clone_git(&source, &temp, INTERACTIVE_GIT) {
            cleanup(&temp);
            return Err(e);
        }
    }

    let catalog = match read_catalog(&temp) {
        Ok(catalog) => catalog,
        Err(e) => {
            cleanup(&temp);
            return Err(e);
        }
    };
    let key = add.name.clone().unwrap_or_else(|| catalog.name.clone());
    if !valid_plugin_name(&key) {
        cleanup(&temp);
        return fail(format!("`{key}` is not a marketplace name"));
    }
    if registry.marketplace.iter().any(|m| m.name == key) {
        cleanup(&temp);
        return fail(format!("a marketplace named `{key}` is already registered"));
    }
    let pin = if local {
        content_hash(&temp).inspect_err(|_e| {
            cleanup(&temp);
        })?
    } else {
        git(&temp, &["rev-parse", "HEAD"], INTERACTIVE_GIT).inspect_err(|_e| {
            cleanup(&temp);
        })?
    };
    let target = marketplace_dir(&dirs.config, &key);
    if let Err(e) = replace_dir(&temp, &target) {
        cleanup(&temp);
        return Err(e);
    }
    registry.marketplace.push(MarketplaceEntry {
        name: key.clone(),
        url: stored_source,
        pin,
        ..Default::default()
    });
    write_under_lock(&dirs.config, &registry)?;
    println!(
        "registered marketplace `{key}` ({} plugin(s))",
        catalog.plugins.len()
    );
    Ok(())
}

fn marketplace_list(dirs: &Dirs) -> Result<(), String> {
    let registry = plugins::read_registry(&dirs.config).map_err(|e| e.to_string())?;
    if registry.marketplace.is_empty() {
        println!("no marketplaces registered");
        return Ok(());
    }
    for marketplace in &registry.marketplace {
        let installed = registry
            .plugin
            .iter()
            .filter(|p| p.marketplace == marketplace.name)
            .count();
        let marker = if marketplace.auto_update == Some(false) {
            " — auto-update off"
        } else {
            ""
        };
        println!(
            "{} — {} — pin {} — {} plugin(s){marker}",
            marketplace.name,
            marketplace.url,
            plugins::short_pin(&marketplace.pin),
            installed
        );
    }
    Ok(())
}

/// How one marketplace update came out.
pub(crate) enum MarketplaceOutcome {
    /// The new pin equals the installed one (or the revision is already staged).
    UpToDate {
        pin: String,
    },
    Moved {
        old: String,
        new: String,
    },
    Failed(String),
}

/// What a fetch of a marketplace found.
pub(crate) enum MarketplaceFetch {
    UpToDate,
    /// A validated checkout in `temp`, at `new`.
    Ready {
        temp: PathBuf,
        new: String,
    },
}

/// Fetches the marketplace's current upstream revision into a scratch directory and validates
/// its catalog. `known` lists the pins that count as up to date: the remote is compared with
/// them before anything is cloned, and the cloned revision after.
pub(crate) fn fetch_marketplace(
    fx: &Fetch<'_>,
    entry: &MarketplaceEntry,
    kind: Workdir,
    known: &[&str],
) -> Result<MarketplaceFetch, String> {
    let local = Path::new(&entry.url).is_dir();
    let current = if local {
        content_hash(Path::new(&entry.url)).ok()
    } else {
        ls_remote_head(&entry.url, fx.git)
    };
    if current.is_some_and(|pin| known.contains(&pin.as_str())) {
        return Ok(MarketplaceFetch::UpToDate);
    }
    let temp = workdir(&marketplaces_parent(fx.config_dir), kind)?;
    let fetched = if local {
        copy_tree(Path::new(&entry.url), &temp).and_then(|()| content_hash(&temp))
    } else {
        clone_git(&entry.url, &temp, fx.git).and_then(|()| {
            let sha = remote_head(&temp, fx.git)?;
            git(&temp, &["checkout", "--quiet", "--detach", &sha], fx.git)?;
            git(&temp, &["rev-parse", "HEAD"], fx.git)
        })
    };
    let new = match fetched {
        Ok(new) => new,
        Err(e) => {
            remove_dir(&temp);
            return Err(e);
        }
    };
    if known.contains(&new.as_str()) {
        remove_dir(&temp);
        return Ok(MarketplaceFetch::UpToDate);
    }
    // Validate the new catalog before the pin can move.
    if let Err(e) = read_catalog(&temp) {
        remove_dir(&temp);
        return Err(e);
    }
    Ok(MarketplaceFetch::Ready { temp, new })
}

fn marketplace_update(dirs: &Dirs, name: Option<&str>) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let at = match name {
        Some(name) => registry
            .marketplace
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| format!("no marketplace named `{name}` is registered"))?,
        None if registry.marketplace.len() == 1 => 0,
        None if registry.marketplace.is_empty() => return fail("no marketplaces registered"),
        None => return fail("`marketplace update` needs a name when several are registered"),
    };
    let entry = registry.marketplace[at].clone();
    // A marketplace update expands no `${VAR}`, so it needs no environment.
    let no_env = |_: &str| None;
    let fx = Fetch {
        config_dir: &dirs.config,
        env: &no_env,
        git: INTERACTIVE_GIT,
    };
    match update_marketplace(&fx, &mut registry, at)? {
        MarketplaceOutcome::UpToDate { pin } => println!(
            "marketplace `{}` is already up to date ({})",
            entry.name,
            plugins::short_pin(&pin)
        ),
        MarketplaceOutcome::Moved { old, new } => println!(
            "marketplace `{}` moved {} -> {}",
            entry.name,
            plugins::short_pin(&old),
            plugins::short_pin(&new)
        ),
        MarketplaceOutcome::Failed(message) => return Err(message),
    }
    Ok(())
}

/// Moves the marketplace at `at` to its upstream revision, under the registry lock the caller
/// holds, discarding any staged revision.
fn update_marketplace(
    fx: &Fetch<'_>,
    registry: &mut Registry,
    at: usize,
) -> Result<MarketplaceOutcome, String> {
    let entry = registry.marketplace[at].clone();
    let checkout = marketplace_dir(fx.config_dir, &entry.name);
    if !checkout.is_dir() {
        return fail(format!(
            "the checkout of `{}` is missing; remove and re-add it",
            entry.name
        ));
    }
    let (temp, new) = match fetch_marketplace(fx, &entry, Workdir::Cli, &[&entry.pin]) {
        Ok(MarketplaceFetch::UpToDate) => {
            // Upstream is where the pin is, so a staged revision is stale.
            if registry.marketplace[at].staged_pin.take().is_some() {
                remove_dir(&staged_dir(&checkout));
                write_under_lock(fx.config_dir, registry)?;
            }
            return Ok(MarketplaceOutcome::UpToDate { pin: entry.pin });
        }
        Ok(MarketplaceFetch::Ready { temp, new }) => (temp, new),
        Err(e) => return Ok(MarketplaceOutcome::Failed(e)),
    };
    if let Err(e) = replace_dir(&temp, &checkout) {
        remove_dir(&temp);
        return Err(e);
    }
    let old = std::mem::replace(&mut registry.marketplace[at].pin, new.clone());
    registry.marketplace[at].staged_pin = None;
    remove_dir(&staged_dir(&checkout));
    write_under_lock(fx.config_dir, registry)?;
    Ok(MarketplaceOutcome::Moved { old, new })
}

/// The revision the remote's HEAD names, without cloning.
fn ls_remote_head(url: &str, env: GitEnv) -> Option<String> {
    let output = git(Path::new("."), &["ls-remote", "--symref", url, "HEAD"], env).ok()?;
    head_sha(&output)
}

/// The sha on the `HEAD` line of `git ls-remote --symref` output, which prints the symref
/// resolution (`ref: refs/heads/main`) first and the sha on its own line.
fn head_sha(output: &str) -> Option<String> {
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
fn ls_remote_ref(url: &str, name: &str, env: GitEnv) -> Option<String> {
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
fn remote_head(checkout: &Path, env: GitEnv) -> Result<String, String> {
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

fn marketplace_remove(dirs: &Dirs, name: &str) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let at = registry
        .marketplace
        .iter()
        .position(|m| m.name == name)
        .ok_or_else(|| format!("no marketplace named `{name}` is registered"))?;
    if registry.plugin.iter().any(|p| p.marketplace == name) {
        return fail(format!(
            "cannot remove marketplace `{name}` while a plugin installed from it remains; uninstall it first"
        ));
    }
    let checkout = marketplace_dir(&dirs.config, name);
    remove_dir(&checkout);
    remove_dir(&staged_dir(&checkout));
    registry.marketplace.remove(at);
    write_under_lock(&dirs.config, &registry)?;
    println!("removed marketplace `{name}`");
    Ok(())
}

pub(crate) fn install(dirs: &Dirs, host: &Host, spec: &str) -> Result<(), String> {
    let (name, marketplace) = spec
        .split_once('@')
        .filter(|(n, m)| !n.is_empty() && !m.is_empty())
        .ok_or_else(|| "install takes `<name>@<marketplace>`".to_string())?;
    if !valid_plugin_name(name) {
        return fail(format!("`{name}` is not a plugin name"));
    }
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let market = registry
        .marketplace
        .iter()
        .find(|m| m.name == marketplace)
        .ok_or_else(|| format!("no marketplace named `{marketplace}` is registered"))?
        .clone();
    if registry.plugin.iter().any(|p| p.name == name) {
        return fail(format!(
            "a plugin named `{name}` is already installed; plugin names are unique across marketplaces"
        ));
    }
    let market_checkout = marketplace_dir(&dirs.config, &market.name);
    let catalog = read_catalog(&market_checkout)?;
    let entry = catalog
        .plugins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("the catalog of `{marketplace}` has no plugin named `{name}`"))?
        .clone();

    let fx = host_fetch(dirs, host);
    let temp = workdir(&installed_parent(&dirs.config, &market.name), Workdir::Cli)?;
    let source = PluginSource {
        market: &market,
        checkout: &market_checkout,
        pin: &market.pin,
    };
    let materialized = match materialize(&fx, &source, &entry, &temp) {
        Ok(ok) => ok,
        Err(e) => {
            remove_dir(&temp);
            return Err(e);
        }
    };
    print_warnings_of(&materialized.warnings);
    print_summary(&entry, &materialized.build, &temp);
    let approved = approval_of(&temp, name);
    let pin = materialized.pin;
    // Record the install before moving the checkout in: a recorded plugin whose checkout is
    // missing degrades to a warning, while a checkout with no registry entry is an orphan.
    let before = registry.clone();
    registry.plugin.push(InstalledPlugin {
        name: name.to_string(),
        marketplace: market.name.clone(),
        source: entry.source.display(),
        pin: pin.clone(),
        enabled: true,
        approved,
        ..Default::default()
    });
    write_under_lock(&dirs.config, &registry)?;
    let target = checkout_dir(&dirs.config, &market.name, name);
    if let Err(e) = replace_dir(&temp, &target) {
        let _ = write_under_lock(&dirs.config, &before);
        return Err(e);
    }
    println!("installed plugin `{name}` ({})", plugins::short_pin(&pin));
    Ok(())
}

/// The fingerprint of the executable surface the user is about to approve. An entry without one
/// is never auto-updated, so a failure to compute it costs the convenience, not safety.
fn approval_of(root: &Path, name: &str) -> Option<String> {
    match plugins::approval_fingerprint(root) {
        Ok(fingerprint) => Some(fingerprint),
        Err(e) => {
            eprintln!(
                "fontaine: warning: plugin `{name}` will not auto-update: cannot fingerprint it: {e}"
            );
            None
        }
    }
}

fn print_warnings_of(warnings: &[String]) {
    for warning in warnings {
        eprintln!("fontaine: warning: {warning}");
    }
}

/// The marketplace checkout a plugin's catalog entry is read from, and the pin a path source
/// shares.
pub(crate) struct PluginSource<'a> {
    pub market: &'a MarketplaceEntry,
    pub checkout: &'a Path,
    pub pin: &'a str,
}

/// A plugin revision copied or cloned into a scratch directory and validated.
pub(crate) struct Materialized {
    pub temp: PathBuf,
    pub pin: String,
    pub build: plugins::PluginBuild,
    /// Load-time warnings, for the caller to show where it can.
    pub warnings: Vec<String>,
}

/// Copies or clones the entry into `temp`, validates everything and returns the pin and the
/// validated build. Nothing outside `temp` is touched.
pub(crate) fn materialize(
    fx: &Fetch<'_>,
    source: &PluginSource<'_>,
    entry: &CatalogPlugin,
    temp: &Path,
) -> Result<Materialized, String> {
    let pin = match &entry.source {
        CatalogSource::Path(rel) => {
            let from = source.checkout.join(rel);
            let from = from.canonicalize().map_err(|e| {
                format!(
                    "cannot resolve {}: {}",
                    from.display(),
                    fontaine_tools::error_chain::chain(&e)
                )
            })?;
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
    let build = plugins::build_plugin(temp, &entry.name, &ctx).map_err(|e| e.to_string())?;
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
pub(crate) fn would_pin(
    source: &PluginSource<'_>,
    entry: &CatalogPlugin,
    env: GitEnv,
) -> Option<String> {
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
fn same_pin(pin: &str, candidate: &str) -> bool {
    pin == candidate || (!candidate.is_empty() && pin.starts_with(candidate))
}

/// What a fetch of a plugin found.
pub(crate) enum PluginFetch {
    UpToDate,
    Ready(Box<Materialized>),
}

/// Materializes the plugin's upstream revision unless it is already one of the `known` pins.
/// The remote is compared with them before anything is cloned, and the materialized pin after.
pub(crate) fn fetch_plugin(
    fx: &Fetch<'_>,
    source: &PluginSource<'_>,
    entry: &CatalogPlugin,
    kind: Workdir,
    known: &[&str],
) -> Result<PluginFetch, String> {
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
fn validate_components(root: &Path, build: &plugins::PluginBuild) -> Result<Vec<String>, String> {
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
    .map_err(|e| e.to_string())?;
    Ok(warnings)
}

/// A build of the plugin whose `${VAR}` references stay as written, so the approval summary
/// shows what a hook or server will read without printing the value of a secret.
fn unexpanded_build(root: &Path, name: &str) -> Option<plugins::PluginBuild> {
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
fn summary_lines(entry: &CatalogPlugin, build: &plugins::PluginBuild, root: &Path) -> Vec<String> {
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
            for (event, hooks) in [
                ("PreToolUse", &shown.hooks.pre_tool_use),
                ("PostToolUse", &shown.hooks.post_tool_use),
                ("PermissionDenied", &shown.hooks.permission_denied),
            ] {
                for hook in hooks {
                    let matcher = hook
                        .matcher
                        .as_deref()
                        .map(|m| format!(" [{m}]"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  hook {event}{matcher}: {}",
                        with_root_placeholder(&hook.command, root)
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

fn print_summary(entry: &CatalogPlugin, build: &plugins::PluginBuild, root: &Path) {
    for line in summary_lines(entry, build, root) {
        println!("{line}");
    }
    print_warnings(
        &build
            .warnings
            .iter()
            .map(|warning| Notice::warning(warning.clone(), None))
            .collect::<Vec<_>>(),
    );
}

fn uninstall(dirs: &Dirs, name: &str) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let at = registry
        .plugin
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| format!("no plugin named `{name}` is installed"))?;
    let entry = registry.plugin.remove(at);
    let checkout = checkout_dir(&dirs.config, &entry.marketplace, name);
    remove_dir(&checkout);
    remove_dir(&staged_dir(&checkout));
    write_under_lock(&dirs.config, &registry)?;
    println!("uninstalled plugin `{name}`");
    Ok(())
}

/// How one plugin update came out.
pub(crate) enum PluginOutcome {
    /// The new pin equals the installed one (for a `sha`-pinned plugin, the catalog's sha does).
    UpToDate {
        pin: String,
    },
    Moved {
        old: String,
        new: String,
        /// What the user is shown for approval; set by the manual command, which applies the
        /// revision. The background pass stages without applying and has nothing to approve.
        review: Option<Box<Review>>,
    },
    /// A newer revision exists but changes what the user approved, so the old one stays live.
    Held {
        new: String,
        reason: String,
    },
    Failed(String),
}

/// A revision's validated contents, for the approval summary and its load warnings.
pub(crate) struct Review {
    pub entry: CatalogPlugin,
    pub build: plugins::PluginBuild,
    pub warnings: Vec<String>,
    pub root: PathBuf,
}

pub(crate) fn update(dirs: &Dirs, host: &Host, name: &str) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let fx = host_fetch(dirs, host);
    match update_plugin(&fx, &mut registry, name)? {
        PluginOutcome::Moved { new, review, .. } => {
            if let Some(review) = review {
                print_warnings_of(&review.warnings);
                print_summary(&review.entry, &review.build, &review.root);
            }
            println!("plugin `{name}` moved to {}", plugins::short_pin(&new));
            Ok(())
        }
        // Never produced by `update_plugin` (it passes no known pins); the variant is shared
        // with the background pass.
        PluginOutcome::UpToDate { pin } => {
            println!(
                "plugin `{name}` is already up to date ({})",
                plugins::short_pin(&pin)
            );
            Ok(())
        }
        PluginOutcome::Held { reason, .. } | PluginOutcome::Failed(reason) => Err(reason),
    }
}

/// Re-materializes the installed plugin from its marketplace, records the executable surface
/// the user just saw as approved, and replaces the checkout. The revision is applied whether or
/// not the pin moved: a plugin installed by an older build gets its fingerprint recorded this
/// way. Any staged or held revision is discarded.
fn update_plugin(
    fx: &Fetch<'_>,
    registry: &mut Registry,
    name: &str,
) -> Result<PluginOutcome, String> {
    let at = registry
        .plugin
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| format!("no plugin named `{name}` is installed"))?;
    let plugin = registry.plugin[at].clone();
    let market = registry
        .marketplace
        .iter()
        .find(|m| m.name == plugin.marketplace)
        .ok_or_else(|| {
            format!(
                "the marketplace `{}` of `{name}` is not registered",
                plugin.marketplace
            )
        })?
        .clone();
    let market_checkout = marketplace_dir(fx.config_dir, &market.name);
    let catalog = read_catalog(&market_checkout)?;
    let entry = catalog
        .plugins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| {
            format!(
                "the catalog of `{}` no longer has a plugin named `{name}`",
                market.name
            )
        })?
        .clone();

    let source = PluginSource {
        market: &market,
        checkout: &market_checkout,
        pin: &market.pin,
    };
    let fetched = match fetch_plugin(fx, &source, &entry, Workdir::Cli, &[]) {
        Ok(PluginFetch::Ready(m)) => *m,
        Ok(PluginFetch::UpToDate) => unreachable!("no known pins were passed"),
        Err(e) => {
            return fail(format!(
                "the new revision of `{name}` did not validate; the old checkout stays live: {e}"
            ));
        }
    };
    let approved = approval_of(&fetched.temp, name);
    // Record the move before replacing the checkout, and put the old registry back if the
    // replacement fails (replace_dir restores the old checkout itself).
    let before = registry.clone();
    let record = &mut registry.plugin[at];
    let old = std::mem::replace(&mut record.pin, fetched.pin.clone());
    record.source = entry.source.display();
    record.approved = approved;
    record.held_pin = None;
    record.staged_pin = None;
    write_under_lock(fx.config_dir, registry)?;
    let target = checkout_dir(fx.config_dir, &market.name, name);
    if let Err(e) = replace_dir(&fetched.temp, &target) {
        let _ = write_under_lock(fx.config_dir, &before);
        return Err(e);
    }
    remove_dir(&staged_dir(&target));
    let root = target;
    Ok(PluginOutcome::Moved {
        old,
        new: fetched.pin,
        review: Some(Box::new(Review {
            entry,
            build: fetched.build,
            warnings: fetched.warnings,
            root,
        })),
    })
}

pub(crate) fn set_enabled(dirs: &Dirs, name: &str, enabled: bool) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let at = registry
        .plugin
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| format!("no plugin named `{name}` is installed"))?;
    registry.plugin[at].enabled = enabled;
    write_under_lock(&dirs.config, &registry)?;
    println!(
        "plugin `{name}` is now {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn set_marketplace_auto_update(dirs: &Dirs, name: &str, setting: Toggle) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let entry = registry
        .marketplace
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("no marketplace named `{name}` is registered"))?;
    entry.auto_update = opt_out(setting);
    write_under_lock(&dirs.config, &registry)?;
    println!(
        "marketplace `{name}` auto-update is now {}",
        toggle_word(setting)
    );
    Ok(())
}

fn set_plugin_auto_update(dirs: &Dirs, name: &str, setting: Toggle) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
    let entry = registry
        .plugin
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("no plugin named `{name}` is installed"))?;
    entry.auto_update = opt_out(setting);
    write_under_lock(&dirs.config, &registry)?;
    println!(
        "plugin `{name}` auto-update is now {}",
        toggle_word(setting)
    );
    Ok(())
}

/// The registry records only the opt-out: `yes` removes the override and the global
/// `[plugins] auto_update` decides again.
fn opt_out(setting: Toggle) -> Option<bool> {
    (setting == Toggle::No).then_some(false)
}

fn toggle_word(setting: Toggle) -> &'static str {
    match setting {
        Toggle::Yes => "on",
        Toggle::No => "off",
    }
}

fn list(dirs: &Dirs, host: &Host, format: ReportFormat) -> Result<(), String> {
    let registry = plugins::read_registry(&dirs.config).map_err(|e| e.to_string())?;
    match format {
        ReportFormat::Json => {
            let value = serde_json::json!({
                "marketplaces": registry.marketplace.iter().map(|m| serde_json::json!({
                    "name": m.name,
                    "url": m.url,
                    "pin": m.pin,
                    "auto_update": m.auto_update != Some(false),
                })).collect::<Vec<_>>(),
                "plugins": registry.plugin.iter().map(|p| {
                    let counts = component_counts(dirs, host, p);
                    serde_json::json!({
                        "name": p.name,
                        "marketplace": p.marketplace,
                        "source": p.source,
                        "pin": p.pin,
                        "held_pin": p.held_pin,
                        "auto_update": p.auto_update != Some(false),
                        "enabled": p.enabled,
                        "components": counts.map(|s| serde_json::json!({
                            "skills": s.skills,
                            "agents": s.agents,
                            "styles": s.styles,
                            "hooks": s.hooks,
                            "servers": s.servers,
                        })),
                    })
                }).collect::<Vec<_>>(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("a JSON value serializes")
            );
        }
        ReportFormat::Text => {
            if registry.plugin.is_empty() {
                println!("no plugins installed");
                return Ok(());
            }
            for plugin in &registry.plugin {
                let counts = component_counts(dirs, host, plugin)
                    .map(|s| {
                        format!(
                            "{} skill(s), {} agent(s), {} style(s), {} hook(s), {} server(s)",
                            s.skills, s.agents, s.styles, s.hooks, s.servers
                        )
                    })
                    .unwrap_or_else(|| "components unavailable".to_string());
                let mut markers = String::new();
                if let Some(held) = &plugin.held_pin {
                    markers.push_str(&format!(" — held {}", plugins::short_pin(held)));
                }
                if plugin.auto_update == Some(false) {
                    markers.push_str(" — auto-update off");
                }
                println!(
                    "{} ({}): {} — {} — {}{markers}",
                    plugin.name,
                    plugin.marketplace,
                    if plugin.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    plugins::short_pin(&plugin.pin),
                    counts
                );
            }
        }
    }
    Ok(())
}

/// The validated component counts of one installed plugin, for `list`. A missing checkout or an
/// invalid manifest yields `None`: the plugin still lists, just without counts.
fn component_counts(
    dirs: &Dirs,
    host: &Host,
    plugin: &InstalledPlugin,
) -> Option<plugins::PluginSummary> {
    let root = checkout_dir(&dirs.config, &plugin.marketplace, &plugin.name);
    if !root.is_dir() {
        return None;
    }
    let ctx = plugin_context(&*host.env, &dirs.config);
    plugins::build_plugin(&root, &plugin.name, &ctx)
        .ok()
        .map(|build| build.summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_paths_expand_to_the_home_directory() {
        let home = ::dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home.to_string_lossy());
        assert_eq!(expand_tilde("~/x"), home.join("x").to_string_lossy());
        assert_eq!(expand_tilde("https://x/y"), "https://x/y");
        assert_eq!(expand_tilde("plain"), "plain");
    }
}
