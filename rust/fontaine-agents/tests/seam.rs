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
use fontaine_tools::jobs::tools::{JobOutputArgs, JobOutputTool};
use fontaine_tools::jobs::{AgentWaits, Jobs};
use futures::future::BoxFuture;
use rig_core::tool::{Tool, ToolContext};
use rig_tools::OutputLimit;
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
    prompt: String,
    human: Vec<String>,
    inherits_context: bool,
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
    /// What the human typed that the last run never read.
    missed: Mutex<Vec<String>>,
    /// Runs while the missed messages are taken, after the run was concluded.
    on_take: Mutex<Option<Box<dyn Fn() + Send>>>,
    resumes: Mutex<Vec<String>>,
    undelivered: Mutex<Vec<(String, String)>>,
}

impl FakeChild {
    fn running() -> FakeChild {
        FakeChild {
            status: Mutex::new(ChildStatus::Running),
            report: watch::channel(None).0,
            killed: Mutex::new(false),
            missed: Mutex::new(Vec::new()),
            on_take: Mutex::new(None),
            resumes: Mutex::new(Vec::new()),
            undelivered: Mutex::new(Vec::new()),
        }
    }

    fn finish(&self, status: ChildStatus, text: &str) {
        *self.status.lock().unwrap() = status;
        self.report.send_replace(Some(ChildReport {
            status,
            text: text.to_string(),
            reason: match status {
                ChildStatus::BudgetExhausted => Some("its budget is spent".into()),
                ChildStatus::Failed => Some("the provider is down".into()),
                _ => None,
            },
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

    fn resume(&self, prompt: String) {
        self.resumes.lock().unwrap().push(prompt);
        *self.status.lock().unwrap() = ChildStatus::Running;
        self.report.send_replace(None);
    }

    fn take_missed(&self) -> Vec<String> {
        if let Some(hook) = self.on_take.lock().unwrap().as_ref() {
            hook();
        }
        std::mem::take(&mut *self.missed.lock().unwrap())
    }

    fn report_undelivered(&self, refusal: &str, text: &str) {
        self.undelivered
            .lock()
            .unwrap()
            .push((refusal.to_string(), text.to_string()));
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
    /// Messages each new child's first run will have left unread.
    missed: Mutex<Vec<String>>,
    /// Installed as each new child's `on_take`.
    fill: Mutex<Option<Box<dyn Fn() + Send>>>,
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
                prompt: request.prompt.clone(),
                human: request.ledger.human_messages(),
                inherits_context: request.agent_type.inherits_context,
            });
            let (report, _) = watch::channel(None);
            let child = Arc::new(FakeChild {
                status: Mutex::new(ChildStatus::Running),
                report,
                killed: Mutex::new(false),
                missed: Mutex::new(self.missed.lock().unwrap().clone()),
                on_take: Mutex::new(None),
                resumes: Mutex::new(Vec::new()),
                undelivered: Mutex::new(Vec::new()),
            });
            if let Some((status, text)) = self.answer.lock().unwrap().clone() {
                child.finish(status, &text);
            }
            *child.on_take.lock().unwrap() = self.fill.lock().unwrap().take();
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
    }
}

async fn spawn(rig: &Rig, args: AgentArgs) -> Result<String, String> {
    rig.agent
        .call(&mut as_agent(ROOT_AGENT), args)
        .await
        .map(|out| out.render())
        .map_err(|e| e.model_output().render())
}

/// Waits for the watcher task to queue the report the fake child just produced.
async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_call_returns_the_agent_id_before_the_child_ends() {
    let rig = rig();
    let out = spawn(&rig, args("explore", "list the files"))
        .await
        .unwrap();
    assert_eq!(out, r#"{"agent_id":"agent-1"}"#);
    let seen = &rig.runner.spawned()[0];
    assert_eq!(seen.agent_type, "explore");
    assert_eq!(seen.prompt, "list the files");
    assert_eq!(seen.parent, ROOT_AGENT);
    assert_eq!(seen.depth, 1);
    assert!(rig.tree.is_running(&seen.id));
    assert!(rig.tree.report_of(&seen.id).is_none());
    assert!(rig.tree.take_notifications(ROOT_AGENT).is_empty());
}

#[test]
fn the_old_background_argument_is_an_unknown_field() {
    let error = serde_json::from_value::<AgentArgs>(
        serde_json::json!({"agent_type": "explore", "prompt": "x", "background": true}),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unknown field `background`"), "{error}");
}

#[tokio::test]
async fn two_spawns_in_a_row_both_reach_the_runner_while_the_first_child_is_blocked() {
    let rig = rig();
    spawn(&rig, args("explore", "first")).await.unwrap();
    spawn(&rig, args("explore", "second")).await.unwrap();
    assert_eq!(rig.runner.spawned().len(), 2);
    assert_eq!(rig.tree.running_ids().len(), 2);
    assert!(rig.tree.take_notifications(ROOT_AGENT).is_empty());
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
        inherits_context: false,
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
        inherits_context: false,
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
    assert_eq!(rig.runner.spawned()[0].model, ModelSpec::Main);

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
        inherits_context: false,
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
    let started = spawn(&rig, args("explore", "first")).await.unwrap();
    assert!(started.contains("agent-1"), "{started}");

    let error = spawn(&rig, args("explore", "second")).await.unwrap_err();
    assert!(
        error.contains("concurrent subagent limit reached (max_parallel = 1)"),
        "{error}"
    );
    assert!(error.contains("Do not retry"), "{error}");
    assert!(spawn(&rig, args("explore", "third")).await.is_err());
    assert_eq!(rig.runner.spawned().len(), 1);
}

#[tokio::test]
async fn the_report_arrives_as_a_notification_naming_the_agent() {
    let rig = rig();
    let started = spawn(&rig, args("explore", "dig")).await.unwrap();
    assert_eq!(started, r#"{"agent_id":"agent-1"}"#);
    assert!(rig.tree.take_notifications(ROOT_AGENT).is_empty());

    rig.runner
        .child(0)
        .finish(ChildStatus::Done, "buried treasure");
    until(|| rig.tree.has_notifications(ROOT_AGENT)).await;
    let notes = rig.tree.take_notifications(ROOT_AGENT);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].from, "agent-1");
    assert!(
        notes[0]
            .text
            .starts_with("Agent agent-1 (explore) finished: done")
    );
    assert!(notes[0].text.contains("buried treasure"));
    assert!(!rig.tree.is_running("agent-1"));
}

#[tokio::test]
async fn n_children_run_at_once() {
    let rig = rig();
    for n in 0..4 {
        spawn(&rig, args("explore", &format!("task {n}")))
            .await
            .unwrap();
    }
    assert_eq!(rig.tree.running_ids().len(), 4);
    assert!(rig.tree.any_running());
    rig.tree.kill_all();
    assert!(rig.tree.running_ids().is_empty());
}

#[tokio::test]
async fn a_killed_childs_partial_report_is_not_delivered() {
    let rig = rig();
    spawn(&rig, args("explore", "dig")).await.unwrap();
    assert!(rig.tree.kill("agent-1"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(*rig.runner.child(0).killed.lock().unwrap());
    assert!(rig.tree.take_notifications(ROOT_AGENT).is_empty());
}

#[tokio::test]
async fn a_budget_stopped_child_reports_a_partial_result() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() =
        Some((ChildStatus::BudgetExhausted, "half the files".into()));
    spawn(&rig, args("explore", "x")).await.unwrap();
    until(|| rig.tree.has_notifications(ROOT_AGENT)).await;
    let notes = rig.tree.take_notifications(ROOT_AGENT);
    assert!(notes[0].text.contains("half the files"), "{notes:?}");
    assert!(notes[0].text.contains("Partial result"), "{notes:?}");
}

#[tokio::test]
async fn a_failed_childs_reason_reaches_the_parent_and_a_refused_start_frees_its_place() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Failed, String::new()));
    spawn(&rig, args("explore", "x")).await.unwrap();
    until(|| rig.tree.has_notifications(ROOT_AGENT)).await;
    let notes = rig.tree.take_notifications(ROOT_AGENT);
    assert!(notes[0].text.contains("failed"), "{notes:?}");
    assert!(notes[0].text.contains("the provider is down"), "{notes:?}");

    *rig.runner.answer.lock().unwrap() = None;
    *rig.runner.fail.lock().unwrap() = Some("no such model".into());
    let error = spawn(&rig, args("explore", "x")).await.unwrap_err();
    assert!(error.contains("no such model"), "{error}");
    assert!(
        rig.tree.running_ids().is_empty(),
        "the reservation was released"
    );
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
        inherits_context: false,
    });
    let rig = rig_with(
        Limits {
            max_parallel: 10,
            ..Limits::default()
        },
        types,
        RootMode::Ladder(Ladder::Manual),
    );
    spawn(&rig, args("nester", "layer one")).await.unwrap();

    // Each layer's registry is cut from its parent's, so give each child the parent's set.
    let mut parent = "agent-1".to_string();
    for layer in 2..=3_u32 {
        rig.tree
            .set_root_tools(|| ["read", "agent", "send_message"].map(String::from).to_vec());
        let mut context = as_agent(&parent);
        let next = args("nester", "deeper");
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
    let fourth = args("nester", "too deep");
    let error = rig.agent.call(&mut context, fourth).await.unwrap_err();
    assert!(error.model_output().render().contains("3 layers"));
}

#[tokio::test]
async fn send_message_queues_for_the_recipients_next_turn_and_names_the_sender() {
    let rig = rig();
    let one = args("explore", "a");
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
    assert!(!rig.tree.any_running());
}

#[tokio::test]
async fn a_kill_that_lands_before_the_handle_attaches_still_stops_the_child() {
    let rig = rig();
    let id = rig
        .tree
        .reserve(ROOT_AGENT, "explore", "m", vec![], Ladder::Manual)
        .unwrap();
    assert!(rig.tree.kill(&id));
    let child = Arc::new(FakeChild::running());
    rig.tree.attach(&id, child.clone());
    assert!(*child.killed.lock().unwrap());
}

#[tokio::test]
async fn a_child_that_ends_abnormally_stops_its_own_running_children() {
    let rig = rig();
    spawn(&rig, args("explore", "dig")).await.unwrap();
    let grandchild = rig
        .tree
        .reserve("agent-1", "explore", "m", vec![], Ladder::Manual)
        .unwrap();
    let handle = Arc::new(FakeChild::running());
    rig.tree.attach(&grandchild, handle.clone());

    rig.runner
        .child(0)
        .finish(ChildStatus::BudgetExhausted, "finished");
    until(|| !rig.tree.is_running(&grandchild)).await;
    assert!(rig.tree.was_killed(&grandchild));
    assert!(*handle.killed.lock().unwrap());
    assert!(!rig.tree.any_running());
}

#[tokio::test]
async fn a_call_that_does_not_say_whose_it_is_is_not_served_as_the_root_agents() {
    let rig = rig();
    let error = rig
        .agent
        .call(&mut ToolContext::new(), args("explore", "a"))
        .await
        .unwrap_err();
    assert!(error.model_output().render().contains("which agent"));
    let error = rig
        .send
        .clone()
        .call(
            &mut ToolContext::new(),
            SendMessageArgs {
                to: "main".to_string(),
                message: "hi".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert!(error.model_output().render().contains("which agent"));
}

#[tokio::test]
async fn a_fork_inherits_the_parents_tools_and_mode_and_only_it_inherits_context() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&rig, args("fork", "short task")).await.unwrap();
    spawn(&rig, args("explore", "look")).await.unwrap();
    let spawned = rig.runner.spawned();
    assert!(spawned[0].inherits_context);
    assert!(!spawned[1].inherits_context);
    assert_eq!(spawned[0].model, ModelSpec::Main);
    assert_eq!(spawned[0].mode, Ladder::Auto);
    assert_eq!(
        spawned[0].tools,
        [
            "ls",
            "read",
            "glob",
            "grep",
            "edit",
            "write",
            "shell",
            "agent",
            "send_message"
        ]
    );
}

#[tokio::test]
async fn a_fork_refuses_a_model_and_a_call_from_a_child() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    let mut with_model = args("fork", "x");
    with_model.model = Some("$main".into());
    let error = spawn(&rig, with_model).await.unwrap_err();
    assert!(error.contains("session model"), "{error}");

    let from_child = rig
        .agent
        .call(&mut as_agent("agent-1"), args("fork", "x"))
        .await
        .unwrap_err()
        .model_output()
        .render();
    assert!(
        from_child.contains("only the main conversation"),
        "{from_child}"
    );
    assert!(rig.runner.spawned().is_empty());
    assert!(rig.tree.running_ids().is_empty());
}

#[tokio::test]
async fn a_configured_type_named_fork_replaces_the_built_in_and_loses_the_capability() {
    let mut types = AgentTypes::builtin();
    types.insert(AgentType {
        name: "fork".into(),
        description: "".into(),
        prompt: None,
        tools: None,
        disallowed: vec![],
        model: None,
        mode: fontaine_agents::mode::ModeSetting::Inherit,
        origin: TypeOrigin::Config,
        inherits_context: false,
    });
    let rig = rig_with(Limits::default(), types, RootMode::Ladder(Ladder::Manual));
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "ok".into()));
    spawn(&rig, args("fork", "x")).await.unwrap();
    assert!(!rig.runner.spawned()[0].inherits_context);
}

fn waiter(rig: &Rig) -> JobOutputTool {
    JobOutputTool {
        jobs: Arc::new(Jobs::new()),
        limit: OutputLimit::default(),
        agents: Some(rig.tree.clone() as Arc<dyn AgentWaits>),
        wake: None,
    }
}

async fn wait_as(
    tool: &JobOutputTool,
    caller: &str,
    id: &str,
    wait_secs: Option<u64>,
) -> Result<String, String> {
    tool.call(
        &mut as_agent(caller),
        JobOutputArgs {
            id: Some(id.to_string()),
            wait_secs,
        },
    )
    .await
    .map_err(|e| e.model_output().render())
}

fn reports_for(rig: &Rig, agent: &str) -> usize {
    let notes = rig.tree.take_notifications(agent);
    let reports = notes
        .iter()
        .filter(|n| n.kind == fontaine_agents::tree::NotificationKind::Report)
        .count();
    assert_eq!(reports, notes.len());
    reports
}

#[tokio::test]
async fn waiting_on_a_child_returns_its_status_and_leaves_one_report() {
    let rig = rig();
    spawn(&rig, args("explore", "dig")).await.unwrap();
    let child = rig.runner.child(0);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        child.finish(ChildStatus::Done, "buried treasure");
    });
    let started = std::time::Instant::now();
    let out = wait_as(&waiter(&rig), ROOT_AGENT, "agent-1", None)
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(
        out,
        "[agent-1 ended: done]\nIts report follows as a notification."
    );
    assert!(!out.contains("buried treasure"));
    assert_eq!(reports_for(&rig, ROOT_AGENT), 1);
}

#[tokio::test]
async fn a_nested_parent_waits_for_its_own_child_and_stays_alive() {
    let mut types = AgentTypes::builtin();
    let mut lead = types.get("worker").unwrap().clone();
    lead.name = "lead".into();
    lead.tools = None;
    types.insert(lead);
    let rig = rig_with(
        Limits {
            max_parallel: 4,
            ..Limits::default()
        },
        types,
        RootMode::Ladder(Ladder::Auto),
    );
    spawn(&rig, args("lead", "coordinate")).await.unwrap();
    rig.agent
        .call(&mut as_agent("agent-1"), args("explore", "dig"))
        .await
        .unwrap();
    let grandchild = rig.runner.child(1);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        grandchild.finish(ChildStatus::Done, "deep treasure");
    });
    let out = wait_as(&waiter(&rig), "agent-1", "agent-2", None)
        .await
        .unwrap();
    assert!(out.starts_with("[agent-2 ended: done]"), "{out}");
    assert!(rig.tree.is_running("agent-1"));
    assert!(!rig.tree.was_killed("agent-2"));
    assert_eq!(reports_for(&rig, "agent-1"), 1);

    let err = wait_as(&waiter(&rig), ROOT_AGENT, "agent-2", Some(0))
        .await
        .unwrap_err();
    assert!(err.contains("not a subagent you started"), "{err}");
}

#[tokio::test]
async fn an_abandoned_wait_leaves_the_child_running_and_its_report_queued() {
    let rig = rig();
    spawn(&rig, args("explore", "dig")).await.unwrap();
    let tool = waiter(&rig);
    let parked = tokio::spawn(async move { wait_as(&tool, ROOT_AGENT, "agent-1", None).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    parked.abort();
    let _ = parked.await;
    assert!(rig.tree.is_running("agent-1"));
    rig.runner.child(0).finish(ChildStatus::Done, "late news");
    until(|| rig.tree.has_notifications(ROOT_AGENT)).await;
    assert!(!rig.tree.was_killed("agent-1"));
    assert!(!*rig.runner.child(0).killed.lock().unwrap());
    assert_eq!(reports_for(&rig, ROOT_AGENT), 1);
}

#[tokio::test]
async fn a_child_that_missed_a_message_is_resumed_and_reports_twice() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "first answer".into()));
    *rig.runner.missed.lock().unwrap() = vec!["more please".into()];
    spawn(&rig, args("worker", "look around")).await.unwrap();
    until(|| {
        rig.tree.has_notifications(ROOT_AGENT)
            && !rig.runner.child(0).resumes.lock().unwrap().is_empty()
    })
    .await;

    let child = rig.runner.child(0);
    let id = rig.runner.spawned()[0].id.clone();
    assert_eq!(*child.resumes.lock().unwrap(), ["more please"]);
    assert!(rig.tree.is_running(&id));
    let first = rig.tree.take_notifications(ROOT_AGENT);
    assert!(first[0].text.contains("first answer"), "{first:?}");

    child.finish(ChildStatus::Done, "second answer");
    until(|| rig.tree.has_notifications(ROOT_AGENT)).await;
    let second = rig.tree.take_notifications(ROOT_AGENT);
    assert!(second[0].text.contains("second answer"), "{second:?}");
}

#[tokio::test]
async fn a_refused_automatic_resume_hands_the_refusal_and_the_text_to_the_child() {
    let rig = rig_with(
        Limits {
            max_parallel: 1,
            ..Limits::default()
        },
        AgentTypes::builtin(),
        RootMode::Ladder(Ladder::Auto),
    );
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "first answer".into()));
    *rig.runner.missed.lock().unwrap() = vec!["too late".into()];
    // By the time the leftovers are taken, another agent holds the only place.
    let tree = rig.tree.clone();
    let runner = rig.runner.clone();
    *runner.fill.lock().unwrap() = Some(Box::new(move || {
        tree.reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
            .unwrap();
    }));
    spawn(&rig, args("worker", "look around")).await.unwrap();

    let child = rig.runner.child(0);
    until(|| !child.undelivered.lock().unwrap().is_empty()).await;
    assert!(child.resumes.lock().unwrap().is_empty());
    let undelivered = child.undelivered.lock().unwrap().clone();
    assert_eq!(undelivered.len(), 1, "{undelivered:?}");
    assert!(
        undelivered[0].0.contains("max_parallel = 1"),
        "{undelivered:?}"
    );
    assert_eq!(undelivered[0].1, "too late");
}

#[tokio::test]
async fn a_killed_child_that_missed_a_message_is_not_resumed() {
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Killed, "partial".into()));
    *rig.runner.missed.lock().unwrap() = vec!["are you there".into()];
    let _ = spawn(&rig, args("worker", "look around")).await;
    let child = rig.runner.child(0);
    until(|| !child.undelivered.lock().unwrap().is_empty()).await;
    assert!(child.resumes.lock().unwrap().is_empty());
    let undelivered = child.undelivered.lock().unwrap().clone();
    assert_eq!(undelivered.len(), 1);
    assert!(undelivered[0].0.contains("killed"), "{undelivered:?}");
}

#[tokio::test]
async fn of_two_racing_resumes_of_an_ended_child_one_starts_a_run() {
    use fontaine_agents::spawn::resume_child;
    let rig = rig();
    *rig.runner.answer.lock().unwrap() = Some((ChildStatus::Done, "first answer".into()));
    spawn(&rig, args("worker", "look around")).await.unwrap();
    let id = rig.runner.spawned()[0].id.clone();
    until(|| !rig.tree.is_running(&id)).await;
    let handle = rig.tree.handle_of(&id).expect("the tree holds the handle");
    let ledger: Arc<dyn RootLedger> = rig.ledger.clone();

    assert!(resume_child(&rig.tree, &ledger, &id, &handle, "first".into()).is_ok());
    assert_eq!(
        resume_child(&rig.tree, &ledger, &id, &handle, "second".into()),
        Err(fontaine_agents::tree::Refusal::AlreadyRunning)
    );
    assert_eq!(*rig.runner.child(0).resumes.lock().unwrap(), ["first"]);
}
