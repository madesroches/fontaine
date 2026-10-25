//! Skills, agent files and output styles: Claude Code's frontmatter, what a repository's files
//! may not do, and what running a skill does to the permission pipeline.

mod common;

use std::sync::Arc;

use common::pipeline::{Options, Rig, allowed};
use common::{World, write};
use fontaine_agents::mode::ModeSetting;
use fontaine_agents::registry::TypeOrigin;
use fontaine_core::agent::build_agent_types;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::extensions::{Extensions, LoadParams, Scope, load};
use fontaine_core::permissions::Answer;
use fontaine_core::skills::SkillRuntime;
use fontaine_core::tier::Tier;
use fontaine_core::tools::{RegistryParams, registry};
use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::skill::SkillSource;
use serde_json::json;

fn load_for(
    w: &World,
    servers: &[&str],
) -> Result<Extensions, fontaine_core::extensions::ExtensionError> {
    let servers: Vec<String> = servers.iter().map(|s| s.to_string()).collect();
    load(&LoadParams {
        config_dir: &w.config,
        project_root: &w.work,
        servers: &servers,
    })
}

fn user_skill(w: &World, name: &str, text: &str) {
    write(&w.config.join(format!("skills/{name}/SKILL.md")), text);
}

fn project_skill(w: &World, name: &str, text: &str) {
    write(
        &w.work.join(format!(".fontaine/skills/{name}/SKILL.md")),
        text,
    );
}

fn user_agent(w: &World, name: &str, text: &str) {
    write(&w.config.join(format!("agents/{name}.md")), text);
}

fn project_agent(w: &World, name: &str, text: &str) {
    write(&w.work.join(format!(".fontaine/agents/{name}.md")), text);
}

fn message(w: &World, servers: &[&str]) -> String {
    load_for(w, servers).unwrap_err().to_string()
}

#[test]
fn claude_code_style_skills_and_agents_load_by_copying() {
    let w = World::new();
    user_skill(
        &w,
        "review",
        "---\nname: review\ndescription: Review a diff\nallowed-tools: Bash(git diff *), Read\nargument-hint: [pr-number]\nmodel: inherit\ncontext: fork\n---\nLook at $ARGUMENTS\n",
    );
    user_agent(
        &w,
        "researcher",
        "---\nname: researcher\ndescription: Digs through code\ntools: Read, Grep, Glob\ndisallowedTools: Bash\nmodel: inherit\npermissionMode: default\ncolor: blue\n---\nYou dig.\n",
    );
    let ext = load_for(&w, &[]).unwrap();
    let skill = ext.skill("review").unwrap();
    assert_eq!(skill.description, "Review a diff");
    assert_eq!(skill.argument_hint.as_deref(), Some("[pr-number]"));
    assert_eq!(skill.allowed_tools.len(), 2);
    assert_eq!(skill.allowed_tools[0].to_string(), "shell(git diff *)");
    assert_eq!(skill.allowed_tools[1].to_string(), "read");
    assert!(skill.user_invocable && !skill.disable_model_invocation);

    let agent = ext
        .agent_types
        .iter()
        .find(|a| a.name == "researcher")
        .unwrap();
    assert_eq!(
        agent.tools.as_deref(),
        Some(&["read".to_string(), "grep".to_string(), "glob".to_string()][..])
    );
    assert_eq!(agent.disallowed, ["shell"]);
    assert_eq!(agent.model, None);
    assert_eq!(agent.mode, ModeSetting::Manual);
    assert_eq!(agent.prompt.as_deref(), Some("You dig."));
}

#[test]
fn tool_lists_load_as_yaml_lists_and_as_comma_strings() {
    let w = World::new();
    user_agent(&w, "a", "---\ntools:\n  - Read\n  - Bash(git *)\n---\nx");
    user_agent(&w, "b", "---\ntools: Read, Bash(git *)\n---\nx");
    let ext = load_for(&w, &[]).unwrap();
    let tools = |name: &str| {
        ext.agent_types
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .tools
            .clone()
            .unwrap()
    };
    assert_eq!(tools("a"), ["read", "shell(git *)"]);
    assert_eq!(tools("b"), ["read", "shell(git *)"]);
}

#[test]
fn unmapped_names_colon_specifiers_and_model_aliases_are_errors_naming_the_file() {
    let w = World::new();
    user_agent(&w, "bad", "---\ntools: Read, TodoWrite\n---\nx");
    let e = message(&w, &[]);
    assert!(e.contains("bad.md") && e.contains("TodoWrite"), "{e}");

    let w = World::new();
    user_skill(&w, "colon", "---\nallowed-tools: Bash(git add:*)\n---\nx");
    let e = message(&w, &[]);
    assert!(e.contains("SKILL.md") && e.contains("allowed-tools"), "{e}");

    let w = World::new();
    user_agent(&w, "alias", "---\nmodel: sonnet\n---\nx");
    let e = message(&w, &[]);
    assert!(e.contains("alias.md") && e.contains("sonnet"), "{e}");

    let w = World::new();
    user_agent(&w, "ok", "---\nmodel: work/claude-sonnet\n---\nx");
    assert!(load_for(&w, &[]).is_ok());
}

#[test]
fn executables_and_permission_settings_in_a_file_are_load_errors() {
    for (kind, text) in [
        ("agent", "---\nhooks:\n  PreToolUse:\n    - a: b\n---\nx"),
        ("agent", "---\nmcpServers:\n  gh:\n    command: x\n---\nx"),
        ("skill", "---\nhooks:\n  PreToolUse:\n    - a: b\n---\nx"),
        ("skill", "---\nmcpServers:\n  gh:\n    command: x\n---\nx"),
        ("skill", "---\npermissionMode: auto\n---\nx"),
    ] {
        let w = World::new();
        if kind == "agent" {
            user_agent(&w, "f", text);
        } else {
            user_skill(&w, "f", text);
        }
        let e = message(&w, &[]);
        let file = if kind == "agent" { "f.md" } else { "SKILL.md" };
        assert!(e.contains(file), "{e}");
    }
}

#[test]
fn unknown_frontmatter_keys_are_ignored() {
    let w = World::new();
    user_skill(
        &w,
        "s",
        "---\ncolor: red\nversion: 2\nlicense: MIT\n---\nbody",
    );
    user_agent(&w, "a", "---\ncolor: red\nmaxTurns: 3\n---\nbody");
    assert!(load_for(&w, &[]).is_ok());
}

#[test]
fn user_agent_files_accept_only_inherit_default_and_dont_ask() {
    for mode in ["acceptEdits", "auto", "plan", "bypassPermissions"] {
        let w = World::new();
        user_agent(&w, "a", &format!("---\npermissionMode: {mode}\n---\nx"));
        let e = message(&w, &[]);
        assert!(e.contains("a.md") && e.contains(mode), "{e}");
    }
    for (mode, expected) in [
        ("inherit", ModeSetting::Inherit),
        ("default", ModeSetting::Manual),
        ("dontAsk", ModeSetting::DontAsk),
    ] {
        let w = World::new();
        user_agent(&w, "a", &format!("---\npermissionMode: {mode}\n---\nx"));
        let ext = load_for(&w, &[]).unwrap();
        assert_eq!(ext.agent_types[0].mode, expected);
    }
}

#[test]
fn a_project_agent_file_can_only_narrow() {
    // A model, an inherited or looser mode, and a tool the session lacks are all rejected.
    for (text, needle) in [
        ("---\nmodel: work/claude\n---\nx", "model"),
        ("---\npermissionMode: inherit\n---\nx", "inherit"),
        ("---\npermissionMode: auto\n---\nx", "auto"),
        ("---\npermissionMode: acceptEdits\n---\nx", "acceptEdits"),
        ("---\ntools: Read, mcp__ghost__tool\n---\nx", "ghost"),
    ] {
        let w = World::new();
        project_agent(&w, "p", text);
        let e = message(&w, &[]);
        assert!(e.contains("p.md") && e.contains(needle), "{needle}: {e}");
    }
    // Inherit written as the absence of a model is fine, and a tool of a configured server is
    // one the parent has.
    let w = World::new();
    project_agent(
        &w,
        "p",
        "---\nmodel: inherit\ntools: Read, Grep, mcp__github__create_issue\n---\nx",
    );
    let ext = load_for(&w, &["github"]).unwrap();
    let agent = &ext.agent_types[0];
    assert_eq!(agent.mode, ModeSetting::Manual, "the default is manual");
    assert!(matches!(agent.origin, TypeOrigin::ProjectFile(_)));
    assert_eq!(agent.model, None);
    let w = World::new();
    project_agent(&w, "p", "---\npermissionMode: dontAsk\n---\nx");
    assert_eq!(
        load_for(&w, &[]).unwrap().agent_types[0].mode,
        ModeSetting::DontAsk
    );
}

#[test]
fn a_project_agent_overrides_a_same_name_user_agent_and_stays_narrowed() {
    let w = World::new();
    user_agent(
        &w,
        "builder",
        "---\ntools: Read, Edit, Write, Bash\nmodel: work/big\npermissionMode: inherit\n---\nuser version",
    );
    project_agent(&w, "builder", "---\ntools: Read\n---\nproject version");
    w.user_file("[agents.types.other]\ntools = [\"read\"]\n");
    let ext = load_for(&w, &[]).unwrap();
    let config = w.load().unwrap();
    let types = build_agent_types(&config, &ext);
    let builder = types.get("builder").unwrap();
    assert_eq!(builder.prompt.as_deref(), Some("project version"));
    assert_eq!(builder.model, None);
    assert_eq!(builder.mode, ModeSetting::Manual);
    assert_eq!(
        builder.toolset(&["read".to_string(), "edit".to_string(), "shell".to_string()]),
        ["read"]
    );
    assert!(types.get("other").is_some());
    assert!(types.get("explore").is_some());
}

#[test]
fn a_project_skills_allowed_tools_are_ignored_but_still_checked() {
    let w = World::new();
    project_skill(&w, "p", "---\nallowed-tools: Bash(git *)\n---\nbody");
    let ext = load_for(&w, &[]).unwrap();
    assert_eq!(ext.skill("p").unwrap().scope, Scope::Project);
    assert!(ext.skill("p").unwrap().allowed_tools.is_empty());

    let w = World::new();
    project_skill(&w, "p", "---\nallowed-tools: Bash(git add:*)\n---\nbody");
    assert!(load_for(&w, &[]).is_err());
}

#[test]
fn a_personal_skill_outranks_a_project_one_of_the_same_name() {
    let w = World::new();
    user_skill(&w, "same", "---\ndescription: mine\n---\nuser body");
    project_skill(&w, "same", "---\ndescription: theirs\n---\nproject body");
    let ext = load_for(&w, &[]).unwrap();
    assert_eq!(ext.skill("same").unwrap().description, "mine");
}

#[test]
fn user_invocable_false_and_disable_model_invocation_are_read() {
    let w = World::new();
    user_skill(&w, "hidden", "---\nuser-invocable: false\n---\nbody");
    user_skill(
        &w,
        "manual",
        "---\ndisable-model-invocation: true\n---\nbody",
    );
    let ext = load_for(&w, &[]).unwrap();
    assert!(!ext.skill("hidden").unwrap().user_invocable);
    assert!(ext.skill("manual").unwrap().disable_model_invocation);
    let index = fontaine_core::skills::index(&ext).unwrap();
    assert!(index.contains("hidden"), "{index}");
    assert!(!index.contains("manual"), "{index}");
}

#[test]
fn output_styles_load_from_both_scopes_with_the_project_overriding() {
    let w = World::new();
    write(
        &w.config.join("output-styles/terse.md"),
        "---\ndescription: short\n---\nBe brief.",
    );
    write(
        &w.work.join(".fontaine/output-styles/terse.md"),
        "---\ndescription: project\n---\nBe very brief.",
    );
    let ext = load_for(&w, &[]).unwrap();
    assert_eq!(ext.output_style("terse").unwrap().body, "Be very brief.");
}

/// A pipeline over the real registry, and the skills running through it.
struct Skills {
    rig: Rig,
    runtime: SkillRuntime,
}

fn skills(w_setup: impl FnOnce(&World)) -> Skills {
    let rig = Rig::with(Options::new(Manual));
    w_setup(&rig.world);
    let ext = Arc::new(load_for(&rig.world, &[]).unwrap());
    let runtime = SkillRuntime::new(ext, rig.state.clone(), true);
    let tools = registry(RegistryParams {
        gate: rig.pipeline.clone(),
        scope: rig.state.scope(),
        seen: Default::default(),
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        git_bash: common::git_bash(),
        results: Arc::new(fontaine_tools::result_pipeline::ResultPipeline::new()),
        hide_mcp_names: false,
        network: Default::default(),
        extension: None,
    })
    .unwrap();
    let shell = tools.into_iter().find(|t| t.name == "shell").unwrap();
    runtime.set_shell(shell.tool);
    Skills { rig, runtime }
}

#[tokio::test]
async fn a_skills_command_runs_through_the_pipeline_as_a_shell_call() {
    let s = skills(|w| {
        user_skill(
            w,
            "status",
            "---\ndescription: s\n---\nOutput: !`echo hello-from-skill`\n",
        )
    });
    s.rig.approver.answer(Answer::AllowOnce);
    let text = s
        .runtime
        .expand("status", "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert_eq!(text, "Output: hello-from-skill");
    assert_eq!(
        s.rig.approver.asked(),
        1,
        "the command was put to the human"
    );
    let record = s
        .rig
        .audit()
        .into_iter()
        .find(|r| r.tool == "shell")
        .unwrap();
    assert_eq!(record.tier, Tier::Mode);
    assert_eq!(record.agent_id, ROOT_AGENT);
}

#[tokio::test]
async fn a_subagent_without_a_shell_cannot_run_a_skills_commands() {
    let s = skills(|w| {
        user_skill(
            w,
            "status",
            "---\ndescription: s\n---\nOutput: !`echo hello`\n",
        )
    });
    let child = s
        .rig
        .state
        .agents()
        .reserve(
            ROOT_AGENT,
            "reader",
            "m",
            vec!["read".into(), "skill".into()],
            fontaine_agents::mode::Ladder::Manual,
            false,
        )
        .unwrap();
    let text = s.runtime.expand("status", "", &child, false).await.unwrap();
    assert!(text.contains("[command not run:"), "{text}");
    assert_eq!(s.rig.approver.asked(), 0);
}

#[tokio::test]
async fn a_denied_skill_command_is_replaced_by_the_denial_and_the_skill_still_loads() {
    let s = skills(|w| {
        user_skill(
            w,
            "status",
            "---\ndescription: s\n---\nOutput: !`echo hi`\n",
        )
    });
    let text = s
        .runtime
        .expand("status", "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert!(text.contains("[command failed:"), "{text}");
    assert!(!text.contains("Output: hi"));
}

#[tokio::test]
async fn arguments_are_substituted_before_the_commands_run() {
    let s = skills(|w| {
        user_skill(
            w,
            "greet",
            "---\ndescription: g\n---\n!`echo $0`\nand $ARGUMENTS[1]\n",
        )
    });
    s.rig.approver.answer(Answer::AllowOnce);
    let text = s
        .runtime
        .expand("greet", "alpha \"b c\"", ROOT_AGENT, false)
        .await
        .unwrap();
    assert_eq!(text, "alpha\nand b c");
    let asked = s.rig.approver.last_request();
    assert_eq!(asked.args["command"], json!("echo alpha"));
}

#[tokio::test]
async fn the_skill_tool_cannot_load_a_skill_only_the_user_may_run() {
    let s = skills(|w| {
        user_skill(
            w,
            "deploy",
            "---\ndisable-model-invocation: true\n---\nship it",
        );
        user_skill(w, "plain", "---\ndescription: p\n---\nhello");
    });
    assert_eq!(s.runtime.load("plain", ROOT_AGENT).await.unwrap(), "hello");
    let refused = s.runtime.load("deploy", ROOT_AGENT).await.unwrap_err();
    assert!(refused.contains("for the user"), "{refused}");
    // The user's own command works.
    assert_eq!(
        s.runtime
            .expand("deploy", "", ROOT_AGENT, false)
            .await
            .unwrap(),
        "ship it"
    );
    let unknown = s.runtime.load("nope", ROOT_AGENT).await.unwrap_err();
    assert!(unknown.contains("plain"), "{unknown}");
}

#[tokio::test]
async fn a_skills_allowed_tools_last_to_the_end_of_the_turn_for_the_invoking_agent_only() {
    let s = skills(|w| {
        user_skill(
            w,
            "tester",
            "---\ndescription: t\nallowed-tools: Bash(cargo test)\n---\nrun the tests",
        )
    });
    let rig = &s.rig;
    // Before the skill runs, the command needs the human.
    rig.check("shell", json!({"command": "cargo test"})).await;
    assert_eq!(rig.approver.asked(), 1);

    // `/tester` and the `skill` tool lend the same rule.
    s.runtime
        .expand("tester", "", ROOT_AGENT, false)
        .await
        .unwrap();
    assert!(allowed(
        &rig.check("shell", json!({"command": "cargo test"})).await
    ));
    assert_eq!(rig.audit().last().unwrap().tier, Tier::AllowRule);
    assert!(
        rig.audit()
            .last()
            .unwrap()
            .rule
            .as_deref()
            .unwrap()
            .contains("[skill]")
    );
    // The rule is exact: other commands still ask.
    rig.check("shell", json!({"command": "cargo build"})).await;
    assert_eq!(rig.approver.asked(), 2);

    // A subagent the invoking agent spawns does not hold it.
    let child = rig
        .agents
        .reserve(
            ROOT_AGENT,
            "worker",
            "m",
            vec!["shell".into()],
            fontaine_agents::mode::Ladder::Manual,
            false,
        )
        .unwrap();
    rig.check_as(&child, "shell", json!({"command": "cargo test"}))
        .await;
    assert_eq!(rig.approver.asked(), 3);

    // The turn ends, and so does the loan.
    rig.state.end_turn();
    rig.check("shell", json!({"command": "cargo test"})).await;
    assert_eq!(rig.approver.asked(), 4);

    // An interrupted turn drops its future; the loan ends all the same.
    s.runtime.load("tester", ROOT_AGENT).await.unwrap();
    drop(fontaine_core::turn::EndTurnOnDrop(&rig.state));
    assert!(rig.state.skill_grants(ROOT_AGENT).is_empty());

    // By the `skill` tool, the invoking agent gets it too.
    s.runtime.load("tester", ROOT_AGENT).await.unwrap();
    assert!(allowed(
        &rig.check("shell", json!({"command": "cargo test"})).await
    ));
}

#[tokio::test]
async fn a_project_skills_commands_do_not_run_when_the_workspace_is_not_trusted() {
    let rig = Rig::with(Options::new(Manual));
    project_skill(
        &rig.world,
        "p",
        "---\ndescription: p\n---\n!`echo should-not-run`",
    );
    let ext = Arc::new(load_for(&rig.world, &[]).unwrap());
    let runtime = SkillRuntime::new(ext, rig.state.clone(), false);
    let text = runtime.expand("p", "", ROOT_AGENT, false).await.unwrap();
    assert!(text.contains("not trusted"), "{text}");
    assert_eq!(rig.approver.asked(), 0);
}

#[tokio::test]
async fn a_user_typed_skill_is_not_attributed_to_the_previous_tool_call() {
    let w = World::new();
    user_skill(&w, "review", "---\ndescription: Review\n---\nbody\n");
    let built = common::runtime::build(&w, Manual, Vec::new(), true);
    let state = &built.runtime.state;
    state.set_current_call(fontaine_tools::gate::ROOT_AGENT, "stale-call".to_string());
    built.runtime.expand_skill("review", "").await.unwrap();
    assert_eq!(state.current_call(fontaine_tools::gate::ROOT_AGENT), None);
}
