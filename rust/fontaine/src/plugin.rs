//! `fontaine plugin`: installing and managing plugins and marketplaces. These are CLI-only
//! mutations: they run outside any session, need no permission pipeline, and keep installs out
//! of a model-reachable surface. Git is shelled out to with fixed argv and no shell.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fontaine_core::dirs::{self, Dirs};
use fontaine_core::extensions;
use fontaine_core::notice::Notice;
use fontaine_core::plugins::{
    self, Catalog, CatalogPlugin, CatalogSource, InstalledPlugin, MarketplaceEntry, PluginContext,
    Registry, checkout_dir, content_hash, marketplace_dir, registry_path, valid_plugin_name,
};
use uuid::Uuid;

use crate::cli::{MarketplaceAction, MarketplaceAddArgs, PluginAction, PluginArgs, ReportFormat};
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
        },
        PluginAction::Install(args) => install(&dirs, host, &args.name),
        PluginAction::Uninstall(args) => uninstall(&dirs, &args.name),
        PluginAction::Update(args) => update(&dirs, host, &args.name),
        PluginAction::Enable(args) => set_enabled(&dirs, &args.name, true),
        PluginAction::Disable(args) => set_enabled(&dirs, &args.name, false),
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

/// A git command with fixed argv, no shell. Its trimmed stdout is the result.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
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

fn temp_workdir(parent: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "cannot create {}: {}",
            parent.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    // A killed run leaves its temp directory behind; sweep the stale ones before adding ours.
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
    let dir = parent.join(format!(".tmp-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).map_err(|e| {
        format!(
            "cannot create {}: {}",
            dir.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    Ok(dir)
}

fn remove_dir(path: &Path) {
    let _ = fontaine_core::owner_only::remove_dir_all_retrying(path);
}

/// Replaces `target` with `temp` atomically enough: the old checkout is kept as a backup until
/// the rename into place has succeeded.
fn replace_dir(temp: &Path, target: &Path) -> Result<(), String> {
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

fn copy_tree(source: &Path, dest: &Path) -> Result<(), String> {
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

fn read_catalog(checkout: &Path) -> Result<Catalog, String> {
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

fn clone_git(url: &str, dest: &Path) -> Result<(), String> {
    require_git()?;
    let dest = dest.to_string_lossy().into_owned();
    git(Path::new("."), &["clone", "--quiet", url, &dest])?;
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
fn pin_clone(dest: &Path, source: &CatalogSource) -> Result<String, String> {
    match source {
        CatalogSource::Github { sha: Some(sha), .. }
        | CatalogSource::Url { sha: Some(sha), .. } => {
            let _ = git(dest, &["fetch", "--quiet", "origin"]);
            git(dest, &["checkout", "--quiet", "--detach", sha])?;
        }
        CatalogSource::Github { r#ref: Some(r), .. }
        | CatalogSource::Url { r#ref: Some(r), .. } => {
            git(dest, &["fetch", "--quiet", "origin", r])?;
            git(dest, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])?;
        }
        _ => {}
    }
    git(dest, &["rev-parse", "HEAD"])
}

fn clone_url_of(source: &CatalogSource) -> String {
    match source {
        CatalogSource::Github { repo, .. } => format!("https://github.com/{repo}.git"),
        CatalogSource::Url { url, .. } => url.clone(),
        CatalogSource::Path(_) => unreachable!("a path source is copied, not cloned"),
    }
}

fn plugin_context<'a>(host: &'a Host, project_root: &'a Path) -> PluginContext<'a> {
    PluginContext {
        project_root,
        env: &*host.env,
    }
}

fn marketplace_add(dirs: &Dirs, add: &MarketplaceAddArgs) -> Result<(), String> {
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
    let parent = dirs
        .config
        .join(plugins::PLUGINS_DIR)
        .join(plugins::MARKETPLACES_DIR);
    let temp = temp_workdir(&parent)?;
    let cleanup = |temp: &Path| remove_dir(temp);

    if local {
        copy_tree(source_path, &temp).inspect_err(|_e| {
            cleanup(&temp);
        })?;
    } else {
        if let Err(e) = clone_git(&source, &temp) {
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
        git(&temp, &["rev-parse", "HEAD"]).inspect_err(|_e| {
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
        println!(
            "{} — {} — pin {} — {} plugin(s)",
            marketplace.name,
            marketplace.url,
            plugins::short_pin(&marketplace.pin),
            installed
        );
    }
    Ok(())
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
    let checkout = marketplace_dir(&dirs.config, &entry.name);
    if !checkout.is_dir() {
        return fail(format!(
            "the checkout of `{}` is missing; remove and re-add it",
            entry.name
        ));
    }
    let parent = dirs
        .config
        .join(plugins::PLUGINS_DIR)
        .join(plugins::MARKETPLACES_DIR);
    let temp = temp_workdir(&parent)?;
    let old = entry.pin.clone();
    let new = if Path::new(&entry.url).is_dir() {
        copy_tree(Path::new(&entry.url), &temp)?;
        content_hash(&temp)?
    } else {
        clone_git(&entry.url, &temp)?;
        let sha = remote_head(&temp).inspect_err(|_e| {
            remove_dir(&temp);
        })?;
        git(&temp, &["checkout", "--quiet", "--detach", &sha])?;
        git(&temp, &["rev-parse", "HEAD"])?
    };
    if new == old {
        remove_dir(&temp);
        println!(
            "marketplace `{}` is already up to date ({})",
            entry.name,
            plugins::short_pin(&old)
        );
        return Ok(());
    }
    // Validate the new catalog before moving the pin.
    if let Err(e) = read_catalog(&temp) {
        remove_dir(&temp);
        return Err(e);
    }
    replace_dir(&temp, &checkout)?;
    registry.marketplace[at].pin = new.clone();
    write_under_lock(&dirs.config, &registry)?;
    println!(
        "marketplace `{}` moved {} -> {}",
        entry.name,
        plugins::short_pin(&old),
        plugins::short_pin(&new)
    );
    Ok(())
}

/// The remote default-branch HEAD of a clone.
fn remote_head(checkout: &Path) -> Result<String, String> {
    git(checkout, &["fetch", "--quiet", "origin"])?;
    // `ls-remote --symref` prints the symref resolution first ("ref: refs/heads/main") and the
    // sha on its own line; the sha is what a checkout takes. Ask the remote rather than the
    // local `origin/HEAD`, whose symref stays on the default branch the clone first saw.
    if let Ok(output) = git(checkout, &["ls-remote", "--symref", "origin", "HEAD"]) {
        for line in output.lines() {
            let mut fields = line.split('\t');
            let sha = fields.next().unwrap_or("");
            if fields.next() == Some("HEAD") && !sha.starts_with("ref:") && !sha.is_empty() {
                return Ok(sha.to_string());
            }
        }
    }
    match git(checkout, &["rev-parse", "origin/HEAD"]) {
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
    registry.marketplace.remove(at);
    write_under_lock(&dirs.config, &registry)?;
    println!("removed marketplace `{name}`");
    Ok(())
}

fn install(dirs: &Dirs, host: &Host, spec: &str) -> Result<(), String> {
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

    let parent = dirs
        .config
        .join(plugins::PLUGINS_DIR)
        .join(plugins::INSTALLED_DIR)
        .join(&market.name);
    let temp = temp_workdir(&parent)?;
    let (pin, build) = match materialize(dirs, host, &market, &entry, &temp) {
        Ok(ok) => ok,
        Err(e) => {
            remove_dir(&temp);
            return Err(e);
        }
    };
    print_summary(&entry, &build);
    // Record the install before moving the checkout in: a recorded plugin whose checkout is
    // missing degrades to a warning, while a checkout with no registry entry is an orphan.
    let before = registry.clone();
    registry.plugin.push(InstalledPlugin {
        name: name.to_string(),
        marketplace: market.name.clone(),
        source: entry.source.display(),
        pin: pin.clone(),
        enabled: true,
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

/// Copies or clones the entry into `temp`, validates everything and returns the pin and the
/// validated build. Nothing outside `temp` is touched.
fn materialize(
    dirs: &Dirs,
    host: &Host,
    market: &MarketplaceEntry,
    entry: &CatalogPlugin,
    temp: &Path,
) -> Result<(String, plugins::PluginBuild), String> {
    let pin = match &entry.source {
        CatalogSource::Path(rel) => {
            let market_checkout = marketplace_dir(&dirs.config, &market.name);
            let source = market_checkout.join(rel);
            let source = source.canonicalize().map_err(|e| {
                format!(
                    "cannot resolve {}: {}",
                    source.display(),
                    fontaine_tools::error_chain::chain(&e)
                )
            })?;
            // Compare canonical to canonical: the config dir itself may sit behind a
            // symlink, and the resolved source never carries one.
            let market_root = market_checkout
                .canonicalize()
                .unwrap_or_else(|_| market_checkout.clone());
            if !source.starts_with(&market_root) {
                return fail(format!(
                    "the source of `{}` points outside its marketplace",
                    entry.name
                ));
            }
            copy_tree(&source, temp)?;
            market.pin.clone()
        }
        CatalogSource::Github { .. } | CatalogSource::Url { .. } => {
            clone_git(&clone_url_of(&entry.source), temp)?;
            pin_clone(temp, &entry.source)?
        }
    };
    let ctx = plugin_context(host, &dirs.config);
    let build = plugins::build_plugin(temp, &entry.name, &ctx).map_err(|e| e.to_string())?;
    validate_components(temp, &build)?;
    Ok((pin, build))
}

/// Loads the plugin's extension files the way a session will, so install catches a bad name,
/// model, mode or duplicate component before the registry records it.
fn validate_components(root: &Path, build: &plugins::PluginBuild) -> Result<(), String> {
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
    for warning in warnings {
        eprintln!("fontaine: warning: {warning}");
    }
    Ok(())
}

fn print_summary(entry: &CatalogPlugin, build: &plugins::PluginBuild) {
    let s = build.summary;
    println!(
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
    );
    for name in build.dir.mcp_names.values() {
        println!("  MCP server {name}");
    }
    // A language server is a program that runs on every edit, so the approval names each.
    for (name, server) in &build.lsp_servers {
        println!("  LSP server {name}: {}", server.command);
    }
    if !build.env_vars.is_empty() {
        // Name the variables, never their values: a reference moves the variable's value into
        // a hook command or a server env/URL, and the install approves that.
        println!(
            "  reads environment variables: {}",
            build
                .env_vars
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
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
    write_under_lock(&dirs.config, &registry)?;
    println!("uninstalled plugin `{name}`");
    Ok(())
}

fn update(dirs: &Dirs, host: &Host, name: &str) -> Result<(), String> {
    let _lock = lock_registry(&dirs.config)?;
    let mut registry = read_under_lock(&dirs.config)?;
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
    let catalog = read_catalog(&marketplace_dir(&dirs.config, &market.name))?;
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

    let parent = dirs
        .config
        .join(plugins::PLUGINS_DIR)
        .join(plugins::INSTALLED_DIR)
        .join(&market.name);
    let temp = temp_workdir(&parent)?;
    let (pin, build) = match materialize(dirs, host, &market, &entry, &temp) {
        Ok(ok) => ok,
        Err(e) => {
            remove_dir(&temp);
            return fail(format!(
                "the new revision of `{name}` did not validate; the old checkout stays live: {e}"
            ));
        }
    };
    print_summary(&entry, &build);
    // Record the move before replacing the checkout, and put the old registry back if the
    // replacement fails (replace_dir restores the old checkout itself).
    let before = registry.clone();
    registry.plugin[at].pin = pin.clone();
    registry.plugin[at].source = entry.source.display();
    write_under_lock(&dirs.config, &registry)?;
    let target = checkout_dir(&dirs.config, &market.name, name);
    if let Err(e) = replace_dir(&temp, &target) {
        let _ = write_under_lock(&dirs.config, &before);
        return Err(e);
    }
    println!("plugin `{name}` moved to {}", plugins::short_pin(&pin));
    Ok(())
}

fn set_enabled(dirs: &Dirs, name: &str, enabled: bool) -> Result<(), String> {
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

fn list(dirs: &Dirs, host: &Host, format: ReportFormat) -> Result<(), String> {
    let registry = plugins::read_registry(&dirs.config).map_err(|e| e.to_string())?;
    match format {
        ReportFormat::Json => {
            let value = serde_json::json!({
                "marketplaces": registry.marketplace.iter().map(|m| serde_json::json!({
                    "name": m.name,
                    "url": m.url,
                    "pin": m.pin,
                })).collect::<Vec<_>>(),
                "plugins": registry.plugin.iter().map(|p| {
                    let counts = component_counts(dirs, host, p);
                    serde_json::json!({
                        "name": p.name,
                        "marketplace": p.marketplace,
                        "source": p.source,
                        "pin": p.pin,
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
                println!(
                    "{} ({}): {} — {} — {}",
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
    let ctx = plugin_context(host, &dirs.config);
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
