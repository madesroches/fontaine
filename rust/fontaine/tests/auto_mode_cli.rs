mod common;

use clap::Parser;
use common::{Endpoint, Sandbox, Turn, Wire, write};
use fontaine::cli::{AutoModeAction, Cli, Command, ReportFormat};
use fontaine_guardian::List;
use serde_json::Value;

async fn setup() -> (Sandbox, Endpoint) {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("Entry one is vague.")]).await;
    let sandbox = Sandbox::compatible(&endpoint.url());
    (sandbox, endpoint)
}

fn json(out: &common::Output) -> Value {
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

#[test]
fn global_flags_before_the_subcommand_parse() {
    let cli = Cli::try_parse_from(["fontaine", "--cd", "/x", "auto-mode", "config"]).unwrap();
    assert_eq!(cli.cd.as_deref(), Some(std::path::Path::new("/x")));
    let Some(Command::AutoMode(args)) = cli.command else {
        panic!("not the subcommand");
    };
    let AutoModeAction::Config(f) = args.action else {
        panic!("not config");
    };
    assert_eq!(f.format, ReportFormat::Text);
}

#[test]
fn a_prompt_equal_to_the_subcommand_name_runs_the_subcommand_unless_escaped() {
    let cli = Cli::try_parse_from(["fontaine", "-p", "auto-mode", "defaults"]).unwrap();
    assert!(matches!(cli.command, Some(Command::AutoMode(_))));

    let cli = Cli::try_parse_from(["fontaine", "-p", "--", "auto-mode"]).unwrap();
    assert!(cli.command.is_none());
    assert_eq!(cli.prompt.as_deref(), Some("auto-mode"));
}

#[tokio::test(flavor = "multi_thread")]
async fn defaults_prints_the_shipped_lists_and_needs_no_config() {
    let s = Sandbox::new();
    let out = s.run(&["auto-mode", "defaults", "--format", "json"]).await;
    let value = json(&out);
    for list in List::ALL {
        let printed: Vec<&str> = value[list.name()]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert_eq!(printed, list.defaults(), "{}", list.name());
    }

    let text = s.run(&["auto-mode", "defaults"]).await;
    assert_eq!(text.code, Some(0), "{}", text.stderr);
    assert!(text.stdout.starts_with("[auto_mode]\nenvironment = ["));
}

#[tokio::test(flavor = "multi_thread")]
async fn config_shows_the_merged_entries_and_the_scopes_that_set_them() {
    let (s, _endpoint) = setup().await;
    let url = s.config.join("config.toml");
    let base = std::fs::read_to_string(&url).unwrap();
    s.user_config(&format!(
        "{base}\n[auto_mode]\nallow = [\"$defaults\", \"deploys to staging are fine\"]\n"
    ));
    write(
        &s.work.join(".fontaine/settings.toml"),
        "[auto_mode]\nhard_deny = [\"never print the staging token\"]\n",
    );
    let value = json(&s.run(&["auto-mode", "config", "--format", "json"]).await);

    assert_eq!(value["allow"]["scopes"], serde_json::json!(["user"]));
    assert_eq!(value["hard_deny"]["scopes"], serde_json::json!(["project"]));
    assert_eq!(value["environment"]["scopes"], serde_json::json!([]));
    let allow = value["allow"]["entries"].as_array().unwrap();
    assert_eq!(allow.last().unwrap()["text"], "deploys to staging are fine");
    assert_eq!(allow.last().unwrap()["origin"], "custom");
    let hard = value["hard_deny"]["entries"].as_array().unwrap();
    assert_eq!(hard.len(), List::HardDeny.defaults().len() + 1);
    assert_eq!(hard.last().unwrap()["origin"], "custom");
    assert_eq!(hard[0]["origin"], "default");

    let text = s.run(&["auto-mode", "config"]).await;
    assert!(
        text.stdout.contains("auto_mode.allow  (user)"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout.contains("auto_mode.hard_deny  (project)"),
        "{}",
        text.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn critique_without_custom_entries_exits_zero_and_calls_nothing() {
    let (s, endpoint) = setup().await;
    let out = s.run(&["auto-mode", "critique"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout.contains("nothing to critique"), "{}", out.stdout);
    assert!(endpoint.requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn critique_sends_the_custom_entries_and_prints_the_reply() {
    let (s, endpoint) = setup().await;
    let base = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!(
        "{base}\n[auto_mode]\nsoft_deny = [\"touching the vault directory\"]\n"
    ));
    let out = s.run(&["auto-mode", "critique", "--format", "json"]).await;
    let value = json(&out);
    assert_eq!(value["critique"], "Entry one is vague.");
    assert_eq!(value["model"], "mock/scripted");
    assert!(out.stderr.contains("mock/scripted"), "{}", out.stderr);

    let sent = endpoint.requests().await;
    assert_eq!(sent.len(), 1);
    assert!(sent[0].to_string().contains("touching the vault directory"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_after_the_escape_stays_a_prompt() {
    let (s, _endpoint) = setup().await;
    let out = s.headless(&["--", "auto-mode"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout.contains("Entry one is vague."), "{}", out.stdout);
}
