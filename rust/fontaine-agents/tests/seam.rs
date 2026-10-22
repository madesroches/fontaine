//! The spawn tool, the registry and the budgets, run against a fake `ChildRunner` with no
//! `fontaine-core` in sight: the fake receives what a real runner would and answers through the
//! handle it returns.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_agents::budget::{Budget, Spend};
use fontaine_agents::mode::{Ladder, RootMode};
use fontaine_agents::registry::{AgentType, AgentTypes, ModelSpec, TypeOrigin};
use fontaine_agents::root_ledger::{AgentEvent, RootLedger};
use fontaine_agents::runner::{ChildHandle, ChildReport, ChildRunner, ChildStatus, SpawnRequest};
use fontaine_agents::spawn::{AgentArgs, AgentTool, SendMessageArgs, SendMessageTool};
use fontaine_agents::tree::{AgentTree, Limits};
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use futures::future::BoxFuture;
use rig_core::tool::{Tool, ToolContext};
use tokio::sync::watch;

/// What the fake saw of one spawn.
#[derive(Clone, Debug)]
struct Spawned {
    id: String,
    parent: String,
    agent_type: String,
    model: ModelSpec,
    tools: Vec<String>,
    mode: Ladder,
    budget: Budget,
    depth: usize,
    background: bool,
    prompt: String,
    human: Vec<String>,
}

#[derive(Default)]
struct FakeLedger {
    messages: Vec<String>,
    events: Mutex<Vec<AgentEvent>>,
}

impl RootLedger for FakeLedger {
    fn human_messages(&self) -> Vec<String> {
        self.messages.clone()
    }

    fn append_event(&self, event: AgentEvent) {
        self.events.lock().unwrap().push(event);
    }
}

struct FakeChild {
    status: Mutex<ChildStatus>,
    report: watch::Sender<Option<ChildReport>>,
    killed: Mutex<bool>,
}

impl FakeChild {
    fn finish(&self, status: ChildStatus, text: &str) {
        *self.status.lock().unwrap() = status;
        self.report.send_replace(Some(ChildReport {
            status,
            text: text.to_string(),
            reason: (status == ChildStatus::BudgetExhausted).then(|| "its budget is spent".into()),
        }));
    }
}

impl ChildHandle for FakeChild {
    fn status(&self) -> ChildStatus {
        *self.status.lock().unwrap()
    }

    fn spend(&self) -> Spend {
        Spend::default()
    }

    fn kill(&self) {
        *self.killed.lock().unwrap() = true;
        self.finish(ChildStatus::Killed, "partial");
    }

    fn report(&self) -> BoxFuture<'_, ChildReport> {
        let mut rx = self.report.subscribe();
        Box::pin(async move {
            loop {
                if let Some(report) = rx.borrow().clone() {
                    return report;
                }
                rx.changed().await.unwrap();
            }
        })
    }
}

#[derive(Default)]
struct FakeRunner {
    spawned: Mutex<Vec<Spawned>>,
    children: Mutex<Vec<Arc<FakeChild>>>,
    /// Finish a child at once with this text, instead of leaving it running.
    answer: Mutex<Option<(ChildStatus, String)>>,
    fail: Mutex<Option<String>>,
    /// Never finish starting a child, as when resolving its model takes long.
    hang: Mutex<bool>,
}

impl FakeRunner {
    fn child(&self, at: usize) -> Arc<FakeChild> {
        self.children.lock().unwrap()[at].clone()
    }

    fn spawned(&self) -> Vec<Spawned> {
        self.spawned.lock().unwrap().clone()
    }
}

impl ChildRunner for FakeRunner {
    fn spawn<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>> {
        Box::pin(async move {
            if *self.hang.lock().unwrap() {
                std::future::pending::<()>().await;
            }
            if let Some(why) = self.fail.lock().unwrap().clone() {
                return Err(why);
            }
            self.spawned.lock().unwrap().push(Spawned {
                id: request.id.clone(),
                parent: request.parent.clone(),
                agent_type: request.agent_type.name.clone(),
                model: request.model.clone(),
                tools: request.tools.clone(),
                mode: request.mode,
                budget: request.budget,
                depth: request.depth,
                background: request.background,
                prompt: request.prompt.clone(),
                human: request.ledger.human_messages(),
            });
            let (report, _) = watch::channel(None);
            let child = Arc::new(FakeChild {
                status: Mutex::new(ChildStatus::Running),
                report,
                killed: Mutex::new(false),
            });
            if let Some((status, text)) = self.answer.lock().unwrap().clone() {
                child.finish(status, &text);
            }
            self.children.lock().unwrap().push(child.clone());
            Ok(child as Arc<dyn ChildHandle>)
        })
    }
}

struct Rig {
    tree: Arc<AgentTree>,
    runner: Arc<FakeRunner>,
    ledger: Arc<FakeLedger>,
    agent: AgentTool,
    send: SendMessageTool,
}

fn rig_with(limits: Limits, types: AgentTypes, root_mode: RootMode) -> Rig {
    let tree = Arc::new(AgentTree::new(limits));
    tree.set_root_mode(move || root_mode);
    tree.set_root_tools(|| {
        [
            "ls",
            "read",
            "glob",
            "grep",
            "edit",
            "write",
            "shell",
            "agent",
            "send_message",
        ]
        .map(String::from)
        .to_vec()
    });
    let runner = Arc::new(FakeRunner::default());
    let ledger = Arc::new(FakeLedger {
        messages: vec!["Summarize the repository".to_string()],
        ..FakeLedger::default()
    });
    Rig {
        agent: AgentTool {
            tree: tree.clone(),
            types: Arc::new(types),
            runner: runner.clone(),
            ledger: ledger.clone(),
        },
        send: SendMessageTool {
            tree: tree.clone(),
            ledger: ledger.clone(),
        },
        tree,
        runner,
        ledger,
    }
}

fn rig() -> Rig {
    rig_with(
        Limits {
            max_parallel: 4,
            subagent: Budget {
                tokens: Some(1000),
                cost_usd: Some(1.0),
            },
            ..Limits::default()
        },
        AgentTypes::builtin(),
        RootMode::Ladder(Ladder::Auto),
    )
}

fn as_agent(id: &str) -> ToolContext {
    let mut context = ToolContext::new();
    context.insert(AgentId(id.to_string())).unwrap();
    context
}

fn args(agent_type: &str, prompt: &str) -> AgentArgs {
    AgentArgs {
        agent_type: agent_type.to_string(),
        prompt: prompt.to_string(),
        model: None,
        background: None,
    }
}

async fn spawn(rig: &Rig, args: AgentArgs) -> Result<String, String> {
    rig.agent
        .call(&mut as_agent(ROOT_AGENT), args)
        .await
        .map(|out| out.render())
        .map_err(|e| e.model_output().render())
}

#[tokio::test]
async fn a_blocking_call_returns_the_childs_report() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "found three files".into()));
    let out = spawn(&rig, args("explore", "list the files"))
        .await
        .unwrap();
    assert_eq!(out, "found three files");
    let seen = &rig.runner.spawned()[0];
    assert_eq!(seen.agent_type, "explore");
    assert_eq!(seen.prompt, "list the files");
    assert_eq!(seen.parent, ROOT_AGENT);
    assert_eq!(seen.depth, 1);
    assert!(!seen.background);
    assert_eq!(
        rig.tree.report_of(&seen.id).unwrap().status,
        ChildStatus::Done
    );
}

#[tokio::test]
async fn the_runner_gets_type_mode_budget_a_cut_toolset_and_the_roots_ledger() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&rig, args("worker", "edit things")).await.unwrap();
    let seen = &rig.runner.spawned()[0];
    // The type says inherit, the root is in auto.
    assert_eq!(seen.mode, Ladder::Auto);
    assert_eq!(seen.budget.tokens, Some(1000));
    assert_eq!(seen.budget.cost_usd, Some(1.0));
    // `agent` is not in the worker's toolset, and the rest is a subset of the parent's.
    assert_eq!(
        seen.tools,
        [
            "ls",
            "read",
            "glob",
            "grep",
            "edit",
            "write",
            "shell",
            "send_message"
        ]
    );
    // The intent a guardian would use comes from the root's human messages alone.
    assert_eq!(seen.human, ["Summarize the repository"]);
    assert!(!seen.human.iter().any(|m| m.contains("edit things")));
}

#[tokio::test]
async fn a_toolset_never_exceeds_the_parents() {
    let tree_limits = Limits::default();
    let mut types = AgentTypes::builtin();
    types.insert(AgentType {
        name: "wide".into(),
        description: "asks for everything".into(),
        prompt: None,
        tools: Some(vec!["shell".into(), "webfetch".into(), "mcp__x__y".into()]),
        disallowed: vec![],
        model: None,
        mode: fontaine_agents::mode::ModeSetting::Inherit,
        origin: TypeOrigin::Config,
    });
    let rig = rig_with(tree_limits, types, RootMode::Ladder(Ladder::Manual));
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&rig, args("wide", "x")).await.unwrap();
    assert_eq!(rig.runner.spawned()[0].tools, ["shell"]);
}

#[tokio::test]
async fn an_explicit_mode_is_never_looser_than_the_parents() {
    let mut types = AgentTypes::builtin();
    types.insert(AgentType {
        name: "careful".into(),
        description: "".into(),
        prompt: None,
        tools: None,
        disallowed: vec![],
        model: None,
        mode: fontaine_agents::mode::ModeSetting::Manual,
        origin: TypeOrigin::Config,
    });
    let strict = rig_with(
        Limits::default(),
        types.clone(),
        RootMode::Ladder(Ladder::DontAsk),
    );
    *strict.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&strict, args("careful", "x")).await.unwrap();
    assert_eq!(strict.runner.spawned()[0].mode, Ladder::DontAsk);

    let loose = rig_with(Limits::default(), types, RootMode::Ladder(Ladder::Auto));
    *loose.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&loose, args("careful", "x")).await.unwrap();
    assert_eq!(loose.runner.spawned()[0].mode, Ladder::Manual);
}

#[tokio::test]
async fn the_models_precedence_is_the_call_then_the_type_then_the_session() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&rig, args("explore", "x")).await.unwrap();
    assert_eq!(rig.runner.spawned()[0].model, ModelSpec::GuardianDefault);

    let mut with_model = args("explore", "x");
    with_model.model = Some("work/claude-haiku".into());
    spawn(&rig, with_model).await.unwrap();
    assert_eq!(
        rig.runner.spawned()[1].model,
        ModelSpec::Model("work/claude-haiku".into())
    );

    let mut main = args("explore", "x");
    main.model = Some("$main".into());
    spawn(&rig, main).await.unwrap();
    assert_eq!(rig.runner.spawned()[2].model, ModelSpec::Main);
}

#[tokio::test]
async fn an_invalid_model_and_a_model_on_a_project_type_are_error_results() {
    let mut types = AgentTypes::builtin();
    types.insert(AgentType {
        name: "repo-reviewer".into(),
        description: "from a project file".into(),
        prompt: None,
        tools: None,
        disallowed: vec![],
        model: None,
        mode: fontaine_agents::mode::ModeSetting::Manual,
        origin: TypeOrigin::ProjectFile("repo-reviewer.md".into()),
    });
    let rig = rig_with(Limits::default(), types, RootMode::Ladder(Ladder::Manual));
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));

    let mut bad = args("explore", "x");
    bad.model = Some("sonnet".into());
    let error = spawn(&rig, bad).await.unwrap_err();
    assert!(error.contains("not a model"), "{error}");

    let mut on_project = args("repo-reviewer", "x");
    on_project.model = Some("work/claude".into());
    let error = spawn(&rig, on_project).await.unwrap_err();
    assert!(error.contains("session model"), "{error}");

    // Without a model it runs, on the session model.
    spawn(&rig, args("repo-reviewer", "x")).await.unwrap();
    assert_eq!(rig.runner.spawned().len(), 1);
    assert_eq!(rig.runner.spawned()[0].model, ModelSpec::Main);
}

#[tokio::test]
async fn an_unknown_type_is_an_error_listing_the_known_ones() {
    let rig = rig();
    let error = spawn(&rig, args("nope", "x")).await.unwrap_err();
    assert!(
        error.contains("explore") && error.contains("worker"),
        "{error}"
    );
    assert!(rig.runner.spawned().is_empty());
}

#[tokio::test]
async fn a_spawn_past_max_parallel_starts_nothing_and_says_not_to_retry() {
    let rig = rig_with(
        Limits {
            max_parallel: 1,
            ..Limits::default()
        },
        AgentTypes::builtin(),
        RootMode::Ladder(Ladder::Manual),
    );
    let mut background = args("explore", "first");
    background.background = Some(true);
    let started = spawn(&rig, background).await.unwrap();
    assert!(started.contains("agent-1"), "{started}");

    let error = spawn(&rig, args("explore", "second")).await.unwrap_err();
    assert!(
        error.contains("concurrent subagent limit reached (max_parallel = 1)"),
        "{error}"
    );
    assert!(error.contains("Do not retry"), "{error}");
    let mut also_background = args("explore", "third");
    also_background.background = Some(true);
    assert!(spawn(&rig, also_background).await.is_err());
    assert_eq!(rig.runner.spawned().len(), 1);
}

#[tokio::test]
async fn background_returns_an_id_at_once_and_the_report_arrives_as_a_notification() {
    let rig = rig();
    let mut background = args("explore", "dig");
    background.background = Some(true);
    let started = spawn(&rig, background).await.unwrap();
    assert_eq!(started, r#"{"agent_id":"agent-1"}"#);
    assert!(rig.tree.take_notifications(ROOT_AGENT).is_empty());

    rig.runner
        .child(0)
        .finish(ChildStatus::Done, "buried treasure");
    for _ in 0..100 {
        if rig.tree.has_notifications(ROOT_AGENT) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let notes = rig.tree.take_notifications(ROOT_AGENT);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].from, "agent-1");
    assert!(notes[0].text.contains("buried treasure"));
    assert!(!rig.tree.is_running("agent-1"));
}

#[tokio::test]
async fn n_background_children_run_at_once() {
    let rig = rig();
    for n in 0..4 {
        let mut a = args("explore", &format!("task {n}"));
        a.background = Some(true);
        spawn(&rig, a).await.unwrap();
    }
    assert_eq!(rig.tree.running_ids().len(), 4);
    assert!(rig.tree.background_running());
    rig.tree.kill_all();
    assert!(rig.tree.running_ids().is_empty());
}

#[tokio::test]
async fn a_killed_background_childs_partial_report_is_not_delivered() {
    let rig = rig();
    let mut background = args("explore", "dig");
    background.background = Some(true);
    spawn(&rig, background).await.unwrap();
    assert!(rig.tree.kill("agent-1"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(*rig.runner.child(0).killed.lock().unwrap());
    assert!(rig.tree.take_notifications(ROOT_AGENT).is_empty());
}

#[tokio::test]
async fn a_budget_stopped_child_returns_a_partial_report() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() =
        Some((ChildStatus::BudgetExhausted, "half the files".into()));
    let out = spawn(&rig, args("explore", "x")).await.unwrap();
    assert!(out.contains("half the files"), "{out}");
    assert!(out.contains("Partial result"), "{out}");
}

#[tokio::test]
async fn a_failed_child_is_an_error_result_and_a_refused_start_frees_its_place() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Failed, String::new()));
    assert!(spawn(&rig, args("explore", "x")).await.is_err());

    *rig.runner.fail.lock().unwrap() = Some("no such model".into());
    let error = spawn(&rig, args("explore", "x")).await.unwrap_err();
    assert!(error.contains("no such model"), "{error}");
    assert!(
        rig.tree.running_ids().is_empty(),
        "the reservation was released"
    );
}

#[tokio::test]
async fn dropping_a_blocking_call_kills_the_child() {
    let rig = rig();
    let mut call = Box::pin(spawn(&rig, args("explore", "x")));
    tokio::select! {
        _ = &mut call => panic!("the child never finishes"),
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    }
    drop(call);
    assert!(*rig.runner.child(0).killed.lock().unwrap());
}

#[tokio::test]
async fn nesting_stops_at_three_layers_and_the_agent_tool_is_withheld_there() {
    let mut types = AgentTypes::builtin();
    types.insert(AgentType {
        name: "nester".into(),
        description: "".into(),
        prompt: None,
        tools: None,
        disallowed: vec![],
        model: None,
        mode: fontaine_agents::mode::ModeSetting::Inherit,
        origin: TypeOrigin::Config,
    });
    let rig = rig_with(
        Limits {
            max_parallel: 10,
            ..Limits::default()
        },
        types,
        RootMode::Ladder(Ladder::Manual),
    );
    let mut background = args("nester", "layer one");
    background.background = Some(true);
    spawn(&rig, background).await.unwrap();

    // Each layer's registry is cut from its parent's, so give each child the parent's set.
    let mut parent = "agent-1".to_string();
    for layer in 2..=3_u32 {
        rig.tree
            .set_root_tools(|| ["read", "agent", "send_message"].map(String::from).to_vec());
        let mut context = as_agent(&parent);
        let mut next = args("nester", "deeper");
        next.background = Some(true);
        let out = rig.agent.call(&mut context, next).await;
        out.unwrap();
        parent = format!("agent-{layer}");
    }
    let spawned = rig.runner.spawned();
    assert_eq!(
        spawned.iter().map(|s| s.depth).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    // The third layer's toolset has no `agent`.
    assert!(
        spawned[2].tools.iter().all(|t| t != "agent"),
        "{:?}",
        spawned[2].tools
    );

    let mut context = as_agent(&parent);
    let mut fourth = args("nester", "too deep");
    fourth.background = Some(true);
    let error = rig.agent.call(&mut context, fourth).await.unwrap_err();
    assert!(error.model_output().render().contains("3 layers"));
}

#[tokio::test]
async fn send_message_queues_for_the_recipients_next_turn_and_names_the_sender() {
    let rig = rig();
    let mut one = args("explore", "a");
    one.background = Some(true);
    spawn(&rig, one.clone()).await.unwrap();
    spawn(&rig, one).await.unwrap();

    let send = |rig: &Rig, from: &str, to: &str, text: &str| {
        let tool = rig.send.clone();
        let mut context = as_agent(from);
        let args = SendMessageArgs {
            to: to.to_string(),
            message: text.to_string(),
        };
        async move {
            tool.call(&mut context, args)
                .await
                .map(|o| o.render())
                .map_err(|e| e.model_output().render())
        }
    };
    // Sibling, parent-to-child, child-to-main.
    assert_eq!(
        send(&rig, "agent-1", "agent-2", "hello sibling")
            .await
            .unwrap(),
        r#"{"status":"queued"}"#
    );
    assert_eq!(
        send(&rig, ROOT_AGENT, "agent-1", "hello child")
            .await
            .unwrap(),
        r#"{"status":"queued"}"#
    );
    assert_eq!(
        send(&rig, "agent-2", "main", "hello main").await.unwrap(),
        r#"{"status":"queued"}"#
    );

    let sibling = rig.tree.take_notifications("agent-2");
    assert_eq!(sibling[0].from, "agent-1");
    assert!(sibling[0].render().contains("agent agent-1"));
    assert!(sibling[0].render().contains("untrusted"));
    assert_eq!(
        rig.tree.take_notifications("agent-1")[0].text,
        "hello child"
    );
    assert_eq!(rig.tree.take_notifications(ROOT_AGENT)[0].from, "agent-2");

    // A finished or killed target is not running.
    rig.tree.kill("agent-2");
    assert_eq!(
        send(&rig, "agent-1", "agent-2", "late").await.unwrap_err(),
        "agent not running"
    );
    assert_eq!(
        send(&rig, "agent-1", "agent-99", "who").await.unwrap_err(),
        "agent not running"
    );
    assert!(rig.ledger.events.lock().unwrap().iter().any(
        |e| matches!(e, AgentEvent::Message { from, to } if from == "agent-1" && to == "agent-2")
    ));
}

#[tokio::test]
async fn dropping_a_spawn_that_is_still_starting_gives_back_its_slot() {
    let rig = rig();
    *rig.runner.hang.lock().unwrap() = true;
    let started = tokio::time::timeout(
        Duration::from_millis(50),
        spawn(&rig, args("explore", "slow")),
    )
    .await;
    assert!(started.is_err());
    assert!(rig.tree.running_ids().is_empty());
    assert!(!rig.tree.background_running());
}

#[tokio::test]
async fn a_kill_that_lands_before_the_handle_attaches_still_stops_the_child() {
    let rig = rig();
    let id = rig
        .tree
        .reserve(ROOT_AGENT, "explore", "m", vec![], Ladder::Manual, false)
        .unwrap();
    assert!(rig.tree.kill(&id));
    let (report, _) = watch::channel(None);
    let child = Arc::new(FakeChild {
        status: Mutex::new(ChildStatus::Running),
        report,
        killed: Mutex::new(false),
    });
    rig.tree.attach(&id, child.clone());
    assert!(*child.killed.lock().unwrap());
}
