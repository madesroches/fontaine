//! The choke point over core's registry assembly, the one place the built-ins, the subagent
//! and skill tools and the MCP tools meet: every tool is registered wrapped, with a risk class,
//! and nothing an MCP server declares changes either.

mod common;

#[path = "../../fontaine-tools/tests/mcp_common/server.rs"]
mod server;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::pipeline::Rig;
use fontaine_agents::registry::AgentTypes;
use fontaine_agents::root_ledger::{AgentEvent, RootLedger};
use fontaine_agents::runner::{ChildHandle, ChildRunner, SpawnRequest};
use fontaine_agents::spawn::{AgentTool, SendMessageTool};
use fontaine_agents::tools::{BoardShowTool, BoardUpdateTool};
use fontaine_agents::tree::{AgentTree, Limits};
use fontaine_core::config::PermissionMode::{self, Manual};
use fontaine_core::memory_tool::MemoryTool;
use fontaine_core::questions::HumanQuestions;
use fontaine_core::tier::Tier;
use fontaine_core::tools::{ExtensionTool, ExtensionTools, RegistryParams, registry};
use fontaine_tools::advisor_tool::{AdvisorSource, AdvisorTool};
use fontaine_tools::ask_user::{AskUserTool, Question, QuestionReply, QuestionSource};
use fontaine_tools::bug_draft::{DraftArgs, DraftBugReportTool, DraftSink};
use fontaine_tools::descriptor::{TOOLS, find};
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use fontaine_tools::guarded::Guard;
use fontaine_tools::mcp::{DeferMode, Deferral, LoadedSets, McpOptions, connect_transport};
use fontaine_tools::models_tool::{ModelSource, ModelsTool};
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::{RiskClass, registered_risk};
use fontaine_tools::schedule::{
    CronCreateTool, CronDeleteTool, CronJobsTool, JobInfo, NewJob, ScheduleError, ScheduleStore,
    ScheduleWakeupTool, WakeOutcome, WakeRequest,
};
use fontaine_tools::skill::{SkillSource, SkillTool};
use fontaine_tools::todo::{TodoError, TodoItem, TodoStore, TodoTool};
use fontaine_tools::tool_search::ToolSearchTool;
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
    fn load<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no skills".to_string()) })
    }
}

struct NoModels;

impl ModelSource for NoModels {
    fn report<'a>(&'a self, _: Option<&'a str>) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no models".to_string()) })
    }
}

struct NoAdvisor;

impl AdvisorSource for NoAdvisor {
    fn consult<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no advisor".to_string()) })
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

struct NoSchedule;

impl ScheduleStore for NoSchedule {
    fn create(&self, _: NewJob) -> Result<JobInfo, ScheduleError> {
        Err(ScheduleError::TooMany)
    }

    fn list(&self) -> Vec<JobInfo> {
        Vec::new()
    }

    fn delete(&self, _: &str) -> Result<bool, ScheduleError> {
        Ok(false)
    }

    fn wake(&self, _: WakeRequest) -> Result<WakeOutcome, ScheduleError> {
        Err(ScheduleError::NoLoop)
    }
}

struct NoDrafts;

impl DraftSink for NoDrafts {
    fn save(&self, _: &DraftArgs) -> Result<String, String> {
        Err("no drafts".to_string())
    }
}

fn extension() -> ExtensionTools {
    extension_asking(AskUserTool {
        source: Arc::new(NoQuestions),
    })
}

fn extension_asking(ask_user: AskUserTool) -> ExtensionTools {
    extension_memory(ask_user, true)
}

fn extension_memory(ask_user: AskUserTool, with_memory: bool) -> ExtensionTools {
    let tree = Arc::new(AgentTree::new(Limits::default(), 1));
    let mut tools = vec![
        ExtensionTool::new(AgentTool {
            tree: tree.clone(),
            types: Arc::new(fontaine_tools::live::Live::new(AgentTypes::builtin())),
            runner: Arc::new(NoRunner),
            ledger: Arc::new(NoLedger),
            roles: String::new(),
        }),
        ExtensionTool::new(SendMessageTool {
            tree: tree.clone(),
            ledger: Arc::new(NoLedger),
        }),
        ExtensionTool::new(BoardShowTool { tree: tree.clone() }),
        ExtensionTool::new(BoardUpdateTool {
            tree,
            ledger: Arc::new(NoLedger),
        }),
        ExtensionTool::new(SkillTool {
            source: Arc::new(NoSkills),
        }),
        ExtensionTool::new(ask_user),
        ExtensionTool::new(ModelsTool {
            source: Arc::new(NoModels),
        }),
        ExtensionTool::new(ToolSearchTool {
            deferral: Default::default(),
            loaded: Default::default(),
            reveal_names: false,
        }),
        ExtensionTool::new(DraftBugReportTool {
            sink: Arc::new(NoDrafts),
        }),
        ExtensionTool::new(AdvisorTool {
            source: Arc::new(NoAdvisor),
        }),
        ExtensionTool::new(TodoTool {
            store: Arc::new(NoTodos),
        }),
        ExtensionTool::new(CronCreateTool {
            store: Arc::new(NoSchedule),
        }),
        ExtensionTool::new(CronJobsTool {
            store: Arc::new(NoSchedule),
        }),
        ExtensionTool::new(CronDeleteTool {
            store: Arc::new(NoSchedule),
        }),
        ExtensionTool::new(ScheduleWakeupTool {
            store: Arc::new(NoSchedule),
        }),
    ];
    if with_memory {
        tools.push(ExtensionTool::new(MemoryTool {
            root: std::env::temp_dir().join("fontaine-memory-choke-point"),
        }));
    }
    ExtensionTools(tools)
}

fn params(rig: &Rig) -> RegistryParams {
    RegistryParams {
        gate: rig.pipeline.clone(),
        scope: rig.state.scope(),
        mcp_resources: Arc::new(fontaine_tools::mcp::NoMcpResources),
        mcp_limits: Arc::new(fontaine_tools::mcp::OutputLimits::default()),
        mcp_warn: Arc::new(|_| {}),
        seen: Default::default(),
        scrub_env: Vec::new(),
        set_env: Vec::new(),
        shell_max_timeout_secs: 600,
        git_bash: common::git_bash(),
        powershell: None,
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: false,
        websearch: None,
        searxng: Default::default(),
        jobs: Default::default(),
        shell_cwd: Default::default(),
        scratch: None,
        agent_waits: None,
        agent_kills: None,
        wait_wake: None,
        network: Default::default(),
        extension: Some(extension()),
        pre_images: None,
        kills: Arc::new(fontaine_tools::kills::KillSwitches::new()),
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

    // `websearch` exists only when a backend is configured, `lsp` only when a language server is,
    // `powershell` only when an interpreter was found.
    let expected: BTreeSet<&str> = TOOLS
        .iter()
        .map(|d| d.name)
        .filter(|name| !matches!(*name, "websearch" | "lsp" | "powershell"))
        .collect();
    assert_eq!(unique, expected);
    // The ones beyond the built-ins are exactly these.
    let extras: BTreeSet<&str> = [
        "agent",
        "skill",
        "send_message",
        "board_show",
        "board_update",
        "ask_user_question",
        "todo_write",
        "cron_create",
        "cron_jobs",
        "cron_delete",
        "schedule_wakeup",
        "models",
        "tool_search",
        "draft_bug_report",
        "advisor",
        "memory",
    ]
    .into_iter()
    .collect();
    let built_ins: BTreeSet<&str> = [
        "read",
        "ls",
        "glob",
        "grep",
        "write",
        "edit",
        "notebook_edit",
        "shell",
        "job_output",
        "job_cancel",
        "monitor",
        "webfetch",
        "list_mcp_resources",
        "read_mcp_resource",
    ]
    .into_iter()
    .collect();
    assert_eq!(&expected - &built_ins, extras);
    // Nothing for the model to list, inspect or kill agents with. The two MCP resource tools
    // are exempt: their snake_case names carry `list`, which the gate forbids elsewhere.
    for name in &names {
        if matches!(*name, "list_mcp_resources" | "read_mcp_resource") {
            continue;
        }
        for word in name.split('_') {
            assert!(
                !["list", "status", "kill", "interrupt", "stop"].contains(&word),
                "{name}"
            );
        }
    }
}

fn searxng() -> fontaine_tools::websearch::WebsearchConfig {
    use fontaine_tools::websearch::{BackendConfig, WebsearchConfig};
    WebsearchConfig::new(BackendConfig::Searxng {
        base_url: "http://127.0.0.1:1".parse().unwrap(),
        launch: None,
    })
}

#[test]
fn a_configured_backend_adds_exactly_websearch_with_its_descriptor_class() {
    let rig = Rig::new(Manual);
    let plain: BTreeSet<&str> = registry(params(&rig))
        .unwrap()
        .iter()
        .map(|t| t.name)
        .collect();
    let mut p = params(&rig);
    p.websearch = Some(searxng());
    let tools = registry(p).unwrap();
    let configured: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
    assert_eq!(&configured - &plain, BTreeSet::from(["websearch"]));
    assert!(plain.is_subset(&configured));
    let tool = tools.iter().find(|t| t.name == "websearch").unwrap();
    assert_eq!(Some(tool.risk), registered_risk("websearch"));
    assert_eq!(tool.risk, fontaine_tools::risk::RiskClass::Arbitrary);
}

#[test]
fn a_memory_root_adds_exactly_memory_with_its_descriptor_class() {
    let rig = Rig::new(Manual);
    let ask = AskUserTool {
        source: Arc::new(NoQuestions),
    };
    let mut p = params(&rig);
    p.extension = Some(extension_memory(ask, false));
    let plain: BTreeSet<&str> = registry(p).unwrap().iter().map(|t| t.name).collect();
    assert!(!plain.contains("memory"));

    // With the store the session would have, exactly `memory` joins, at its descriptor class.
    let mut p = params(&rig);
    p.extension = Some(extension());
    let tools = registry(p).unwrap();
    let offered: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
    assert_eq!(&offered - &plain, BTreeSet::from(["memory"]));
    assert!(plain.is_subset(&offered));
    let tool = tools.iter().find(|t| t.name == "memory").unwrap();
    assert_eq!(Some(tool.risk), registered_risk("memory"));
    assert_eq!(tool.risk, RiskClass::WorkspaceWrite);
}

struct RigWorkspace(Arc<fontaine_tools::scope::Scope>);

impl fontaine_tools::lsp::LspWorkspace for RigWorkspace {
    fn scope(&self) -> Arc<fontaine_tools::scope::Scope> {
        self.0.clone()
    }

    fn folders(&self) -> Vec<std::path::PathBuf> {
        self.0.roots().to_vec()
    }
}

fn lsp_tool(rig: &Rig) -> fontaine_tools::lsp::LspTool {
    use fontaine_tools::lsp::{LspServerSpec, LspServers, LspTool};
    let workspace: Arc<dyn fontaine_tools::lsp::LspWorkspace> =
        Arc::new(RigWorkspace(rig.state.scope()));
    let spec = LspServerSpec {
        name: "fake".to_string(),
        command: "no-such-server".to_string(),
        args: Vec::new(),
        env_vars: Vec::new(),
        set_env: Vec::new(),
        extension_to_language: [(".rs".to_string(), "rust".to_string())].into(),
        initialization_options: None,
        settings: None,
        startup_timeout: Duration::from_secs(1),
        shutdown_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(1),
        restart_on_crash: false,
        max_restarts: 0,
        diagnostics: true,
    };
    let claims = [(".rs".to_string(), "fake".to_string())].into();
    let servers = LspServers::new(vec![spec], &claims, workspace.clone(), Arc::new(|_| None));
    LspTool { servers, workspace }
}

#[test]
fn a_configured_language_server_adds_exactly_lsp_with_its_descriptor_class() {
    let rig = Rig::new(Manual);
    let plain: BTreeSet<&str> = registry(params(&rig))
        .unwrap()
        .iter()
        .map(|t| t.name)
        .collect();
    assert!(!plain.contains("lsp"));
    let mut p = params(&rig);
    let mut extension = extension();
    extension.0.push(ExtensionTool::new(lsp_tool(&rig)));
    p.extension = Some(extension);
    let tools = registry(p).unwrap();
    let configured: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
    assert_eq!(&configured - &plain, BTreeSet::from(["lsp"]));
    let tool = tools.iter().find(|t| t.name == "lsp").unwrap();
    assert_eq!(Some(tool.risk), registered_risk("lsp"));
    assert_eq!(tool.risk, RiskClass::ReadOnly);
}

#[test]
fn a_found_interpreter_adds_exactly_powershell_with_its_descriptor_class() {
    use fontaine_tools::powershell::PowerShellSettings;

    let rig = Rig::new(Manual);
    let plain: BTreeSet<&str> = registry(params(&rig))
        .unwrap()
        .iter()
        .map(|t| t.name)
        .collect();
    let mut p = params(&rig);
    p.powershell = Some(PowerShellSettings {
        path: "pwsh.exe".into(),
    });
    let tools = registry(p).unwrap();
    let found: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
    assert_eq!(&found - &plain, BTreeSet::from(["powershell"]));
    assert!(plain.is_subset(&found));
    let tool = tools.iter().find(|t| t.name == "powershell").unwrap();
    assert_eq!(Some(tool.risk), registered_risk("powershell"));
    assert_eq!(tool.risk, fontaine_tools::risk::RiskClass::Arbitrary);
}

#[test]
fn every_registered_tool_has_a_descriptor_row_and_its_class() {
    let rig = Rig::new(Manual);
    let mut p = params(&rig);
    p.websearch = Some(searxng());
    let mut extension = extension();
    extension.0.push(ExtensionTool::new(lsp_tool(&rig)));
    p.extension = Some(extension);
    for tool in registry(p).unwrap() {
        let row = find(tool.name);
        assert!(row.is_some(), "{} has no descriptor row", tool.name);
        assert_eq!(Some(tool.risk), row.map(|d| d.risk), "{}", tool.name);
        assert_eq!(Some(tool.risk), registered_risk(tool.name), "{}", tool.name);
    }
}

#[test]
fn without_the_extension_the_registry_is_the_fourteen_built_ins() {
    let rig = Rig::new(Manual);
    let mut p = params(&rig);
    p.extension = None;
    assert_eq!(registry(p).unwrap().len(), 14);
}

async fn serve(
    double: Double,
    rig: &Rig,
    handle: &rig_agent::tool::server::ToolServerHandle,
) -> fontaine_tools::mcp::McpConnectionHandle {
    serve_deferred(double, rig, handle, Deferral::default())
        .await
        .connection
}

async fn serve_deferred(
    double: Double,
    rig: &Rig,
    handle: &rig_agent::tool::server::ToolServerHandle,
    deferral: Deferral,
) -> fontaine_tools::mcp::Connected {
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
        pre_images: None,
        kills: None,
    };
    let mut options = McpOptions::new(guard, false);
    options.deferral = deferral;
    connect_transport("double", (cfs, c2s), handle.clone(), &options)
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

#[tokio::test]
async fn a_server_served_again_after_its_connection_closed_is_wrapped_and_answers_from_the_new_server()
 {
    let rig = Rig::new(Manual);
    let handle = ToolServer::new().run();
    let first = Double::with_tools(vec![echo_tool(), lookup_tool()]);
    let first_calls = first.calls.clone();
    let connection = serve(first, &rig, &handle).await;
    connection.close().await;

    // What a reconnect does: the same tool server, a new connection, a new wrapping.
    let second = Double::with_tools(vec![echo_tool(), lookup_tool()]);
    let second_calls = second.calls.clone();
    let _again = serve(second, &rig, &handle).await;
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

    // Each re-registered tool reaches the gate, which asks and is denied, so nothing is raw.
    for name in &names {
        assert!(call(&handle, name).await.is_err(), "{name}");
    }
    assert_eq!(rig.approver.asked(), 2);
    for record in rig.audit().iter().filter(|r| r.tool.starts_with("mcp__")) {
        assert!(
            matches!(record.tier, Tier::Mode | Tier::Human),
            "{record:?}"
        );
    }
    assert!(first_calls.lock().unwrap().is_empty());
    assert!(second_calls.lock().unwrap().is_empty());

    rig.approver
        .answer(fontaine_core::permissions::Answer::AllowOnce);
    assert!(call(&handle, "mcp__double__lookup").await.is_ok());
    assert_eq!(
        second_calls.lock().unwrap().len(),
        1,
        "the new server answered"
    );
    assert!(first_calls.lock().unwrap().is_empty(), "not the closed one");
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
        // The human's pick is recorded as their answer, never decided by a mode.
        assert_eq!(
            rig.state.ledger.view().user_messages,
            vec!["Answered the agent's question \"Which?\": picked \"b\" (\"second\")"],
            "{mode:?}"
        );
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

#[tokio::test]
async fn a_deferred_mcp_tool_that_is_not_loaded_still_takes_the_gate() {
    let rig = Rig::new(Manual);
    let handle = ToolServer::new().run();
    let deferral = Deferral::default();
    deferral.set_policy(DeferMode::On, None);
    let double = Double::with_tools(vec![lookup_tool()]);
    let calls = double.calls.clone();
    let _connection = serve_deferred(double, &rig, &handle, deferral.clone()).await;
    assert!(deferral.is_active());
    assert!(deferral.is_registered("mcp__double__lookup"));
    // The tool stays registered and wrapped: deferral only changes what is advertised.
    assert_eq!(
        handle.toolset().names().collect::<Vec<_>>(),
        ["mcp__double__lookup"]
    );

    // Not loaded: a call that reaches the callback by any route still asks the human.
    let denied = call(&handle, "mcp__double__lookup").await.unwrap_err();
    assert!(denied.contains("denied"), "{denied}");
    assert_eq!(rig.approver.asked(), 1);

    assert!(calls.lock().unwrap().is_empty());
    for record in rig
        .audit()
        .iter()
        .filter(|r| r.tool == "mcp__double__lookup")
    {
        assert!(
            matches!(record.tier, Tier::Mode | Tier::Human),
            "{record:?}"
        );
    }
    // The ask records its decision and the human's answer.
    assert!(
        rig.audit()
            .iter()
            .filter(|r| r.tool == "mcp__double__lookup")
            .count()
            >= 1
    );

    rig.approver
        .answer(fontaine_core::permissions::Answer::AllowOnce);
    assert!(call(&handle, "mcp__double__lookup").await.is_ok());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_list_changed_refresh_updates_the_deferred_names_and_drops_vanished_loaded_tools() {
    let rig = Rig::new(Manual);
    let handle = ToolServer::new().run();
    let deferral = Deferral::default();
    deferral.set_policy(DeferMode::On, None);
    let double = Double::with_tools(vec![echo_tool(), lookup_tool()]);
    let _connection = serve_deferred(double.clone(), &rig, &handle, deferral.clone()).await;
    let sets = LoadedSets::default();
    sets.load(
        ROOT_AGENT,
        [
            "mcp__double__echo".to_string(),
            "mcp__double__lookup".to_string(),
        ],
    );
    assert_eq!(sets.loaded(ROOT_AGENT, &deferral).len(), 2);

    double.replace_tools(vec![lookup_tool()]).await;
    for _ in 0..200 {
        if !deferral.is_registered("mcp__double__echo") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!deferral.is_registered("mcp__double__echo"));
    assert_eq!(sets.loaded(ROOT_AGENT, &deferral), ["mcp__double__lookup"]);
    assert_eq!(
        deferral.names_by_server(&|_| true),
        [(
            "double".to_string(),
            vec!["mcp__double__lookup".to_string()]
        )]
    );
}

#[tokio::test]
async fn a_closed_connection_takes_its_tools_out_of_the_deferral_registry() {
    let rig = Rig::new(Manual);
    let handle = ToolServer::new().run();
    let deferral = Deferral::default();
    let connected = serve_deferred(
        Double::with_tools(vec![echo_tool()]),
        &rig,
        &handle,
        deferral.clone(),
    )
    .await;
    assert!(deferral.is_registered("mcp__double__echo"));
    connected.mute();
    assert!(!deferral.is_registered("mcp__double__echo"));
}
