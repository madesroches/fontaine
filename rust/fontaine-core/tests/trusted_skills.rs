//! A skill the user trusted: which of its `allowed-tools` count as allows, which only reach the
//! guardian as context, and what a changed file does to both.

mod common;

use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use common::pipeline::{Fake, Options, Rig, allow_verdict, allowed, fake_guardian};
use common::write;
use fontaine_core::config::PermissionMode::{self, Auto, Manual, Plan};
use fontaine_core::extensions::{self, Extensions};
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_core::plugins::{self, PluginContext, build_plugin, checkout_dir};
use fontaine_core::skill_trust::{self, TrustRecord, TrustState};
use fontaine_core::skills::SkillRuntime;
use fontaine_core::tier::Tier;
use fontaine_tools::gate::ROOT_AGENT;
use futures::future::BoxFuture;
use serde_json::{Value, json};

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn shell(command: &str) -> Value {
    json!({"command": command})
}

#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Approver for Warnings {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

const PLUGIN_SKILL: &str = "p:pr";

/// A plugin `p` with one skill `pr` whose frontmatter declares `tools`.
fn plugin_skill_text(tools: &str) -> String {
    format!(
        "---\ndescription: opens a pull request\nallowed-tools: {tools}\n---\nRebase, then open the PR.\n"
    )
}

struct Setup {
    rig: Rig,
    fake: Fake,
    warnings: Arc<Warnings>,
}

fn setup(mode: PermissionMode, settings: &str) -> Setup {
    let fake = fake_guardian(allow_verdict());
    let warnings = Arc::new(Warnings::default());
    let mut options = Options::new(mode)
        .guardian(fake.guardian.clone())
        .settings(settings);
    options.approver = Some(warnings.clone());
    Setup {
        rig: Rig::with(options),
        fake,
        warnings,
    }
}

fn install_plugin_skill(rig: &Rig, text: &str) -> std::path::PathBuf {
    let root = checkout_dir(&rig.world.config, "team", "p");
    write(&root.join(".claude-plugin/plugin.json"), r#"{"name": "p"}"#);
    let file = root.join("skills/pr/SKILL.md");
    write(&file, text);
    file
}

fn trust_text(rig: &Rig, name: &str, text: &str) {
    skill_trust::trust(
        &rig.world.config,
        vec![TrustRecord {
            name: name.to_string(),
            digest: skill_trust::digest_of(text.as_bytes()),
            tools: Vec::new(),
            commands: Vec::new(),
        }],
    )
    .unwrap();
}

fn load(rig: &Rig) -> (Extensions, Vec<String>) {
    let root = checkout_dir(&rig.world.config, "team", "p");
    let dirs: Vec<plugins::PluginDir> = if root.is_dir() {
        let ctx = PluginContext {
            project_root: &rig.world.work,
            env: &no_env,
        };
        vec![build_plugin(&root, "p", &ctx).unwrap().dir]
    } else {
        Vec::new()
    };
    let mut warnings = Vec::new();
    let extensions = extensions::load(&mut extensions::LoadParams {
        config_dir: &rig.world.config,
        project_root: &rig.world.work,
        servers: &[],
        plugins: &dirs,
        warnings: &mut warnings,
    })
    .unwrap();
    (extensions, warnings)
}

fn runtime(rig: &Rig) -> SkillRuntime {
    SkillRuntime::new(Arc::new(load(rig).0), rig.state.clone(), true)
}

fn last_tier(rig: &Rig) -> Tier {
    rig.audit().last().unwrap().tier
}

fn guardian_saw_skill(fake: &Fake) -> bool {
    fake.mock
        .requests()
        .iter()
        .any(|request| format!("{request:?}").contains("active_skill: p:pr (trusted by the user"))
}

#[tokio::test]
async fn a_trusted_plugin_skills_specific_rule_is_an_allow_for_the_invoking_agent_only() {
    let s = setup(Auto, "");
    let text = plugin_skill_text("Bash(git status), Read");
    install_plugin_skill(&s.rig, &text);
    trust_text(&s.rig, PLUGIN_SKILL, &text);
    let runtime = runtime(&s.rig);
    runtime
        .expand(PLUGIN_SKILL, "", ROOT_AGENT, false)
        .await
        .unwrap();

    assert!(allowed(&s.rig.check("shell", shell("git status")).await));
    assert_eq!(last_tier(&s.rig), Tier::AllowRule);
    assert_eq!(s.fake.calls(), 0, "an allow rule never asks the guardian");

    let child = s
        .rig
        .agents
        .reserve(
            ROOT_AGENT,
            "worker",
            "m",
            vec!["shell".into()],
            fontaine_agents::mode::Ladder::Auto,
        )
        .unwrap();
    s.rig.check_as(&child, "shell", shell("git status")).await;
    assert_ne!(last_tier(&s.rig), Tier::AllowRule);

    s.rig.state.end_turn();
    s.rig.check("shell", shell("git status")).await;
    assert_ne!(last_tier(&s.rig), Tier::AllowRule);
}

#[tokio::test]
async fn an_untrusted_or_stale_plugin_skill_lends_nothing_and_warns_once() {
    let s = setup(Manual, "");
    let text = plugin_skill_text("Bash(git status)");
    install_plugin_skill(&s.rig, &text);
    let runtime = runtime(&s.rig);
    for _ in 0..2 {
        runtime
            .expand(PLUGIN_SKILL, "", ROOT_AGENT, false)
            .await
            .unwrap();
    }
    assert!(s.rig.state.skill_grants(ROOT_AGENT).is_empty());
    let told = s.warnings.0.lock().unwrap().clone();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("p:pr") && told[0].contains("not trusted"),
        "{told:?}"
    );

    // Trusted at one version, edited since.
    let s = setup(Manual, "");
    install_plugin_skill(&s.rig, &text);
    trust_text(&s.rig, PLUGIN_SKILL, &text);
    install_plugin_skill(
        &s.rig,
        &plugin_skill_text("Bash(git status), Bash(rm file)"),
    );
    let (extensions, warnings) = load(&s.rig);
    assert_eq!(
        extensions.trust_state(extensions.skill(PLUGIN_SKILL).unwrap()),
        TrustState::Stale
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("/skills trust p:pr"), "{warnings:?}");
    let runtime = SkillRuntime::new(Arc::new(extensions), s.rig.state.clone(), true);
    runtime
        .expand(PLUGIN_SKILL, "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert!(s.rig.state.skill_grants(ROOT_AGENT).is_empty());
    assert!(s.warnings.0.lock().unwrap()[0].contains("changed since you trusted it"));
}

#[tokio::test]
async fn wildcard_rules_are_context_for_the_guardian_never_an_allow() {
    for (mode, tools) in [
        (Auto, "Bash(git *)"),
        (Plan, "Bash(git *)"),
        (Auto, "Bash"),
        (Manual, "Bash(git *)"),
        (Plan, "PowerShell"),
        (Manual, "PowerShell"),
        (Plan, "Agent"),
        (Manual, "Agent"),
    ] {
        let s = setup(mode, "");
        let text = plugin_skill_text(tools);
        install_plugin_skill(&s.rig, &text);
        trust_text(&s.rig, PLUGIN_SKILL, &text);
        let runtime = runtime(&s.rig);
        runtime
            .expand(PLUGIN_SKILL, "", ROOT_AGENT, false)
            .await
            .unwrap();

        assert!(
            s.rig.state.skill_grants(ROOT_AGENT).is_empty(),
            "{mode:?} {tools}"
        );
        let notes = s.rig.state.trusted_skill_notes(ROOT_AGENT);
        assert_eq!(notes.len(), 1, "{mode:?} {tools}");
        assert_eq!(notes[0].name, PLUGIN_SKILL);
        if tools == "Bash" {
            assert_eq!(notes[0].declared, ["*"]);
        }

        for command in ["git rebase origin/main", "git push --force origin main"] {
            s.rig.approver.answer(Answer::Deny { feedback: None });
            s.rig.check("shell", shell(command)).await;
            assert_ne!(
                last_tier(&s.rig),
                Tier::AllowRule,
                "{mode:?} {tools} {command}"
            );
        }
        if mode == Auto {
            assert!(s.fake.calls() >= 2, "{tools}: the guardian judged both");
            assert!(
                guardian_saw_skill(&s.fake),
                "{tools}: the request names the skill"
            );
        }

        s.rig.state.end_turn();
        assert!(s.rig.state.trusted_skill_notes(ROOT_AGENT).is_empty());
    }
}

#[tokio::test]
async fn a_subagent_of_the_invoking_agent_carries_no_active_skill() {
    let s = setup(Auto, "");
    let text = plugin_skill_text("Bash(git *)");
    install_plugin_skill(&s.rig, &text);
    trust_text(&s.rig, PLUGIN_SKILL, &text);
    runtime(&s.rig)
        .expand(PLUGIN_SKILL, "", ROOT_AGENT, false)
        .await
        .unwrap();
    let child = s
        .rig
        .agents
        .reserve(
            ROOT_AGENT,
            "worker",
            "m",
            vec!["shell".into()],
            fontaine_agents::mode::Ladder::Auto,
        )
        .unwrap();
    let args = shell("git rebase origin/main");
    let request = |agent: &str| {
        fontaine_core::adjudication::build(&s.rig.state, agent, "shell", &args, Vec::new(), None)
    };
    assert_eq!(request(ROOT_AGENT).session_facts.active_skills.len(), 1);
    assert!(request(&child).session_facts.active_skills.is_empty());
}

#[tokio::test]
async fn deny_rules_and_the_floor_still_win_over_a_trusted_skills_allow() {
    let s = setup(Auto, "[permissions]\ndeny = [\"shell(git status)\"]\n");
    let text = plugin_skill_text("Bash(git status)");
    install_plugin_skill(&s.rig, &text);
    trust_text(&s.rig, PLUGIN_SKILL, &text);
    runtime(&s.rig)
        .expand(PLUGIN_SKILL, "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert!(!allowed(&s.rig.check("shell", shell("git status")).await));
    assert_eq!(last_tier(&s.rig), Tier::DenyRule);
}

#[tokio::test]
async fn a_users_own_wildcard_rule_stays_an_allow_in_auto() {
    let s = setup(Auto, "[permissions]\nallow = [\"shell(git *)\"]\n");
    assert!(allowed(
        &s.rig.check("shell", shell("git rebase origin/main")).await
    ));
    assert_eq!(last_tier(&s.rig), Tier::AllowRule);
    assert_eq!(s.fake.calls(), 0);
}

fn user_skill(rig: &Rig, text: &str) {
    write(&rig.world.config.join("skills/mine/SKILL.md"), text);
}

#[tokio::test]
async fn a_user_skill_keeps_lending_every_rule_and_a_trusted_one_adds_the_note() {
    let text = "---\ndescription: m\nallowed-tools: Bash(git *), Bash(git status)\n---\nbody\n";

    // Untrusted: all rules, no note.
    let s = setup(Manual, "");
    user_skill(&s.rig, text);
    runtime(&s.rig)
        .expand("mine", "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert_eq!(s.rig.state.skill_grants(ROOT_AGENT).len(), 2);
    assert!(s.rig.state.trusted_skill_notes(ROOT_AGENT).is_empty());

    // Trusted: all rules, plus the note for the wildcard one.
    let s = setup(Manual, "");
    user_skill(&s.rig, text);
    trust_text(&s.rig, "mine", text);
    runtime(&s.rig)
        .expand("mine", "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert_eq!(s.rig.state.skill_grants(ROOT_AGENT).len(), 2);
    let notes = s.rig.state.trusted_skill_notes(ROOT_AGENT);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].declared, ["git *"]);

    // Stale: back to the untrusted behaviour, with a notice at load.
    let s = setup(Manual, "");
    user_skill(&s.rig, &format!("{text}more"));
    trust_text(&s.rig, "mine", text);
    let (extensions, warnings) = load(&s.rig);
    assert!(
        warnings[0].contains("trust note for the guardian is off"),
        "{warnings:?}"
    );
    SkillRuntime::new(Arc::new(extensions), s.rig.state.clone(), true)
        .expand("mine", "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert_eq!(s.rig.state.skill_grants(ROOT_AGENT).len(), 2);
    assert!(s.rig.state.trusted_skill_notes(ROOT_AGENT).is_empty());
}

#[test]
fn a_project_skill_is_never_trusted_and_its_record_warns() {
    let s = setup(Manual, "");
    let text = "---\ndescription: p\nallowed-tools: Bash(git status)\n---\nbody\n";
    write(
        &s.rig.world.work.join(".fontaine/skills/proj/SKILL.md"),
        text,
    );
    trust_text(&s.rig, "proj", text);
    let (extensions, warnings) = load(&s.rig);
    let skill = extensions.skill("proj").unwrap();
    assert_eq!(extensions.trust_state(skill), TrustState::Untrusted);
    assert!(skill.allowed_tools.is_empty());
    assert!(
        warnings.iter().any(|w| w.contains("project skill")),
        "{warnings:?}"
    );
}

#[test]
fn a_digest_follows_the_body_and_the_allowed_tools() {
    let s = setup(Manual, "");
    let digest = |text: &str| {
        install_plugin_skill(&s.rig, text);
        load(&s.rig).0.skill(PLUGIN_SKILL).unwrap().digest.clone()
    };
    let base = digest(&plugin_skill_text("Bash(git status)"));
    assert_eq!(base, digest(&plugin_skill_text("Bash(git status)")));
    assert_ne!(base, digest(&plugin_skill_text("Bash(git diff)")));
    let edited = plugin_skill_text("Bash(git status)").replace("Rebase", "Merge");
    assert_ne!(base, digest(&edited));
}

#[test]
fn a_plugin_skill_keeps_its_declared_tools_but_lends_none() {
    let s = setup(Manual, "");
    install_plugin_skill(&s.rig, &plugin_skill_text("Bash(git status), Read"));
    let (extensions, _) = load(&s.rig);
    let skill = extensions.skill(PLUGIN_SKILL).unwrap();
    assert_eq!(skill.declared_tools.len(), 2);
    assert!(skill.allowed_tools.is_empty());
}
