//! `fontaine auto-mode`: shows the rules the guardian judges against. Nothing here starts a
//! session, runs a project executable or changes a setting.

use std::process::ExitCode;

use fontaine_core::config::{Config, Slot};
use fontaine_core::containment;
use fontaine_core::dirs::Dirs;
use fontaine_core::models::{self, GuardianModel, ResolveParams};
use fontaine_guardian::critique::{self, CritiqueError};
use fontaine_guardian::rules::{DEFAULTS_MARKER, has_custom, lists};
use fontaine_guardian::{EnvironmentConfig, List, Profile, RuleConfig};
use fontaine_tools::display::escape_controls;
use fontaine_tools::error_chain::chain;
use rig_core::completion::Usage;
use serde_json::{Value, json};

use crate::cli::{AutoModeAction, AutoModeArgs, Cli, ReportFormat};
use crate::{EXIT_CONFIG, Host, print_warnings};

const NOTHING_TO_CRITIQUE: &str = "nothing to critique: no custom entries";
const MINIMAL_NOTE: &str = "the minimal profile's prompt carries a reduced ruleset of its own and the rule lists (allow, soft_deny, hard_deny) are not sent to the guardian; only the environment list is";

pub fn run(cli: &Cli, args: &AutoModeArgs, config: &Config, dirs: &Dirs, host: &Host) -> ExitCode {
    match &args.action {
        AutoModeAction::Defaults(f) => emit(&defaults(f.format)),
        AutoModeAction::Config(f) => {
            let (profile, warnings) = profile(cli, config);
            print_warnings(&warnings);
            emit(&config_report(config, profile, f.format))
        }
        AutoModeAction::Critique(f) => critique_run(cli, config, dirs, host, f.format),
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

/// The effective lists with where each came from.
pub fn config_report(config: &Config, profile: Profile, format: ReportFormat) -> String {
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
            out.trim_end().to_string()
        }
    }
}

fn usage_line(usage: &Usage, billed: bool) -> String {
    let tokens = |n: Option<u64>| n.map_or("unknown".to_string(), |n| n.to_string());
    let mut line = format!(
        "{} input tokens, {} output tokens",
        tokens(usage.input_tokens),
        tokens(usage.output_tokens)
    );
    if let Some(cost) = usage.cost.filter(|_| billed) {
        line.push_str(&format!(", ${:.4}", cost.total));
    }
    line
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
        ReportFormat::Text => format!("{text}\n\nmodel: {model} ({})", usage_line(usage, billed)),
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
        let text = config_report(&config, Profile::Full, ReportFormat::Text);
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
        let text = config_report(&config_with(|_| {}), Profile::Minimal, ReportFormat::Text);
        assert!(text.starts_with("note: the minimal profile"), "{text}");
    }

    #[test]
    fn config_json_mirrors_the_text() {
        let config = config_with(|c| {
            c.auto_mode.allow = vec!["$defaults".into(), "mine\nsecond line".into()];
            c.sources
                .insert("auto_mode.allow".into(), vec![Slot::Project]);
        });
        let value: Value =
            serde_json::from_str(&config_report(&config, Profile::Full, ReportFormat::Json))
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
}
