//! Teammates through a real runtime: a finished turn leaves the teammate idle, a peer's message,
//! a hand-off note or a hook's reason wakes it without becoming the user's words, and the
//! conversation commands refuse while one is idle.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::{Built, build_keyed};
use fontaine_agents::runner::ChildStatus;
use fontaine_core::agent::{Delivered, RewindError};
use fontaine_core::checkpoint::RewindWhat;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::ledger::Record;
use fontaine_core::session::ClearError;
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

fn spawn_teammate() -> Vec<MockStreamEvent> {
    call(
        "agent",
        json!({"agent_type": "worker", "prompt": "dig into it", "teammate": true, "name": "digger"}),
    )
}

fn world(extra: &str) -> World {
    let w = World::new();
    w.user_file(&format!(
        "[permissions]\nallow = [\"agent\", \"send_message\", \"board_update\", \"board_show\"]\n[agents]\nteams = true\n{extra}"
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

/// A runtime whose main conversation starts a teammate, which answers "first turn" and goes idle.
async fn idle_teammate(
    w: &World,
    root_after: Vec<Vec<MockStreamEvent>>,
    child_after: Vec<Vec<MockStreamEvent>>,
) -> (Built, MockCompletionModel) {
    let mut root = vec![spawn_teammate(), text("started")];
    root.extend(root_after);
    let mut child = vec![text("first turn")];
    child.extend(child_after);
    let (built, model) = build_keyed(w, Manual, "dig into it", root, child);
    root_turn(&built, "start a teammate").await;
    let tree = built.runtime.state.agents().clone();
    until("the teammate never went idle", || tree.is_idle("agent-1")).await;
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

#[tokio::test]
async fn a_peers_message_wakes_an_idle_teammate_and_is_never_recorded_as_the_users() {
    let w = world("");
    let (built, child) = idle_teammate(
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
    until("the teammate never ran again", || {
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
async fn the_human_typing_into_an_idle_teammates_view_is_still_recorded() {
    let w = world("");
    let (built, _child) = idle_teammate(&w, vec![], vec![text("after the human")]).await;
    let delivered = built
        .runtime
        .message_agent("agent-1", "the human's words".into())
        .unwrap();
    assert_eq!(delivered, Delivered::Resumed);
    let tree = built.runtime.state.agents().clone();
    assert!(!tree.is_idle("agent-1"));
    until("the teammate never went idle again", || {
        tree.is_idle("agent-1")
    })
    .await;
    assert_eq!(
        user_messages(&built).last().unwrap(),
        "agent-1: the human's words"
    );
}

#[tokio::test]
async fn the_hand_off_note_wakes_a_teammate_without_recording_a_user_message_and_stops_at_the_cap()
{
    let w = world("");
    // Turns of the teammate: its first, the one the human's message starts, then the three
    // hand-off resumes the cap allows.
    let (built, child) = idle_teammate(
        &w,
        vec![
            call(
                "board_update",
                json!({"action": "create", "title": "one more thing"}),
            ),
            text("created"),
        ],
        vec![
            text("w"),
            text("h1"),
            text("h2"),
            text("h3"),
            text("never asked"),
        ],
    )
    .await;
    let tree = built.runtime.state.agents().clone();
    let board = tree.teams().expect("teams are on").board.clone();
    // Idle teammates are not woken by the board; the task waits for the next finished turn.
    root_turn(&built, "add a task").await;
    assert_eq!(board.list().len(), 1);
    assert!(tree.is_idle("agent-1"));
    let before = user_messages(&built).len();

    built
        .runtime
        .message_agent("agent-1", "go on".into())
        .unwrap();
    until("the teammate never settled", || {
        tree.is_idle("agent-1") && child.requests().len() >= 5
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        child.requests().len(),
        5,
        "first, the human's, and three hand-offs"
    );
    let notes: Vec<String> = child
        .requests()
        .iter()
        .map(|request| format!("{request:?}"))
        .collect();
    assert!(notes[2].contains("task t1 is available"), "{}", notes[2]);
    let recorded = user_messages(&built);
    assert_eq!(
        recorded.len(),
        before + 1,
        "only the human's own words: {recorded:?}"
    );
    assert!(
        recorded.iter().all(|m| !m.contains("is available")),
        "{recorded:?}"
    );
    assert!(tree.is_idle("agent-1"));
}

#[tokio::test]
async fn clear_and_rewind_are_refused_while_a_teammate_is_idle() {
    let w = world("");
    let (built, _child) = idle_teammate(&w, vec![], vec![]).await;
    assert!(matches!(
        built.runtime.clear_conversation().await,
        Err(ClearError::IdleTeammates)
    ));
    let plan = built.runtime.rewind_plan(1).unwrap();
    assert!(matches!(
        built.runtime.rewind(&plan, RewindWhat::Both, false).await,
        Err(RewindError::IdleTeammates)
    ));
    let tree = built.runtime.state.agents().clone();
    assert!(tree.is_idle("agent-1"));
    assert_eq!(tree.status_of("agent-1"), Some(ChildStatus::Done));
}

#[tokio::test]
async fn the_board_tools_exist_only_with_teams_on() {
    let off = World::new();
    off.user_file("[permissions]\nallow = [\"agent\"]\n[agents]\nteams = false\n");
    let (built, _) = build_keyed(&off, Manual, "x", vec![], vec![]);
    let names = built.runtime.tool_names();
    assert!(!names.iter().any(|n| n.starts_with("board_")), "{names:?}");

    let on = world("");
    let (built, _) = build_keyed(&on, Manual, "x", vec![], vec![]);
    let names = built.runtime.tool_names();
    assert!(names.contains(&"board_show".to_string()), "{names:?}");
    assert!(names.contains(&"board_update".to_string()), "{names:?}");
}
