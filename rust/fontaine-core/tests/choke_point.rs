//! The choke point over core's registry assembly, the one place the built-ins, the subagent
//! and skill tools and the MCP tools meet: every tool is registered wrapped, with a risk class,
//! and nothing an MCP server declares changes either.

mod common;

#[path = "../../fontaine-mcp/tests/common/server.rs"]
mod server;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::pipeline::Rig;
use fontaine_agents::registry::AgentTypes;
use fontaine_agents::root_ledger::{AgentEvent, RootLedger};
use fontaine_agents::runner::{ChildHandle, ChildRunner, SpawnRequest};
use fontaine_agents::spawn::{AgentTool, SendMessageTool};
use fontaine_agents::tree::{AgentTree, Limits};
use fontaine_core::config::PermissionMode::{self, Manual};
use fontaine_core::questions::HumanQuestions;
use fontaine_core::tier::Tier;
use fontaine_core::tools::{ExtensionTool, ExtensionTools, RegistryParams, registry};
use fontaine_mcp::connect_transport;
use fontaine_tools::ask_user::{AskUserTool, Question, QuestionReply, QuestionSource};
use fontaine_tools::descriptor::{TOOLS, find};
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::{RiskClass, registered_risk};
use fontaine_tools::skill::{SkillSource, SkillTool};
use fontaine_tools::todo::{TodoError, TodoItem, TodoStore, TodoTool};
use futures::future::BoxFuture;
use rig_agent::tool::server::ToolServer;
use rig_core::tool::ToolContext;
use rmcp::ServiceExt;
use server::{Double, echo_tool, lookup_tool};

struct NoRunner;

impl ChildRunner for NoRunner {
    fn spawn<'a>(&'a self, _: SpawnRequest) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>> {
        Box::pin(async { Err("no children in this test".to_string()) })
    }
}

struct NoLedger;

impl RootLedger for NoLedger {
    fn human_messages(&self) -> Vec<String> {
        Vec::new()
    }

    fn append_event(&self, _: AgentEvent) {}
}

struct NoSkills;

impl SkillSource for NoSkills {
    fn load<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no skills".to_string()) })
    }
}

struct NoQuestions;

impl QuestionSource for NoQuestions {
    fn ask<'a>(&'a self, _: &'a str, _: Vec<Question>) -> BoxFuture<'a, QuestionReply> {
        Box::pin(async { QuestionReply::Unavailable })
    }
}

struct NoTodos;

impl TodoStore for NoTodos {
    fn replace(&self, _: &str, _: Vec<TodoItem>) -> Result<(), TodoError> {
        Ok(())
    }
}

fn extension() -> ExtensionTools {
    extension_asking(AskUserTool {
        source: Arc::new(NoQuestions),
    })
}

fn extension_asking(ask_user: AskUserTool) -> ExtensionTools {
    let tree = Arc::new(AgentTree::new(Limits::default()));
    ExtensionTools(vec![
        ExtensionTool::new(AgentTool {
            tree: tree.clone(),
            types: Arc::new(AgentTypes::builtin()),
            runner: Arc::new(NoRunner),
            ledger: Arc::new(NoLedger),
        }),
        ExtensionTool::new(SendMessageTool {
            tree,
            ledger: Arc::new(NoLedger),
        }),
        ExtensionTool::new(SkillTool {
            source: Arc::new(NoSkills),
        }),
        ExtensionTool::new(ask_user),
        ExtensionTool::new(TodoTool {
            store: Arc::new(NoTodos),
        }),
    ])
}

fn params(rig: &Rig) -> RegistryParams {
    RegistryParams {
        gate: rig.pipeline.clone(),
        scope: rig.state.scope(),
        seen: Default::default(),
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        git_bash: None,
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: false,
        network: Default::default(),
        extension: Some(extension()),
    }
}

#[test]
fn the_registered_set_is_the_phase_two_table_plus_exactly_the_extension_tools() {
    let rig = Rig::new(Manual);
    let tools = registry(params(&rig)).unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "duplicate registrations: {names:?}"
    );

    let expected: BTreeSet<&str> = TOOLS.iter().map(|d| d.name).collect();
    assert_eq!(unique, expected);
    // The five beyond the built-ins are exactly these.
    let extras: BTreeSet<&str> = [
        "agent",
        "skill",
        "send_message",
        "ask_user_question",
        "todo_write",
    ]
    .into_iter()
    .collect();
    let built_ins: BTreeSet<&str> = [
        "read", "ls", "glob", "grep", "write", "edit", "shell", "webfetch",
    ]
    .into_iter()
    .collect();
    assert_eq!(&expected - &built_ins, extras);
    // Nothing for the model to list, inspect or kill agents with.
    for name in &names {
        for word in name.split('_') {
            assert!(
                !["list", "status", "kill", "interrupt", "stop"].contains(&word),
                "{name}"
            );
        }
    }
}

#[test]
fn every_registered_tool_has_a_descriptor_row_and_its_class() {
    let rig = Rig::new(Manual);
    for tool in registry(params(&rig)).unwrap() {
        let row = find(tool.name);
        assert!(row.is_some(), "{} has no descriptor row", tool.name);
        assert_eq!(Some(tool.risk), row.map(|d| d.risk), "{}", tool.name);
        assert_eq!(Some(tool.risk), registered_risk(tool.name), "{}", tool.name);
    }
}

#[test]
fn without_the_extension_the_registry_is_the_eight_built_ins() {
    let rig = Rig::new(Manual);
    let mut p = params(&rig);
    p.extension = None;
    assert_eq!(registry(p).unwrap().len(), 8);
}

async fn serve(
    double: Double,
    rig: &Rig,
    handle: &rig_agent::tool::server::ToolServerHandle,
) -> fontaine_mcp::McpConnectionHandle {
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let running = double.serve((sfc, s2c)).await.expect("server start");
        let _ = running.waiting().await;
    });
    let guard = Guard {
        gate: rig.pipeline.clone(),
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: true,
    };
    connect_transport("double", (cfs, c2s), handle.clone(), guard, false)
        .await
        .expect("connect")
}

async fn call(
    handle: &rig_agent::tool::server::ToolServerHandle,
    name: &str,
) -> Result<String, String> {
    match handle
        .toolset()
        .execute(name, "{}", &mut as_root())
        .await
        .into_result()
    {
        Ok(output) => Ok(output.render()),
        Err(e) => Err(e.model_output().render()),
    }
}

#[tokio::test]
async fn an_mcp_tool_that_declares_itself_read_only_still_goes_to_the_human() {
    let rig = Rig::new(Manual);
    let handle = ToolServer::new().run();
    let double = Double::with_tools(vec![lookup_tool()]);
    let calls = double.calls.clone();
    let _connection = serve(double, &rig, &handle).await;

    // Nothing skips the pipeline: manual mode asks, and the scripted approver says no.
    let denied = call(&handle, "mcp__double__lookup").await.unwrap_err();
    assert!(denied.contains("denied"), "{denied}");
    assert_eq!(rig.approver.asked(), 1);
    let record = rig
        .audit()
        .into_iter()
        .find(|r| r.tool == "mcp__double__lookup")
        .unwrap();
    assert_eq!(record.tier, Tier::Mode);
    assert!(
        calls.lock().unwrap().is_empty(),
        "the server never saw the call"
    );

    rig.approver
        .answer(fontaine_core::permissions::Answer::AllowOnce);
    assert!(call(&handle, "mcp__double__lookup").await.is_ok());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn tools_added_or_replaced_by_a_refresh_are_wrapped_and_none_is_reachable_raw() {
    let rig = Rig::new(Manual);
    let handle = ToolServer::new().run();
    let double = Double::with_tools(vec![echo_tool()]);
    let calls = double.calls.clone();
    let _connection = serve(double.clone(), &rig, &handle).await;

    double.replace_tools(vec![lookup_tool(), echo_tool()]).await;
    let mut names: Vec<String> = Vec::new();
    for _ in 0..200 {
        names = handle.toolset().names().map(str::to_owned).collect();
        if names.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    names.sort();
    assert_eq!(names, ["mcp__double__echo", "mcp__double__lookup"]);

    // Every registered tool asks, and the approver (which denies) is the only way through.
    for name in &names {
        assert!(call(&handle, name).await.is_err(), "{name}");
    }
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(rig.approver.asked(), 2);
    for record in rig.audit().iter().filter(|r| r.tool.starts_with("mcp__")) {
        assert!(
            matches!(record.tier, Tier::Mode | Tier::Human),
            "{record:?}"
        );
    }
}

fn as_root() -> ToolContext {
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    context
}

fn one_question() -> serde_json::Value {
    serde_json::json!({ "questions": [{
        "question": "Which?",
        "header": "Pick",
        "options": [
            { "label": "a", "description": "first" },
            { "label": "b", "description": "second" },
        ],
    }] })
}

async fn run_ask(rig: &Rig, context: &mut ToolContext) -> Result<String, String> {
    let mut p = params(rig);
    p.extension = Some(extension_asking(AskUserTool {
        source: Arc::new(HumanQuestions::new(rig.approver.clone(), rig.state.clone())),
    }));
    let tools = registry(p).unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name == "ask_user_question")
        .expect("registered");
    assert_eq!(tool.risk, RiskClass::ReadOnly);
    match tool.tool.run_with(context, one_question()).await {
        Ok(output) => Ok(output.render()),
        Err(e) => Err(e.model_output().render()),
    }
}

#[tokio::test]
async fn no_mode_answers_the_question_for_the_human() {
    for mode in [
        PermissionMode::Manual,
        PermissionMode::AcceptEdits,
        PermissionMode::Plan,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
    ] {
        let rig = Rig::new(mode);
        rig.approver.reply(QuestionReply::Answered(vec![
            fontaine_tools::ask_user::QuestionAnswer {
                question: "Which?".into(),
                selected: vec!["b".into()],
                other: None,
            },
        ]));
        let out = run_ask(&rig, &mut as_root()).await.unwrap();
        assert_eq!(out, "\"Which?\" = \"b\"", "{mode:?}");
        assert_eq!(rig.approver.questions.lock().unwrap().len(), 1, "{mode:?}");
        // The call needed no approval and no guardian verdict.
        assert_eq!(rig.approver.asked(), 0, "{mode:?}");
        let record = rig
            .audit()
            .into_iter()
            .find(|r| r.tool == "ask_user_question")
            .unwrap();
        assert_eq!(record.tier, Tier::ReadOnlyFastPath, "{mode:?}");
        assert!(rig.state.ledger.view().user_messages.is_empty(), "{mode:?}");
    }
}

#[tokio::test]
async fn an_approver_that_cannot_ask_gives_the_unavailable_result() {
    let rig = Rig::new(Manual);
    let out = run_ask(&rig, &mut as_root()).await.unwrap();
    assert_eq!(out, fontaine_tools::ask_user::UNAVAILABLE_TEXT);
    assert!(rig.state.ledger.view().user_messages.is_empty());
}

#[tokio::test]
async fn a_deny_rule_blocks_the_question_before_any_dialog() {
    let settings = "[permissions]\ndeny = [\"ask_user_question\"]\n";
    let rig = Rig::with(common::pipeline::Options::new(Manual).settings(settings));
    rig.approver.reply(QuestionReply::Declined);
    let denied = run_ask(&rig, &mut as_root()).await.unwrap_err();
    assert!(
        denied.contains("denied") || denied.contains("deny"),
        "{denied}"
    );
    assert!(rig.approver.questions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_child_that_inherits_the_tool_is_refused_at_call_time() {
    let rig = Rig::new(Manual);
    rig.approver.reply(QuestionReply::Declined);
    let id = rig
        .agents
        .reserve(
            fontaine_tools::gate::ROOT_AGENT,
            "worker",
            "m",
            Vec::new(),
            fontaine_agents::mode::Ladder::Manual,
            false,
        )
        .unwrap();
    let mut context = ToolContext::new();
    context.insert(AgentId(id)).unwrap();
    let out = run_ask(&rig, &mut context).await;
    assert_eq!(
        out.as_deref(),
        Ok(fontaine_tools::ask_user::UNAVAILABLE_TEXT)
    );
    assert!(rig.approver.questions.lock().unwrap().is_empty());
}
