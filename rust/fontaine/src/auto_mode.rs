//! `fontaine auto-mode`: shows the rules the guardian judges against. Nothing here starts a
//! session or runs a project executable; the only setting `setup` can change is the guardian
//! composition, and only after the user confirms it.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use fontaine_core::composition_offer::{self, Offer, Posture};
use fontaine_core::config::{Config, Slot};
use fontaine_core::containment;
use fontaine_core::dirs::Dirs;
use fontaine_core::models::{self, GuardianModel, ResolveParams, Switcher};
use fontaine_core::project_env::discover::{self, SystemGh};
use fontaine_core::project_env::sample;
use fontaine_core::project_env::{self, ProjectEnvironment, ScanStatus, ScanStep};
use fontaine_core::workspace::{self, RootSource};
use fontaine_core::{audit, ledger, trust};
use fontaine_guardian::critique::{self, CritiqueError};
use fontaine_guardian::rules::{DEFAULTS_MARKER, has_custom, lists};
use fontaine_guardian::{EnvironmentConfig, List, Profile, RuleConfig};
use fontaine_tools::display::escape_controls;
use fontaine_tools::error_chain::chain;
use rig_core::completion::Usage;
use serde_json::{Value, json};

use crate::cli::{AutoModeAction, AutoModeArgs, Cli, ReportFormat, SetupArgs};
use crate::{EXIT_CONFIG, Host, print_warnings};

const NOTHING_TO_CRITIQUE: &str = "nothing to critique: no custom entries";
const MINIMAL_NOTE: &str = "the minimal profile's prompt carries a reduced ruleset of its own and the rule lists (allow, soft_deny, hard_deny) are not sent to the guardian; only the environment list is";

pub fn run(cli: &Cli, args: &AutoModeArgs, config: &Config, dirs: &Dirs, host: &Host) -> ExitCode {
    match &args.action {
        AutoModeAction::Defaults(f) => emit(&defaults(f.format)),
        AutoModeAction::Config(f) => {
            let (profile, warnings) = profile(cli, config);
            print_warnings(&warnings);
            let stored = project_env::load(&dirs.data, &config.workspace);
            if let Some(warning) = &stored.warning {
                eprintln!("fontaine: warning: {warning}");
            }
            let shown = stored.record.map(project_env::expire_now);
            emit(&config_report(config, profile, f.format, shown.as_ref()))
        }
        AutoModeAction::Critique(f) => critique_run(cli, config, dirs, host, f.format),
        AutoModeAction::Setup(args) => setup_run(cli, args, config, dirs, host),
    }
}

fn emit(text: &str) -> ExitCode {
    println!("{text}");
    ExitCode::SUCCESS
}

/// The profile a session started with the same flags would run.
fn profile(cli: &Cli, config: &Config) -> (Profile, Vec<fontaine_core::notice::Notice>) {
    let resolved = containment::resolve(
        config.guardian.profile,
        cli.in_container,
        containment::detect(),
    );
    (resolved.profile, resolved.notice.into_iter().collect())
}

fn json_text(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a JSON value serializes")
}

/// The shipped lists. Text is a TOML fragment that pastes into a settings file.
pub fn defaults(format: ReportFormat) -> String {
    match format {
        ReportFormat::Json => {
            let mut map = serde_json::Map::new();
            for list in List::ALL {
                map.insert(list.name().into(), json!(list.defaults()));
            }
            json_text(&Value::Object(map))
        }
        ReportFormat::Text => {
            let mut out = String::from("[auto_mode]\n");
            for list in List::ALL {
                out.push_str(&format!("{} = [\n", list.name()));
                for entry in list.defaults() {
                    let quoted = serde_json::to_string(entry).expect("a string serializes");
                    out.push_str(&format!("    {quoted},\n"));
                }
                out.push_str("]\n");
            }
            out.trim_end().to_string()
        }
    }
}

fn scopes_of(config: &Config, list: List) -> Vec<&'static str> {
    config
        .sources
        .get(&format!("auto_mode.{}", list.name()))
        .map(|slots| slots.iter().map(|slot: &Slot| slot.label()).collect())
        .unwrap_or_default()
}

fn semantics(list: List) -> String {
    if list.keeps_defaults() {
        "always keeps the shipped entries; configured entries add to them".to_string()
    } else {
        format!(
            "a configured list replaces the shipped entries unless it carries \"{DEFAULTS_MARKER}\", which splices them in"
        )
    }
}

const NO_ENVIRONMENT: &str = "none — run fontaine auto-mode setup";

/// The effective lists with where each came from, and the project's stored environment facts
/// as a session would use them.
pub fn config_report(
    config: &Config,
    profile: Profile,
    format: ReportFormat,
    stored: Option<&ProjectEnvironment>,
) -> String {
    let (environment, rules) = config.effective_auto_mode();
    let minimal = profile == Profile::Minimal;
    match format {
        ReportFormat::Json => {
            let mut map = serde_json::Map::new();
            map.insert("profile".into(), json!(profile.as_str()));
            map.insert("rules_sent".into(), json!(!minimal));
            for (list, entries) in lists(&environment, &rules) {
                let entries: Vec<Value> = entries
                    .iter()
                    .map(|text| {
                        json!({
                            "text": text,
                            "origin": if list.is_default(text) { "default" } else { "custom" },
                        })
                    })
                    .collect();
                map.insert(
                    list.name().into(),
                    json!({"scopes": scopes_of(config, list), "entries": entries}),
                );
            }
            map.insert(
                "project_environment".into(),
                stored.map_or(Value::Null, |record| {
                    serde_json::to_value(record).expect("a record serializes")
                }),
            );
            json_text(&Value::Object(map))
        }
        ReportFormat::Text => {
            let mut out = String::new();
            if minimal {
                out.push_str(&format!("note: {MINIMAL_NOTE}\n\n"));
            }
            for (list, entries) in lists(&environment, &rules) {
                let scopes = scopes_of(config, list);
                let source = if scopes.is_empty() {
                    "shipped defaults".to_string()
                } else {
                    scopes.join(", ")
                };
                out.push_str(&format!("auto_mode.{}  ({source})\n", list.name()));
                out.push_str(&format!("  {}\n", semantics(list)));
                for entry in entries {
                    let origin = if list.is_default(entry) {
                        "default"
                    } else {
                        "custom "
                    };
                    out.push_str(&format!("  [{origin}] {}\n", escape_controls(entry)));
                }
                out.push('\n');
            }
            match stored {
                None => out.push_str(&format!("project_environment: {NO_ENVIRONMENT}")),
                Some(record) => {
                    out.push_str(&format!(
                        "project_environment  (collected {})\n",
                        record.discovered_at
                    ));
                    for line in project_env::report(record, &[]) {
                        out.push_str(&format!("  {}\n", escape_controls(&line)));
                    }
                }
            }
            out.trim_end().to_string()
        }
    }
}

/// The report of a critique that was asked for.
pub fn critique_report(
    model: &str,
    text: &str,
    usage: &Usage,
    billed: bool,
    format: ReportFormat,
) -> String {
    match format {
        ReportFormat::Text => format!(
            "{text}\n\nmodel: {model} ({})",
            project_env::usage_line(usage, billed)
        ),
        ReportFormat::Json => json_text(&json!({
            "model": model,
            "critique": text,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cost_usd": usage.cost.filter(|_| billed).map(|c| c.total),
            },
        })),
    }
}

/// The report of a critique that was not asked for, with the reason.
pub fn skipped_report(note: &str, format: ReportFormat) -> String {
    match format {
        ReportFormat::Text => note.to_string(),
        ReportFormat::Json => json_text(&json!({"critique": null, "note": note})),
    }
}

fn fail(message: impl std::fmt::Display, code: u8) -> ExitCode {
    eprintln!("fontaine: {message}");
    ExitCode::from(code)
}

fn critique_run(
    cli: &Cli,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    format: ReportFormat,
) -> ExitCode {
    let (environment, rules) = config.effective_auto_mode();
    if !has_custom(&environment, &rules) {
        return emit(&skipped_report(NOTHING_TO_CRITIQUE, format));
    }
    let (profile, warnings) = profile(cli, config);
    print_warnings(&warnings);
    if profile == Profile::Minimal {
        return emit(&skipped_report(
            &format!("nothing to critique: {MINIMAL_NOTE}"),
            format,
        ));
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(format!("cannot start the async runtime: {e}"), 1),
    };
    runtime.block_on(critique_models(
        cli,
        config,
        dirs,
        host,
        format,
        (&environment, &rules),
    ))
}

async fn critique_models(
    cli: &Cli,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
    format: ReportFormat,
    (environment, rules): (&EnvironmentConfig, &RuleConfig),
) -> ExitCode {
    let resolved = models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: &*host.env,
        model_override: cli.model.as_deref(),
        session_id: None,
    })
    .await;
    let models = match resolved {
        Ok(models) => models,
        Err(e) => return fail(chain(&e), e.exit_code()),
    };
    print_warnings(&models.notices);
    let handle = match models.guardian {
        Some(GuardianModel::Ready(handle)) => handle,
        Some(GuardianModel::Unavailable { reference, reason }) => {
            return fail(
                format!("the guardian model {reference} is unavailable: {reason}"),
                EXIT_CONFIG,
            );
        }
        None => return fail("no guardian model is configured", EXIT_CONFIG),
    };
    let label = handle.reference.to_string();
    eprintln!(
        "fontaine: sending your custom auto_mode entries to {label} for review (the account's provider receives them)"
    );
    let settings = critique::Settings {
        timeout: std::time::Duration::from_secs(config.guardian.timeout_secs),
        additional_params: handle.thinking_off.clone(),
    };
    match critique::review(&handle.completion, &settings, environment, rules).await {
        Ok((text, usage)) => emit(&critique_report(
            &label,
            &text,
            &usage,
            handle.catalog_billed(),
            format,
        )),
        Err(e @ CritiqueError::Timeout(_)) => fail(chain(&e), 1),
        Err(e) => fail(format!("the critique failed: {}", chain(&e)), 1),
    }
}

fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Asks a numbered question on the terminal. `None` is a declined or unreadable answer.
fn choose(question: &str, options: &[String]) -> Option<usize> {
    eprintln!("{question}");
    for (i, option) in options.iter().enumerate() {
        eprintln!("  {}. {option}", i + 1);
    }
    eprint!("choice [1-{}, anything else declines] ", options.len());
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    let n: usize = answer.trim().parse().ok()?;
    (1..=options.len()).contains(&n).then(|| n - 1)
}

/// What the composition step did, for the report.
pub struct CompositionReport {
    /// `skipped`, `offered_unanswered`, `declined`, `written` or `failed`.
    pub status: &'static str,
    pub lines: Vec<String>,
    /// The `<account>/<model>` pairs the step found.
    pub candidates: Vec<String>,
}

impl CompositionReport {
    fn skipped(lines: Vec<String>) -> CompositionReport {
        CompositionReport {
            status: "skipped",
            lines,
            candidates: Vec::new(),
        }
    }
}

async fn composition_step(
    cli: &Cli,
    args: &SetupArgs,
    (config, dirs, host): (&Config, &Dirs, &Host),
) -> CompositionReport {
    let reason = if args.no_composition {
        "--no-composition"
    } else if composition_offer::existing(config) {
        "a composition is already configured"
    } else if profile(cli, config).0 == Profile::Minimal {
        "the minimal profile"
    } else {
        ""
    };
    if !reason.is_empty() {
        return CompositionReport::skipped(vec![format!(
            "guardian composition: skipped ({reason})"
        )]);
    }
    let ollama = composition_offer::ollama_accounts(config);
    let listings = if ollama.is_empty() {
        Vec::new()
    } else {
        Switcher::new(
            config.clone(),
            dirs.data.clone(),
            crate::shared_env(host),
            None,
        )
        .listings(None, Some(&ollama))
        .await
    };
    let found = composition_offer::detect(&listings);
    if found.is_empty() {
        return CompositionReport::skipped(vec![composition_offer::nothing_to_offer()]);
    }
    let candidates: Vec<String> = found
        .iter()
        .map(|c| format!("{}/{}", c.account, c.benchmark.tag))
        .collect();
    if !std::io::stdin().is_terminal() {
        return CompositionReport {
            status: "offered_unanswered",
            lines: vec![format!(
                "guardian composition: available on {}; run `fontaine auto-mode setup` in a terminal to set it up",
                candidates.join(", ")
            )],
            candidates,
        };
    }
    let declined = |candidates: Vec<String>| CompositionReport {
        status: "declined",
        lines: vec!["guardian composition: not set up".to_owned()],
        candidates,
    };
    let postures = [Posture::Safe, Posture::Fast];
    let labels: Vec<String> = postures.iter().map(|p| p.describe().to_owned()).collect();
    let Some(posture) = choose("Set up the guardian's allow screen?", &labels).map(|i| postures[i])
    else {
        return declined(candidates);
    };
    let pick = if found.len() == 1 {
        Some(0)
    } else {
        choose("Which account runs the screen?", &candidates)
    };
    let Some(pick) = pick else {
        return declined(candidates);
    };
    let offer = Offer {
        account: found[pick].account.clone(),
        benchmark: found[pick].benchmark,
        posture,
    };
    eprintln!("\nsetup would write to your user settings:");
    for line in offer.proposal() {
        eprintln!("  {line}");
    }
    if !confirm("Write it?") {
        return declined(candidates);
    }
    let file = dirs.config.join(fontaine_core::config::USER_FILE);
    match composition_offer::write(&file, config, &offer) {
        Ok(()) => {
            let mut lines = vec![format!(
                "guardian composition: wrote the {} screen on {} to {}",
                match posture {
                    Posture::Safe => "safe",
                    Posture::Fast => "fast",
                },
                offer.model(),
                file.display()
            )];
            lines.extend(composition_offer::calibration_note(&offer));
            CompositionReport {
                status: "written",
                lines,
                candidates,
            }
        }
        Err(e) => CompositionReport {
            status: "failed",
            lines: vec![format!("guardian composition: not written: {}", chain(&e))],
            candidates,
        },
    }
}

async fn scan_step(
    cli: &Cli,
    args: &SetupArgs,
    (config, dirs, host): (&Config, &Dirs, &Host),
    found: &discover::Discovery,
) -> ScanStep {
    let gate = project_env::scan_gate(
        args.no_model,
        std::io::stdin().is_terminal(),
        trust::is_trusted(&dirs.data, &config.workspace),
    );
    if let Err(why) = gate {
        return ScanStep::without(ScanStatus::Skipped(why));
    }
    let resolved = models::resolve(&ResolveParams {
        config,
        data_dir: &dirs.data,
        env: &*host.env,
        model_override: cli.model.as_deref(),
        session_id: None,
    })
    .await;
    let models = match resolved {
        Ok(models) => models,
        Err(e) => return ScanStep::without(ScanStatus::Failed(chain(&e))),
    };
    print_warnings(&models.notices);
    let handle = match project_env::scan_model(models.guardian.as_ref()) {
        Ok(handle) => handle,
        Err(status) => return ScanStep::without(status),
    };
    let label = handle.reference.to_string();
    let sample = sample::gather(&found.repo_root, &found.settings);
    eprintln!("fontaine: {}", project_env::scan_notice(&label));
    let settings = project_env::scan_settings(handle, config.guardian.timeout_secs);
    let proposals = match project_env::propose(&handle.completion, &label, &settings, &sample).await
    {
        Ok(proposals) => proposals,
        Err(e) => return ScanStep::without(ScanStatus::Failed(chain(&e))),
    };
    let (mut step, review) = ScanStep::proposed(proposals, &label, handle.catalog_billed());
    if let Some((scan, count)) = review {
        eprintln!("\n{}\n", sample::review_text(&scan));
        let accepted = confirm(&format!("Save these {count} proposals?"));
        step.decide(scan, count, accepted);
    }
    step
}

/// The report of a setup run.
pub fn setup_report(
    record: &ProjectEnvironment,
    saved: &std::path::Path,
    fallbacks: &[String],
    step: &ScanStepReport<'_>,
    composition: &CompositionReport,
    format: ReportFormat,
) -> String {
    match format {
        ReportFormat::Text => {
            let mut out = String::new();
            for line in
                project_env::report_lines(record, fallbacks, step.status, step.dropped, step.usage)
            {
                out.push_str(&line);
                out.push('\n');
            }
            out.push_str(&format!("saved to {}", saved.display()));
            for line in &composition.lines {
                out.push('\n');
                out.push_str(line);
            }
            out
        }
        ReportFormat::Json => json_text(&json!({
            "saved_to": saved.display().to_string(),
            "record": record,
            "fallbacks": fallbacks,
            "scan": {
                "status": step.status.line(),
                "dropped": step.dropped,
                "usage": step.usage,
            },
            "composition": {
                "status": composition.status,
                "candidates": composition.candidates,
            },
        })),
    }
}

pub struct ScanStepReport<'a> {
    pub status: &'a ScanStatus,
    pub dropped: &'a [String],
    pub usage: Option<&'a str>,
}

fn setup_run(cli: &Cli, args: &SetupArgs, config: &Config, dirs: &Dirs, host: &Host) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(format!("cannot start the async runtime: {e}"), 1),
    };
    runtime.block_on(async {
        let root = &config.workspace;
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
        let roots = workspace::resolve(root, &host.cwd, added).roots;
        let remotes = ledger::git_remotes_of(&roots);
        let found = discover::discover(&SystemGh, root, &remotes, audit::now_rfc3339()).await;
        let step = scan_step(cli, args, (config, dirs, host), &found).await;
        let record = step.apply_to(found.record, &dirs.data, root);
        let composition = composition_step(cli, args, (config, dirs, host)).await;
        let saved = match project_env::store(&dirs.data, root, &record) {
            Ok(saved) => saved,
            Err(e) => {
                return fail(
                    format!("cannot save the project environment: {}", chain(&e)),
                    1,
                );
            }
        };
        emit(&setup_report(
            &record,
            &saved,
            &found.fallbacks,
            &ScanStepReport {
                status: &step.status,
                dropped: &step.dropped,
                usage: step.usage.as_deref(),
            },
            &composition,
            args.format,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_guardian::rules::DEFAULT_HARD_DENY;

    fn config_with(f: impl FnOnce(&mut Config)) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let (launch, config_dir, data) = (
            dir.path().join("work"),
            dir.path().join("config"),
            dir.path().join("data"),
        );
        std::fs::create_dir_all(&launch).unwrap();
        let env = |_: &str| None;
        let mut config = fontaine_core::config::load(&fontaine_core::config::LoadParams {
            launch_dir: &launch,
            config_dir: &config_dir,
            data_dir: &data,
            settings_file: None,
            cli_rules: fontaine_core::config::CliRules {
                allow: &[],
                deny: &[],
            },
            env: &env,
        })
        .unwrap();
        f(&mut config);
        config
    }

    #[test]
    fn defaults_text_is_a_pasteable_toml_fragment() {
        let text = defaults(ReportFormat::Text);
        let parsed: toml::Table = toml::from_str(&text).expect("the output is TOML");
        let section = parsed["auto_mode"].as_table().unwrap();
        for list in List::ALL {
            let entries: Vec<&str> = section[list.name()]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e.as_str().unwrap())
                .collect();
            assert_eq!(entries, list.defaults(), "{}", list.name());
        }
    }

    #[test]
    fn defaults_json_has_the_four_lists() {
        let value: Value = serde_json::from_str(&defaults(ReportFormat::Json)).unwrap();
        assert_eq!(
            value["hard_deny"].as_array().unwrap().len(),
            DEFAULT_HARD_DENY.len()
        );
        for list in List::ALL {
            assert!(value[list.name()].is_array());
        }
    }

    #[test]
    fn config_text_tags_entries_and_names_the_scopes() {
        let config = config_with(|c| {
            c.auto_mode.soft_deny = vec!["never\ttouch \x1b[31m/data".into()];
            c.sources.insert(
                "auto_mode.soft_deny".into(),
                vec![Slot::User, Slot::ProjectLocal],
            );
        });
        let text = config_report(&config, Profile::Full, ReportFormat::Text, None);
        assert!(
            text.contains("auto_mode.soft_deny  (user, project-local)"),
            "{text}"
        );
        assert!(
            text.contains("auto_mode.allow  (shipped defaults)"),
            "{text}"
        );
        assert!(
            text.contains("  [custom ] never\\ttouch \\u{1b}[31m/data"),
            "{text}"
        );
        assert!(text.contains("  [default] Git Destructive"), "{text}");
        assert!(!text.contains('\x1b'));
        assert!(text.contains("always keeps the shipped entries"));
        assert!(text.contains("replaces the shipped entries unless it carries \"$defaults\""));
        assert!(!text.starts_with("note:"));
    }

    #[test]
    fn config_text_notes_the_minimal_profile() {
        let text = config_report(
            &config_with(|_| {}),
            Profile::Minimal,
            ReportFormat::Text,
            None,
        );
        assert!(text.starts_with("note: the minimal profile"), "{text}");
    }

    #[test]
    fn config_json_mirrors_the_text() {
        let config = config_with(|c| {
            c.auto_mode.allow = vec!["$defaults".into(), "mine\nsecond line".into()];
            c.sources
                .insert("auto_mode.allow".into(), vec![Slot::Project]);
        });
        let value: Value = serde_json::from_str(&config_report(
            &config,
            Profile::Full,
            ReportFormat::Json,
            None,
        ))
        .unwrap();
        assert_eq!(value["profile"], "full");
        assert_eq!(value["rules_sent"], true);
        assert_eq!(value["allow"]["scopes"], json!(["project"]));
        assert_eq!(value["environment"]["scopes"], json!([]));
        let entries = value["allow"]["entries"].as_array().unwrap();
        assert_eq!(entries.last().unwrap()["origin"], "custom");
        assert_eq!(entries.last().unwrap()["text"], "mine\nsecond line");
        assert_eq!(entries[0]["origin"], "default");

        let minimal: Value = serde_json::from_str(&config_report(
            &config,
            Profile::Minimal,
            ReportFormat::Json,
            None,
        ))
        .unwrap();
        assert_eq!(minimal["rules_sent"], false);
    }

    #[test]
    fn critique_reports_carry_the_model_and_the_usage() {
        let mut usage = Usage::new();
        usage.input_tokens = Some(10);
        usage.output_tokens = Some(5);
        let text = critique_report("a/m", "Looks vague.", &usage, true, ReportFormat::Text);
        assert_eq!(
            text,
            "Looks vague.\n\nmodel: a/m (10 input tokens, 5 output tokens)"
        );
        let value: Value = serde_json::from_str(&critique_report(
            "a/m",
            "Looks vague.",
            &usage,
            false,
            ReportFormat::Json,
        ))
        .unwrap();
        assert_eq!(value["critique"], "Looks vague.");
        assert_eq!(value["usage"]["input_tokens"], 10);
        assert_eq!(value["usage"]["cost_usd"], Value::Null);
    }

    #[test]
    fn a_skipped_critique_says_why() {
        assert_eq!(
            skipped_report(NOTHING_TO_CRITIQUE, ReportFormat::Text),
            "nothing to critique: no custom entries"
        );
        let value: Value =
            serde_json::from_str(&skipped_report(MINIMAL_NOTE, ReportFormat::Json)).unwrap();
        assert_eq!(value["critique"], Value::Null);
    }

    #[test]
    fn config_text_and_json_show_the_stored_project_environment() {
        let record: ProjectEnvironment = serde_json::from_value(json!({
            "version": 1,
            "discovered_at": "2026-10-30T00:00:00.000Z",
            "remotes": [{
                "url": "https://github.com/acme/proj.git", "slug": "acme/proj",
                "visibility": "public", "default_branch": "main",
                "default_branch_protection": "protected",
                "branch_rules": ["pull_request"], "deploy_environments": [],
            }],
            "tag_release_workflows": [".github/workflows/release.yml"],
            "workflow_environments": [], "package_registries": [],
            "env_files": [], "stack": ["rust"], "scan": null,
        }))
        .unwrap();
        let config = config_with(|_| {});
        let text = config_report(&config, Profile::Full, ReportFormat::Text, Some(&record));
        assert!(
            text.contains("project_environment  (collected 2026-10-30"),
            "{text}"
        );
        assert!(text.contains("visibility public (GitHub API)"), "{text}");
        assert!(text.contains("tag-push release workflows: .github/workflows/release.yml"));
        let none = config_report(&config, Profile::Full, ReportFormat::Text, None);
        assert!(none.ends_with("project_environment: none — run fontaine auto-mode setup"));
        let value: Value = serde_json::from_str(&config_report(
            &config,
            Profile::Full,
            ReportFormat::Json,
            Some(&record),
        ))
        .unwrap();
        assert_eq!(
            value["project_environment"]["remotes"][0]["visibility"],
            "public"
        );
    }

    #[test]
    fn the_setup_report_names_the_scan_outcome_and_where_it_saved() {
        let record: ProjectEnvironment = serde_json::from_value(json!({
            "version": 1, "discovered_at": "2026-10-30T00:00:00.000Z", "remotes": [],
            "tag_release_workflows": [], "workflow_environments": [],
            "package_registries": [], "env_files": [], "stack": [], "scan": null,
        }))
        .unwrap();
        let status = ScanStatus::Skipped("--no-model".into());
        let step = ScanStepReport {
            status: &status,
            dropped: &["deploy_targets \"x\": empty".to_string()],
            usage: None,
        };
        let fallbacks = ["acme/proj: visibility `gh` is not installed".to_string()];
        let text = setup_report(
            &record,
            std::path::Path::new("/d/environment.json"),
            &fallbacks,
            &step,
            &CompositionReport::skipped(Vec::new()),
            ReportFormat::Text,
        );
        assert!(
            text.contains("fell back to unknown: acme/proj: visibility"),
            "{text}"
        );
        assert!(text.contains("model scan: skipped (--no-model)"));
        assert!(text.contains("dropped: deploy_targets"));
        assert!(text.ends_with("saved to /d/environment.json"));
    }
}
