//! A read-only shell command is held to the agent-type read restrictions, and a `pre_tool_use`
//! hook that names a file read takes it off the pre-classifier fast path.

#[path = "core_common/mod.rs"]
mod common;

use std::sync::Arc;

use common::pipeline::{Options, Rig};
use fontaine_sdk::agents::mode::Ladder;
use fontaine_sdk::core::audit::Decision;
use fontaine_sdk::core::config::PermissionMode::Manual;
use fontaine_sdk::core::gate::ChildRestrictions;
use fontaine_sdk::core::permission_rules::parse_rule;
use fontaine_sdk::core::permissions::{HookDecision, HookError, PreToolUseEvent, PreToolUseHook};
use fontaine_sdk::core::tier::Tier;
use fontaine_sdk::tools::gate::ROOT_AGENT;
use futures::future::BoxFuture;
use serde_json::{Value, json};

struct Matched(Option<&'static str>);

impl PreToolUseHook for Matched {
    fn id(&self) -> String {
        "matched".to_string()
    }

    fn matcher(&self) -> Option<String> {
        self.0.map(String::from)
    }

    fn run<'a>(
        &'a self,
        _event: PreToolUseEvent<'a>,
    ) -> BoxFuture<'a, Result<HookDecision, HookError>> {
        Box::pin(async { Ok(HookDecision::default()) })
    }
}

fn shell(command: &str) -> Value {
    json!({ "command": command })
}

fn rig_with(matcher: Option<Option<&'static str>>, settings: &str) -> Rig {
    let mut options = Options::new(Manual).settings(settings);
    if let Some(matcher) = matcher {
        options.hooks = vec![Arc::new(Matched(matcher)) as Arc<dyn PreToolUseHook>];
    }
    let rig = Rig::with(options);
    for dir in ["src/private", "secrets"] {
        std::fs::create_dir_all(rig.work().join(dir)).unwrap();
    }
    for file in ["src/a.txt", "src/private/x", "secrets/key", ".env"] {
        std::fs::write(rig.work().join(file), "x").unwrap();
    }
    rig
}

async fn outcome(rig: &Rig, agent: &str, command: &str) -> (Decision, Tier, Option<String>) {
    rig.check_as(agent, "shell", shell(command)).await;
    let record = rig.audit().last().unwrap().clone();
    (record.decision, record.tier, record.rule)
}

fn child(rig: &Rig, parent: &str) -> String {
    rig.agents
        .reserve(parent, "worker", "m", Vec::new(), Ladder::Manual)
        .unwrap()
}

fn restrict(rig: &Rig, id: &str, only: &[&str], deny: &[&str]) {
    let rules = |texts: &[&str]| texts.iter().map(|t| parse_rule(t).unwrap()).collect();
    rig.state.set_child_restrictions(
        id,
        ChildRestrictions {
            only: rules(only),
            deny: rules(deny),
        },
    );
}

#[tokio::test]
async fn a_read_restriction_on_the_agent_type_denies_the_shell_read_and_reaches_a_grandchild() {
    let rig = rig_with(None, "");
    let id = child(&rig, ROOT_AGENT);
    restrict(&rig, &id, &[], &["read(secrets/**)"]);
    let denied = (Decision::Deny, Tier::DenyRule, Some("agent tools".into()));
    assert_eq!(outcome(&rig, &id, "cat secrets/key").await, denied);
    assert_eq!(
        outcome(&rig, &id, "cat src/a.txt").await,
        (Decision::Allow, Tier::Tier05, None)
    );
    let grandchild = child(&rig, &id);
    assert_eq!(outcome(&rig, &grandchild, "cat secrets/key").await, denied);
}

#[tokio::test]
async fn an_only_restriction_on_read_denies_a_path_it_does_not_cover() {
    let rig = rig_with(None, "");
    let id = child(&rig, ROOT_AGENT);
    restrict(&rig, &id, &["read(src/**)"], &[]);
    assert_eq!(
        outcome(&rig, &id, "cat secrets/key").await,
        (Decision::Deny, Tier::DenyRule, Some("agent tools".into()))
    );
    assert_eq!(
        outcome(&rig, &id, "cat src/a.txt").await,
        (Decision::Allow, Tier::Tier05, None)
    );
}

#[tokio::test]
async fn an_ls_or_grep_restriction_denies_the_matching_shell_command() {
    let rig = rig_with(None, "");
    let id = child(&rig, ROOT_AGENT);
    restrict(&rig, &id, &[], &["ls(src/private)", "grep(src/private/**)"]);
    let denied = (Decision::Deny, Tier::DenyRule, Some("agent tools".into()));
    assert_eq!(outcome(&rig, &id, "ls src/private").await, denied);
    assert_eq!(outcome(&rig, &id, "grep x src/private/x").await, denied);
    assert_eq!(
        outcome(&rig, &id, "cat src/a.txt").await,
        (Decision::Allow, Tier::Tier05, None)
    );
}

#[tokio::test]
async fn a_hook_naming_a_file_read_takes_shell_reads_off_the_fast_path() {
    for matcher in ["read(src/private/**)", "ls", "grep"] {
        let rig = rig_with(Some(Some(matcher)), "");
        for command in ["cat src/private/x", "cat src/a.txt"] {
            let asked = rig.approver.asked();
            let (_, tier, _) = outcome(&rig, ROOT_AGENT, command).await;
            assert_ne!(tier, Tier::Tier05, "{matcher} {command}");
            assert_eq!(rig.approver.asked(), asked + 1, "{matcher} {command}");
        }
    }
}

#[tokio::test]
async fn a_hook_that_names_no_file_read_leaves_the_fast_path() {
    for matcher in [None, Some("shell"), Some("edit(**)")] {
        let rig = rig_with(Some(matcher), "");
        assert_eq!(
            outcome(&rig, ROOT_AGENT, "cat src/private/x").await,
            (Decision::Allow, Tier::Tier05, None),
            "{matcher:?}"
        );
    }
}

#[tokio::test]
async fn an_allow_rule_still_restores_a_secret_read_under_a_read_hook() {
    let rig = rig_with(
        Some(Some("read(secrets/**)")),
        "[permissions]\nallow = [\"read(.env)\"]\n",
    );
    let (decision, tier, _) = outcome(&rig, ROOT_AGENT, "cat .env").await;
    assert_eq!((decision, tier), (Decision::Allow, Tier::AllowRule));
}
