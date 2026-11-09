//! `fontaine plugin`: installing and managing plugins and marketplaces. The commands run outside
//! any session, need no permission pipeline, and keep installs out of a model-reachable surface.
//! The only mutation that runs inside a session process is the staged background pass in
//! `plugin_auto_update`, which reuses the fetch and validation helpers here. Git is shelled out
//! to with fixed argv and no shell.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fontaine_core::dirs::{self, Dirs};
use fontaine_core::extensions::{self, Scope};
use fontaine_core::notice::Notice;
use fontaine_core::plugin_ops::{
    self, EnvRef, Fetch, clone_git, commit_install, commit_update, git, ls_remote_head,
    prepare_install, prepare_update, remote_head,
};
use fontaine_core::plugins::{
    self, InstalledPlugin, MarketplaceEntry, PluginContext, Registry, checkout_dir, content_hash,
    marketplace_dir, staged_dir, valid_plugin_name,
};
use fontaine_core::skill_trust;

// The shared helpers live in core; the auto-update pass and its tests reach them from here.
pub(crate) use fontaine_core::plugin_ops::{
    INTERACTIVE_GIT, PluginFetch, PluginOutcome, UNATTENDED_GIT, Workdir, copy_tree, fetch_plugin,
    marketplaces_parent, read_catalog, remove_dir, replace_dir, workdir,
};

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
        PluginAction::Trust(args) => trust(&dirs, host, &args.name, args.yes),
        PluginAction::Untrust(args) => untrust(&dirs, &args.name),
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
    fontaine_core::file_lock::lock(&plugins::registry_path(config_dir)).map_err(|e| {
        format!(
            "cannot lock {}: {}",
            plugins::registry_path(config_dir).display(),
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
            return Err(e.into());
        }
    }

    let catalog = match read_catalog(&temp) {
        Ok(catalog) => catalog,
        Err(e) => {
            cleanup(&temp);
            return Err(e.into());
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
        return Err(e.into());
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
        let offered = match read_catalog(&plugins::marketplace_dir(&dirs.config, &marketplace.name))
        {
            Ok(catalog) => Some(catalog.plugins.len()),
            Err(e) => {
                eprintln!("fontaine: warning: marketplace `{}`: {e}", marketplace.name);
                None
            }
        };
        println!("{}", marketplace_line(marketplace, offered, installed));
    }
    Ok(())
}

/// One `marketplace list` line. `offered` is `None` when the catalog could not be read, which must
/// not look like an empty catalog.
fn marketplace_line(entry: &MarketplaceEntry, offered: Option<usize>, installed: usize) -> String {
    let counts = match offered {
        Some(n) => format!("{n} offered, {installed} installed"),
        None => format!("catalog unreadable, {installed} installed"),
    };
    let marker = if entry.auto_update == Some(false) {
        " — auto-update off"
    } else {
        ""
    };
    format!(
        "{} — {} — pin {} — {counts}{marker}",
        entry.name,
        entry.url,
        plugins::short_pin(&entry.pin)
    )
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
        copy_tree(Path::new(&entry.url), &temp)
            .map_err(String::from)
            .and_then(|()| content_hash(&temp))
    } else {
        clone_git(&entry.url, &temp, fx.git)
            .and_then(|()| {
                let sha = remote_head(&temp, fx.git)?;
                git(&temp, &["checkout", "--quiet", "--detach", &sha], fx.git)?;
                git(&temp, &["rev-parse", "HEAD"], fx.git)
            })
            .map_err(String::from)
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
        return Err(e.into());
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
        return Err(e.into());
    }
    let old = std::mem::replace(&mut registry.marketplace[at].pin, new.clone());
    registry.marketplace[at].staged_pin = None;
    remove_dir(&staged_dir(&checkout));
    write_under_lock(fx.config_dir, registry)?;
    Ok(MarketplaceOutcome::Moved { old, new })
}

/// The revision the remote's HEAD names, without cloning.
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
    let fx = host_fetch(dirs, host);
    let prepared = prepare_install(&fx, spec, Workdir::Cli)?;
    print_warnings_of(&prepared.warnings);
    print_summary(&prepared.summary, &prepared.build.warnings);
    if let Some(warning) = &prepared.fingerprint_warning {
        eprintln!("fontaine: warning: {warning}");
    }
    let installed = commit_install(&fx, prepared)?;
    println!(
        "installed plugin `{}` ({})",
        installed.name,
        plugins::short_pin(&installed.pin)
    );
    Ok(())
}

fn print_warnings_of(warnings: &[String]) {
    for warning in warnings {
        eprintln!("fontaine: warning: {warning}");
    }
}

fn print_summary(summary: &[String], build_warnings: &[String]) {
    for line in summary {
        println!("{line}");
    }
    print_warnings(
        &build_warnings
            .iter()
            .map(|warning| Notice::warning(warning.clone(), None))
            .collect::<Vec<_>>(),
    );
}

fn uninstall(dirs: &Dirs, name: &str) -> Result<(), String> {
    let removed = plugin_ops::uninstall(&dirs.config, name)?;
    print_warnings_of(&removed.warnings);
    println!("uninstalled plugin `{name}`");
    Ok(())
}

pub(crate) fn update(dirs: &Dirs, host: &Host, name: &str) -> Result<(), String> {
    let fx = host_fetch(dirs, host);
    let prepared = prepare_update(&fx, name, Workdir::Cli)?;
    let summary = prepared.summary.clone();
    let warnings = prepared.warnings.clone();
    let build_warnings = prepared.build.warnings.clone();
    if let Some(warning) = &prepared.fingerprint_warning {
        eprintln!("fontaine: warning: {warning}");
    }
    let updated = commit_update(&fx, prepared)?;
    print_warnings_of(&warnings);
    print_summary(&summary, &build_warnings);
    println!(
        "plugin `{name}` moved to {}",
        plugins::short_pin(&updated.new_pin)
    );
    Ok(())
}

pub(crate) fn set_enabled(dirs: &Dirs, name: &str, enabled: bool) -> Result<(), String> {
    plugin_ops::set_enabled(&dirs.config, name, enabled)?;
    println!(
        "plugin `{name}` is now {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

/// The skills `trust` can name: the user directory's and those of installed, enabled plugins.
/// The project root is a path that holds nothing, so a repository's files are never read.
fn trustable_skills(
    dirs: &Dirs,
    host: &Host,
) -> Result<(extensions::Extensions, Vec<String>), String> {
    let none = dirs.config.join(".fontaine-skill-trust-enumeration");
    let contributions = plugins::load_installed(&dirs.config, &plugin_context(&*host.env, &none))
        .map_err(|e| e.to_string())?;
    let plugin_names = contributions.dirs.iter().map(|d| d.name.clone()).collect();
    let mut warnings = Vec::new();
    let extensions = extensions::load(&mut extensions::LoadParams {
        config_dir: &dirs.config,
        project_root: &none,
        servers: &[],
        plugins: &contributions.dirs,
        warnings: &mut warnings,
    })
    .map_err(|e| e.to_string())?;
    print_warnings_of(&contributions.warnings);
    print_warnings_of(&warnings);
    Ok((extensions, plugin_names))
}

/// The skills a name stands for: the skill itself, or every skill an installed plugin has.
fn named_skills<'a>(
    extensions: &'a extensions::Extensions,
    plugin_names: &[String],
    name: &str,
) -> Result<Vec<&'a extensions::Skill>, String> {
    let skill = extensions
        .skill(name)
        .filter(|skill| skill.scope != Scope::Project);
    let is_plugin = plugin_names.iter().any(|plugin| plugin == name);
    match (skill, is_plugin) {
        (Some(_), true) => fail(format!(
            "`{name}` is both an installed plugin and a skill; name the skill as `<plugin>:{name}` or rename the skill"
        )),
        (Some(skill), false) => Ok(vec![skill]),
        (None, true) => {
            let prefix = format!("{name}:");
            let skills: Vec<_> = extensions
                .skills
                .iter()
                .filter(|skill| skill.scope == Scope::Plugin && skill.name.starts_with(&prefix))
                .collect();
            if skills.is_empty() {
                return fail(format!("plugin `{name}` has no skills"));
            }
            Ok(skills)
        }
        (None, false) => fail(format!(
            "no skill or installed plugin named `{name}`; trust takes a skill name as `/skills` shows it, or the name of an installed, enabled plugin"
        )),
    }
}

fn print_trust_details(skill: &extensions::Skill) {
    let (specific, wildcard) = skill_trust::split_tools(skill);
    println!("skill {}", skill.name);
    println!("  file: {}", skill.path.display());
    println!("  digest: {}", skill.digest);
    let list = |label: &str, rules: &[String]| {
        if rules.is_empty() {
            println!("  {label}: (none)");
        } else {
            println!("  {label}: {}", rules.join(", "));
        }
    };
    if skill.scope == Scope::User {
        list("allowed-tools (already allowed, trusted or not)", &specific);
        list(
            "wildcard shell rules (already allowed too; trust only adds the guardian note)",
            &wildcard,
        );
    } else {
        list("allowed-tools that become allows", &specific);
        list(
            "wildcard shell rules (guardian context only, never an allow)",
            &wildcard,
        );
    }
    let commands = skill_trust::command_lines(&skill.body);
    if commands.is_empty() {
        println!("  commands it runs: (none)");
    } else {
        println!("  commands it runs:");
        for command in commands {
            println!("    {command}");
        }
    }
}

fn trust(dirs: &Dirs, host: &Host, name: &str, yes: bool) -> Result<(), String> {
    use std::io::IsTerminal;
    let (extensions, plugin_names) = trustable_skills(dirs, host)?;
    let skills = named_skills(&extensions, &plugin_names, name)?;
    for skill in &skills {
        print_trust_details(skill);
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return fail(
                "refusing to trust without a terminal to confirm on; pass --yes to record without asking",
            );
        }
        let question = match skills.as_slice() {
            [only] => format!("Honour the allowed-tools of {}?", only.name),
            many => format!("Honour the allowed-tools of these {} skills?", many.len()),
        };
        if !crate::auto_mode::confirm(&question) {
            return fail("not trusted");
        }
    }
    skill_trust::trust(
        &dirs.config,
        skills
            .iter()
            .map(|skill| skill_trust::record_of(skill))
            .collect(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "trusted {} skill(s); this takes effect at the next session start, or at /reload-plugins in a running one",
        skills.len()
    );
    Ok(())
}

fn untrust(dirs: &Dirs, name: &str) -> Result<(), String> {
    let (trusted, unreadable) = skill_trust::load(&dirs.config);
    if let Some(message) = unreadable {
        return fail(message);
    }
    let prefix = format!("{name}:");
    let is_plugin = trusted
        .records()
        .iter()
        .any(|record| record.name.starts_with(&prefix));
    let is_skill = trusted.records().iter().any(|record| record.name == name);
    if is_skill && is_plugin {
        return fail(format!(
            "`{name}` is both a plugin and a skill; name the skill as `<plugin>:{name}` or rename the skill"
        ));
    }
    let names: Vec<String> = trusted
        .records()
        .iter()
        .filter(|record| {
            if is_plugin {
                record.name.starts_with(&prefix)
            } else {
                record.name == name
            }
        })
        .map(|record| record.name.clone())
        .collect();
    if names.is_empty() {
        return fail(format!("no trusted skill or plugin named `{name}`"));
    }
    skill_trust::untrust(&dirs.config, &names).map_err(|e| e.to_string())?;
    println!(
        "untrusted {} skill(s); this takes effect at the next session start, or at /reload-plugins in a running one",
        names.len()
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

    fn entry() -> MarketplaceEntry {
        MarketplaceEntry {
            name: "team".into(),
            url: "https://x/team".into(),
            pin: "0123456789abcdef".into(),
            ..Default::default()
        }
    }

    #[test]
    fn marketplace_line_shows_offered_and_installed() {
        let line = marketplace_line(&entry(), Some(3), 1);
        assert!(line.ends_with("— 3 offered, 1 installed"), "{line}");
    }

    #[test]
    fn marketplace_line_shows_a_fresh_marketplace_as_offering_with_none_installed() {
        let line = marketplace_line(&entry(), Some(3), 0);
        assert!(line.ends_with("— 3 offered, 0 installed"), "{line}");
    }

    #[test]
    fn marketplace_line_does_not_pass_an_unreadable_catalog_off_as_empty() {
        let line = marketplace_line(&entry(), None, 2);
        assert!(
            line.ends_with("— catalog unreadable, 2 installed"),
            "{line}"
        );
        assert!(!line.contains("0 offered"), "{line}");
    }

    #[test]
    fn marketplace_line_appends_the_auto_update_marker() {
        let line = marketplace_line(
            &MarketplaceEntry {
                auto_update: Some(false),
                ..entry()
            },
            Some(1),
            0,
        );
        assert!(
            line.ends_with("— 1 offered, 0 installed — auto-update off"),
            "{line}"
        );
        let on = MarketplaceEntry {
            auto_update: Some(true),
            ..entry()
        };
        assert!(!marketplace_line(&on, Some(1), 0).contains("auto-update"));
    }
}
