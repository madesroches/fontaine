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
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::tier::Tier;
use fontaine_core::tools::{ExtensionTools, RegistryParams, registry};
use fontaine_mcp::connect_transport;
use fontaine_tools::guarded::Guard;
use fontaine_tools::names::BUILTIN_TOOL_NAMES;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::{BUILTIN_RISK, EXTENSION_RISK, RiskClass, registered_risk};
use fontaine_tools::skill::{SkillSource, SkillTool};
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

fn extension() -> ExtensionTools {
    let tree = Arc::new(AgentTree::new(Limits::default()));
    ExtensionTools {
        agent: AgentTool {
            tree: tree.clone(),
            types: Arc::new(AgentTypes::builtin()),
            runner: Arc::new(NoRunner),
            ledger: Arc::new(NoLedger),
        },
        send_message: SendMessageTool {
            tree,
            ledger: Arc::new(NoLedger),
        },
        skill: SkillTool {
            source: Arc::new(NoSkills),
        },
    }
}

fn params(rig: &Rig) -> RegistryParams {
    RegistryParams {
        gate: rig.pipeline.clone(),
        scope: rig.state.scope(),
        seen: Default::default(),
        scrub_env: Vec::new(),
        git_bash: None,
        results: Arc::new(ResultPipeline::new()),
        hide_mcp_names: false,
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

    let expected: BTreeSet<&str> = BUILTIN_RISK
        .iter()
        .chain(EXTENSION_RISK.iter())
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(unique, expected);
    // The three beyond the built-ins are exactly these.
    assert_eq!(
        EXTENSION_RISK,
        [
            ("agent", RiskClass::Arbitrary),
            ("skill", RiskClass::ReadOnly),
            ("send_message", RiskClass::Arbitrary),
        ]
    );
    // The non-MCP names are the names rules can use.
    let known: BTreeSet<&str> = BUILTIN_TOOL_NAMES.iter().copied().collect();
    assert_eq!(unique, known);
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
fn every_registered_tool_has_its_risk_class_from_the_tables() {
    let rig = Rig::new(Manual);
    for tool in registry(params(&rig)).unwrap() {
        let class = registered_risk(tool.name);
        assert!(class.is_some(), "{} has no risk class", tool.name);
        assert_eq!(Some(tool.risk), class, "{}", tool.name);
    }
}

#[test]
fn without_the_extension_the_registry_is_the_eight_built_ins() {
    let rig = Rig::new(Manual);
    let mut p = params(&rig);
    p.extension = None;
    assert_eq!(registry(p).unwrap().len(), BUILTIN_RISK.len());
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
        .execute(name, "{}", &mut ToolContext::new())
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
