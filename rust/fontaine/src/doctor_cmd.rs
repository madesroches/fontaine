//! `fontaine doctor`: reads the setup and says what loaded, what did not, and why. It starts no
//! session and sends no prompt; the only processes it starts are the MCP servers a session
//! would start, and only to count their tools.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fontaine_core::config::{self, Config, LayerDiagnosis, Slot};
use fontaine_core::config_catalog::Running;
use fontaine_core::containment;
use fontaine_core::dirs::Dirs;
use fontaine_core::doctor_report::{
    self as report, DoctorReport, ExtensionFacts, Finding, GuardianFact, HookCount, McpRow,
    ModelFacts, ProjectExecutables, Section, SkillCounts, TrustFacts,
};
use fontaine_core::extensions::{self, Scope};
use fontaine_core::instructions::{self, Sources};
use fontaine_core::memory;
use fontaine_core::models::{self, GuardianChoice, GuardianModel, LISTING_TIMEOUT, ResolveParams};
use fontaine_core::plugins::PluginContributions;
use fontaine_core::trust;
use fontaine_guardian::Profile;
use fontaine_tools::error_chain::chain;

use crate::cli::{Cli, DoctorArgs, ReportFormat, Toggle};
use crate::mcp_probe::{apply_installed_plugins, probe_servers};
use crate::telemetry::{self, DebugSink};
use crate::{
    Host, home_dir, load_params, resolve_dirs, resolve_roots, shared_env, timeout, trust_summary,
};

pub fn run(cli: &Cli, args: &DoctorArgs, host: &Host, sink: Option<&DebugSink>) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("fontaine: cannot start the async runtime: {}", chain(&e));
            return ExitCode::from(1);
        }
    };
    let report = runtime.block_on(build_report(cli, args, host, sink));
    match args.format.format {
        ReportFormat::Text => print!("{}", report.render_text()),
        ReportFormat::Json => println!("{}", report.to_json()),
    }
    if report.failed() {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

async fn build_report(
    cli: &Cli,
    args: &DoctorArgs,
    host: &Host,
    sink: Option<&DebugSink>,
) -> DoctorReport {
    let version = crate::VERSION;
    let dirs = match telemetry::with_debug_only(sink, || resolve_dirs(host)) {
        Ok(dirs) => dirs,
        Err(e) => {
            return DoctorReport {
                sections: vec![report::install_failure_section(version, &e)],
            };
        }
    };
    let params = load_params(cli, host, &dirs);
    let loaded = telemetry::with_debug_only(sink, || config::load(&params));
    let (mut config, load_error, layers) = match loaded {
        Ok(config) => {
            let layers = LayerDiagnosis::from_scopes(&config, &dirs.data);
            (Some(config), None, layers)
        }
        Err(e) => {
            let layers = telemetry::with_debug_only(sink, || config::diagnose(&params));
            (None, Some(e), layers)
        }
    };
    let workspace = match &config {
        Some(config) => config.workspace.clone(),
        None => std::fs::canonicalize(&host.cwd).unwrap_or_else(|_| host.cwd.clone()),
    };

    let mut sections = vec![
        report::install_section(version, &dirs.config, &dirs.data, &workspace),
        report::layers_section(
            &layers,
            load_error.as_ref().map(|e| e as &dyn std::error::Error),
        ),
    ];
    let trusted = trust::is_trusted(&dirs.data, &workspace);

    let Some(base) = config.take() else {
        sections.push(report::skipped_section(report::SETTINGS));
        sections.push(report::trust_section(&TrustFacts {
            workspace: &workspace,
            trusted,
            store_problem: trust::unreadable(&dirs.data),
            project: None,
        }));
        sections.push(report::skipped_section(report::MODELS));
        sections.push(report::skipped_section(report::MCP));
        sections.push(report::skipped_section(report::EXTENSIONS));
        sections.push(memory_section(&dirs, &workspace, None));
        return DoctorReport { sections };
    };

    // Plugin servers, hooks and skills belong to a session's configuration, so they are applied
    // here; staged updates are not, because applying them writes.
    let (config, contributions, plugin_error) = apply_installed_plugins(base, &dirs, host);

    let resolved = timeout(
        LISTING_TIMEOUT,
        models::resolve(&ResolveParams {
            config: &config,
            data_dir: &dirs.data,
            env: shared_env(host),
            model_override: cli.model.as_deref(),
            session_id: None,
        }),
    )
    .await;
    let pinned_guardian = resolved.as_ref().ok().and_then(|m| {
        (m.guardian_choice == GuardianChoice::Pinned)
            .then(|| m.guardian.as_ref().map(|g| g.reference().to_string()))
            .flatten()
    });
    let main_model = resolved
        .as_ref()
        .map(|m| m.main.reference.to_string())
        .unwrap_or_else(|_| "(not resolved)".to_string());
    let running = Running {
        theme: config.tui.theme.as_str(),
        main_model: &main_model,
        guardian_model: pinned_guardian.as_deref(),
        effort: cli.effort.or(config.effort),
    };
    sections.push(report::settings_section(&config, &running, args.all));
    sections.push(report::trust_section(&TrustFacts {
        workspace: &workspace,
        trusted,
        store_problem: trust::unreadable(&dirs.data),
        project: Some({
            let summary = trust_summary(&config);
            ProjectExecutables {
                hooks: summary.hooks,
                servers: summary.servers,
                skills: summary.skills,
            }
        }),
    }));
    let contained = containment::resolve(
        config.guardian.profile,
        cli.in_container,
        containment::detect(),
    );
    let mut models_section = report::models_section(
        resolved.as_ref().map(model_facts).map_err(|e| chain(&**e)),
        match contained.profile {
            Profile::Full => "full",
            Profile::Minimal => "minimal",
        },
    );
    if let Some(notice) = contained.notice {
        models_section
            .findings
            .push(Finding::warn("notice", notice.headline));
    }
    sections.push(models_section);
    sections.push(mcp_section(&config, &dirs, host, args, trusted).await);
    sections.push(extensions_section(
        cli,
        &config,
        &dirs,
        host,
        &contributions,
        plugin_error,
        trusted,
    ));
    sections.push(memory_section(&dirs, &workspace, Some(config.memory.auto)));
    DoctorReport { sections }
}

fn model_facts(models: &models::Models) -> ModelFacts {
    ModelFacts {
        main: models.main.reference.to_string(),
        guardian: match &models.guardian {
            Some(GuardianModel::Ready(handle)) => GuardianFact::Ready(handle.reference.to_string()),
            Some(GuardianModel::Unavailable { reference, reason }) => GuardianFact::Unavailable {
                reference: reference.to_string(),
                reason: reason.clone(),
            },
            None => GuardianFact::NotConfigured,
        },
        choice: match models.guardian_choice {
            GuardianChoice::Pinned => "pinned",
            GuardianChoice::Default => "default",
            GuardianChoice::MainFallback => "main model",
        },
        notices: models.notices.iter().map(ToString::to_string).collect(),
    }
}

async fn mcp_section(
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    args: &DoctorArgs,
    trusted: bool,
) -> Section {
    let probed = probe_servers(
        config,
        dirs,
        host,
        trusted,
        args.connect != Toggle::No,
        None,
    )
    .await;
    let rows: Vec<McpRow> = probed
        .into_iter()
        .map(|probed| {
            let server = &config.mcp_servers[&probed.name];
            let origin = config
                .mcp_origins
                .get(&probed.name)
                .copied()
                .unwrap_or_default();
            McpRow {
                name: probed.name,
                origin: origin.slot.label(),
                transport: if server.command.is_some() {
                    "stdio"
                } else {
                    "http"
                },
                gate: probed.gate,
                project: origin.is_project(),
                state: probed.state,
            }
        })
        .collect();
    report::mcp_section(&rows)
}

fn extensions_section(
    cli: &Cli,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    contributions: &PluginContributions,
    plugin_error: Option<String>,
    trusted: bool,
) -> Section {
    let roots = resolve_roots(cli, config, host);
    let sources = Sources::for_launch(
        dirs.config.clone(),
        home_dir(host),
        &roots.roots,
        config
            .permissions
            .deny
            .iter()
            .map(|r| r.rule.clone())
            .collect(),
    );
    let loaded = instructions::load(&sources);

    let mut warnings: Vec<String> = roots.warnings;
    warnings.extend(config.warnings.iter().cloned());
    warnings.extend(contributions.warnings.iter().cloned());
    warnings.extend(config.lsp_table().warnings);
    let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
    let mut skill_warnings = Vec::new();
    let skills = extensions::load(&mut extensions::LoadParams {
        config_dir: &dirs.config,
        project_root: &config.workspace,
        servers: &servers,
        plugins: &contributions.dirs,
        warnings: &mut skill_warnings,
    })
    .map(|loaded| {
        let count = |scope| loaded.skills.iter().filter(|s| s.scope == scope).count();
        SkillCounts {
            user: count(Scope::User),
            project: count(Scope::Project),
            plugin: count(Scope::Plugin),
            agents: loaded.agent_types.len(),
        }
    })
    .map_err(|e| chain(&e));
    warnings.extend(skill_warnings);

    report::extensions_section(&ExtensionFacts {
        instructions: loaded
            .files
            .iter()
            .map(|f| (f.path.clone(), f.origin.label()))
            .collect(),
        instruction_warnings: loaded.warnings,
        hooks: hook_counts(config),
        workspace_trusted: trusted,
        skills,
        plugin_error,
        warnings,
    })
}

/// Hooks per event and scope, in the order the events are documented.
fn hook_counts(config: &Config) -> Vec<HookCount> {
    let events = config
        .hooks
        .iter()
        .map(|(event, list)| (event.config_key(), list));
    let mut out = Vec::new();
    for (event, list) in events {
        for slot in [
            Slot::User,
            Slot::Project,
            Slot::ProjectLocal,
            Slot::Cli,
            Slot::Plugin,
        ] {
            let count = list.iter().filter(|h| h.origin.slot == slot).count();
            if count > 0 {
                out.push(HookCount {
                    event,
                    scope: slot.label(),
                    project: matches!(slot, Slot::Project | Slot::ProjectLocal),
                    count,
                    never_run: list
                        .iter()
                        .filter(|h| {
                            h.origin.slot == slot
                                && h.origin.is_project()
                                && h.kind != config::HandlerKind::Command
                                && !h.origin.user_trust
                        })
                        .count(),
                });
            }
        }
    }
    out
}

fn memory_section(dirs: &Dirs, workspace: &Path, auto: Option<bool>) -> Section {
    let dir: PathBuf = memory::dir(&dirs.data, workspace);
    report::memory_section(&dir, auto, &memory::check(&dir))
}
