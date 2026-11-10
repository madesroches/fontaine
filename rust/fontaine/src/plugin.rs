//! `fontaine plugin`: installing and managing plugins and marketplaces. The commands run outside
//! any session, need no permission pipeline, and keep installs out of a model-reachable surface.
//! The only mutation that runs inside a session process is the staged background pass in
//! `plugin_auto_update`, which reuses the fetch and validation helpers here. Git is shelled out
//! to with fixed argv and no shell.

use std::path::Path;
use std::process::ExitCode;

use fontaine_core::dirs::{self, Dirs};
use fontaine_core::extensions::{self, Scope};
use fontaine_core::notice::Notice;
use fontaine_core::plugin_ops::{
    self, EnvRef, Fetch, commit_install, commit_update, marketplace_line, prepare_install,
    prepare_update,
};
use fontaine_core::plugins::{self, InstalledPlugin, PluginContext, Registry, checkout_dir};
use fontaine_core::skill_trust;

// The shared helpers live in core; the auto-update pass and its tests reach them from here.
pub(crate) use fontaine_core::plugin_ops::{
    INTERACTIVE_GIT, MarketplaceFetch, MarketplaceOutcome, PluginFetch, PluginOutcome,
    UNATTENDED_GIT, Workdir, fetch_marketplace, fetch_plugin, read_catalog, remove_dir,
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
            MarketplaceAction::Add(add) => marketplace_add(&dirs, host, add),
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

pub(crate) fn marketplace_add(
    dirs: &Dirs,
    host: &Host,
    add: &MarketplaceAddArgs,
) -> Result<(), String> {
    let fx = host_fetch(dirs, host);
    let added = plugin_ops::add_marketplace(&fx, &add.source, add.name.as_deref(), Workdir::Cli)?;
    println!(
        "registered marketplace `{}` ({} plugin(s))",
        added.name, added.plugins
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

fn marketplace_update(dirs: &Dirs, name: Option<&str>) -> Result<(), String> {
    // A marketplace update expands no `${VAR}`, so it needs no environment.
    let no_env = |_: &str| None;
    let fx = Fetch {
        config_dir: &dirs.config,
        env: &no_env,
        git: INTERACTIVE_GIT,
    };
    match plugin_ops::update_marketplace_named(&fx, name)? {
        plugin_ops::MarketplaceUpdate::UpToDate { name, pin } => println!(
            "marketplace `{name}` is already up to date ({})",
            plugins::short_pin(&pin)
        ),
        plugin_ops::MarketplaceUpdate::Moved { name, old, new } => println!(
            "marketplace `{name}` moved {} -> {}",
            plugins::short_pin(&old),
            plugins::short_pin(&new)
        ),
    }
    Ok(())
}

fn marketplace_remove(dirs: &Dirs, name: &str) -> Result<(), String> {
    plugin_ops::remove_marketplace(&dirs.config, name)?;
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
    use std::io::IsTerminal;
    let trustable: Vec<extensions::Skill> = extensions::load_plugin_skills(&prepared.build.dir)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|skill| !skill.declared_tools.is_empty())
        .collect();
    let interactive = std::io::stdin().is_terminal();
    let mut to_trust = Vec::new();
    if interactive && !trustable.is_empty() {
        for skill in &trustable {
            print_trust_details(skill);
        }
        let question = format!(
            "Honour the allowed-tools of {}?",
            match trustable.as_slice() {
                [only] => only.name.clone(),
                many => format!("these {} skills", many.len()),
            }
        );
        if crate::auto_mode::confirm(&question) {
            to_trust = trustable
                .iter()
                .map(skill_trust::record_of)
                .collect::<Vec<_>>();
        }
    }
    let installed = commit_install(&fx, prepared)?;
    println!(
        "installed plugin `{}` ({})",
        installed.name,
        plugins::short_pin(&installed.pin)
    );
    if !to_trust.is_empty() {
        let count = to_trust.len();
        skill_trust::trust(&dirs.config, to_trust).map_err(|e| {
            format!(
                "the plugin is installed, but the trust was not recorded: {}",
                fontaine_tools::error_chain::chain(&e)
            )
        })?;
        println!(
            "trusted {count} skill(s); their allowed-tools count from the install's own reload"
        );
    } else if !interactive && !trustable.is_empty() {
        println!(
            "note: {} skill(s) declare allowed-tools and stay untrusted; `fontaine plugin trust {}` reviews and trusts them",
            trustable.len(),
            installed.name
        );
    }
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
