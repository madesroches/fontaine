//! Idle agents through a real runtime: a finished turn leaves any subagent idle, a peer's message
//! wakes it without becoming the user's words, nothing hands it new board work, the owner of a
//! board is reminded, and `/clear`, `/rewind` and an AFK stop retire idle agents.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::{Built, build_keyed, build_keyed_with};
use fontaine_agents::runner::ChildStatus;
use fontaine_agents::tree::NotificationKind;
use fontaine_core::agent::{Delivered, MessageRefusal};
use fontaine_core::checkpoint::RewindWhat;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::ledger::Record;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use fontaine_tools::gate::ROOT_AGENT;
use rig_core::completion::Usage;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, mock_final};
use serde_json::json;

fn usage() -> Usage {
    let mut usage = Usage::new();
    usage.input_tokens = Some(100);
    usage.output_tokens = Some(50);
    usage
}

fn text(words: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(words.to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage())),
    ]
}

fn call(name: &str, arguments: serde_json::Value) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: format!("call-{name}"),
            name: name.into(),
            arguments,
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(usage())),
    ]
}

fn spawn_worker() -> Vec<MockStreamEvent> {
    call(
        "agent",
        json!({"agent_type": "worker", "prompt": "dig into it", "name": "digger"}),
    )
}

fn world(extra: &str) -> World {
    let w = World::new();
    w.user_file(&format!(
        "[permissions]\nallow = [\"agent\", \"send_message\", \"board_update\", \"board_show\"]\n[agents]\n{extra}"
    ));
    w
}

async fn root_turn(built: &Built, prompt: &str) {
    built.runtime.begin_turn(prompt, false);
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: prompt.to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(5),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: None,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{what}");
}

/// A runtime whose main conversation starts a plain worker, which answers "first turn" and goes
/// idle.
async fn idle_worker(
    w: &World,
    root_after: Vec<Vec<MockStreamEvent>>,
    child_after: Vec<Vec<MockStreamEvent>>,
) -> (Built, MockCompletionModel) {
    idle_worker_with(w, root_after, child_after, true).await
}

async fn idle_worker_with(
    w: &World,
    root_after: Vec<Vec<MockStreamEvent>>,
    child_after: Vec<Vec<MockStreamEvent>>,
    headless: bool,
) -> (Built, MockCompletionModel) {
    let mut root = vec![spawn_worker(), text("started")];
    root.extend(root_after);
    let mut child = vec![text("first turn")];
    child.extend(child_after);
    let (built, model) = build_keyed_with(w, Manual, "dig into it", root, child, None, headless);
    root_turn(&built, "start a worker").await;
    let tree = built.runtime.state.agents().clone();
    until("the worker never went idle", || tree.is_idle("agent-1")).await;
    // Its report waits for the root, which would also block /clear.
    let notes = tree.take_notifications(ROOT_AGENT);
    assert!(
        notes.iter().any(|n| n.text.contains("is idle")),
        "{notes:?}"
    );
    (built, model)
}

fn user_messages(built: &Built) -> Vec<String> {
    built
        .runtime
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.record {
            Record::UserMessage { text, agent, .. } => {
                Some(format!("{}: {text}", agent.unwrap_or_default()))
            }
            _ => None,
        })
        .collect()
}

fn finished(built: &Built, agent: &str) -> Vec<String> {
    built
        .runtime
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.record {
            Record::Agent {
                event,
                agent: id,
                detail,
                ..
            } if event == "finished" && id == agent => detail,
            _ => None,
        })
        .collect()
}

/// The #435 failure: the main conversation's `send_message` to a plain subagent that ended its
/// turn must wake it, with its earlier history and the sender-marked message.
async fn a_peers_message_wakes_a_finished_plain_agent(extra: &str) {
    let w = world(extra);
    let (built, child) = idle_worker(
        &w,
        vec![
            call(
                "send_message",
                json!({"to": "digger", "message": "also check the tests"}),
            ),
            text("sent"),
        ],
        vec![text("second turn")],
    )
    .await;
    let tree = built.runtime.state.agents().clone();
    assert!(tree.is_idle("agent-1"));

    root_turn(&built, "tell it more").await;
    until("the worker never ran again", || {
        tree.is_idle("agent-1") && child.requests().len() >= 2
    })
    .await;

    let last = format!("{:?}", child.requests().last().unwrap());
    assert!(last.contains("also check the tests"), "{last}");
    assert!(last.contains("untrusted"), "{last}");
    assert!(
        last.contains("first turn"),
        "the same conversation continues: {last}"
    );
    let sent = format!("{:?}", built.main.requests().last().unwrap());
    assert!(
        sent.contains("queued"),
        "send_message answered queued: {sent}"
    );
    let recorded = user_messages(&built);
    assert!(
        recorded.iter().all(|m| !m.contains("also check the tests")),
        "{recorded:?}"
    );
    assert!(
        built
            .runtime
            .session
            .ledger
            .view()
            .user_messages
            .iter()
            .all(|m| !m.contains("also check the tests"))
    );
}

#[tokio::test]
async fn a_peers_message_wakes_a_finished_plain_agent_and_is_never_recorded_as_the_users() {
    a_peers_message_wakes_a_finished_plain_agent("teams = true\n").await;
}

#[tokio::test]
async fn idling_and_waking_do_not_depend_on_teams() {
    a_peers_message_wakes_a_finished_plain_agent("teams = false\n").await;
}

#[tokio::test]
async fn the_human_typing_into_an_idle_agents_view_is_still_recorded() {
    let w = world("");
    let (built, _child) = idle_worker(&w, vec![], vec![text("after the human")]).await;
    let delivered = built
        .runtime
        .message_agent("agent-1", "the human's words".into())
        .unwrap();
    assert_eq!(delivered, Delivered::Resumed);
    let tree = built.runtime.state.agents().clone();
    assert!(!tree.is_idle("agent-1"));
    until("the worker never went idle again", || {
        tree.is_idle("agent-1")
    })
    .await;
    assert_eq!(
        user_messages(&built).last().unwrap(),
        "agent-1: the human's words"
    );
}

#[tokio::test]
async fn an_idle_worker_is_not_handed_the_other_task_and_main_is_reminded_once_in_the_tui_only() {
    for headless in [false, true] {
        let w = world("");
        let (built, child) = idle_worker_with(&w, vec![], vec![text("unused")], headless).await;
        let tree = built.runtime.state.agents().clone();
        let board = tree.teams().expect("teams are on").boards.root().clone();
        board.create(ROOT_AGENT, "one", "", &[]).unwrap();
        board.create(ROOT_AGENT, "two", "", &[]).unwrap();
        // Idle agents are not woken by the board, however much work waits.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(tree.is_idle("agent-1"));
        assert_eq!(child.requests().len(), 1);

        let end = TurnOutcome::Answer("done".into());
        built.runtime.goal_turn_ended(Some(&end));
        let notes = tree.take_notifications(ROOT_AGENT);
        if headless {
            assert!(notes.is_empty(), "{notes:?}");
        } else {
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert_eq!(notes[0].kind, NotificationKind::Reminder);
            assert!(
                notes[0]
                    .text
                    .contains("2 tasks on your board are claimable")
            );
            assert!(notes[0].text.contains("4 agent slots are free"));
            built.runtime.goal_turn_ended(Some(&end));
            assert!(
                tree.take_notifications(ROOT_AGENT).is_empty(),
                "once per change"
            );
            built.runtime.goal_turn_ended(Some(&TurnOutcome::Stopped));
            built.runtime.goal_turn_ended(None);
            assert!(tree.take_notifications(ROOT_AGENT).is_empty());
        }
        assert!(
            user_messages(&built)
                .iter()
                .all(|m| !m.contains("claimable"))
        );
    }
}

#[tokio::test]
async fn clear_retires_idle_agents_and_releases_their_tasks() {
    let w = world("");
    let (built, _child) = idle_worker(&w, vec![], vec![]).await;
    let tree = built.runtime.state.agents().clone();
    let board = tree.teams().expect("teams are on").boards.root().clone();
    board.create(ROOT_AGENT, "held", "", &[]).unwrap();
    board.claim("agent-1", "t1").unwrap();

    built.runtime.clear_conversation().await.unwrap();

    assert!(tree.rows().is_empty(), "the pruned agent leaves the list");
    assert_eq!(
        board.get("t1").unwrap().status,
        fontaine_agents::tasks::TaskStatus::Pending
    );
    assert_eq!(finished(&built, "agent-1"), ["done", "retired"]);
}

#[tokio::test]
async fn a_conversation_rewind_retires_idle_agents_and_a_later_message_is_refused() {
    let w = world("");
    let (built, _child) = idle_worker(
        &w,
        vec![
            call(
                "send_message",
                json!({"to": "digger", "message": "wake up"}),
            ),
            text("sent"),
        ],
        vec![],
    )
    .await;
    let tree = built.runtime.state.agents().clone();
    let plan = built.runtime.rewind_plan(1).unwrap();
    built
        .runtime
        .rewind(&plan, RewindWhat::Both, false)
        .await
        .unwrap();
    assert!(!tree.is_idle("agent-1"));
    assert_eq!(tree.status_of("agent-1"), Some(ChildStatus::Done));
    assert_eq!(finished(&built, "agent-1"), ["done", "retired"]);
    let refusal = built
        .runtime
        .message_agent("agent-1", "hello".into())
        .unwrap_err();
    assert!(
        matches!(&refusal, MessageRefusal::Limit(why) if why.contains("retired")),
        "{refusal:?}"
    );
}

#[tokio::test]
async fn an_afk_stop_retires_idle_agents_and_kills_running_ones() {
    let w = world("");
    let (built, _child) = idle_worker(&w, vec![], vec![]).await;
    let tree = built.runtime.state.agents().clone();
    let running = tree
        .reserve(
            ROOT_AGENT,
            "worker",
            "m",
            vec![],
            fontaine_agents::mode::Ladder::Manual,
        )
        .unwrap();
    built.runtime.handle_afk_stop().await;
    assert_eq!(tree.status_of(&running), Some(ChildStatus::Killed));
    assert_eq!(finished(&built, "agent-1"), ["done", "retired"]);
    assert!(!tree.is_idle("agent-1"));
    let refusal = built
        .runtime
        .message_agent("agent-1", "hello".into())
        .unwrap_err();
    assert!(
        matches!(&refusal, MessageRefusal::Limit(why) if why.contains("retired")),
        "{refusal:?}"
    );
}

#[tokio::test]
async fn the_board_tools_exist_only_with_teams_on() {
    let off = World::new();
    off.user_file("[permissions]\nallow = [\"agent\"]\n[agents]\nteams = false\n");
    let (built, _) = build_keyed(&off, Manual, "x", vec![], vec![]);
    let names = built.runtime.tool_names();
    assert!(!names.iter().any(|n| n.starts_with("board_")), "{names:?}");

    let on = world("teams = true\n");
    let (built, _) = build_keyed(&on, Manual, "x", vec![], vec![]);
    let names = built.runtime.tool_names();
    assert!(names.contains(&"board_show".to_string()), "{names:?}");
    assert!(names.contains(&"board_update".to_string()), "{names:?}");
}
