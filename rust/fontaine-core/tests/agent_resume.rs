//! Typing to a subagent after it ended: the same conversation continues in another run, the
//! ledger records the resume before the words, and a kill, a spent budget or a full tree refuse.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::{Built, build, build_keyed};
use fontaine_agents::mode::Ladder;
use fontaine_agents::runner::ChildStatus;
use fontaine_core::agent::{Delivered, MessageRefusal};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::gate::{AgentEventSink, AgentViewEvent};
use fontaine_core::turn::{Flow, SessionEvent, TurnOutcome, TurnParams, run_turn};
use fontaine_tools::gate::ROOT_AGENT;
use rig_core::completion::Usage;
use rig_core::test_utils::{MockStreamEvent, mock_final};
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

fn spawn_call() -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: "call-agent".into(),
            name: "agent".into(),
            arguments: json!({"agent_type": "worker", "prompt": "dig into it"}),
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(usage())),
    ]
}

/// A world that lets the main conversation start subagents unasked.
fn world(extra: &str) -> World {
    let w = World::new();
    w.user_file(&format!("[permissions]\nallow = [\"agent\"]\n{extra}"));
    w
}

/// What the tests with a child get: the runtime with its main conversation, and the model the
/// child talks to.
struct OneChild {
    built: Built,
    child: rig_core::test_utils::MockCompletionModel,
}

/// Runs the root turn that starts a child, then waits for the child to answer "child answer".
/// The child's report waits in the root's inbox.
async fn with_one_child(w: &World, later: Vec<Vec<MockStreamEvent>>) -> OneChild {
    with_one_child_seen(w, later, None).await
}

/// [`with_one_child`], with the sink the child's events go to installed first.
async fn with_one_child_seen(
    w: &World,
    later: Vec<Vec<MockStreamEvent>>,
    sink: Option<AgentEventSink>,
) -> OneChild {
    let mut child_turns = vec![text("child answer")];
    child_turns.extend(later);
    let (built, child) = build_keyed(
        w,
        Manual,
        "dig into it",
        vec![spawn_call(), text("root done")],
        child_turns,
    );
    if let Some(sink) = sink {
        built.runtime.state.set_agent_event_sink(sink);
    }
    built.runtime.begin_turn("start it", false);
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "start it".to_string(),
            max_turns: 5,
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    until_ended(&built, "agent-1").await;
    OneChild { built, child }
}

async fn until_ended(built: &Built, id: &str) {
    let tree = built.runtime.state.agents();
    for _ in 0..500 {
        if !tree.is_running(id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{id} never ended");
}

#[tokio::test]
async fn a_done_child_resumed_with_text_continues_its_conversation_and_reports_to_the_root() {
    let w = world("");
    let OneChild { built, child } = with_one_child(&w, vec![text("resumed answer")]).await;
    let tree = built.runtime.state.agents().clone();
    assert_eq!(tree.status_of("agent-1"), Some(ChildStatus::Done));
    let spent = tree.spend_of("agent-1").tokens();
    assert!(spent > 0);
    // The first run's report waits for the root.
    let first = tree.take_notifications(ROOT_AGENT);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].text.contains("child answer"), "{first:?}");

    let delivered = built
        .runtime
        .message_agent("agent-1", "and the words".into())
        .unwrap();
    assert_eq!(delivered, Delivered::Resumed);
    assert_eq!(tree.status_of("agent-1"), Some(ChildStatus::Running));
    until_ended(&built, "agent-1").await;

    // The scripted model saw the earlier turns of that conversation with the new words.
    let last = format!("{:?}", child.requests().last().unwrap());
    assert!(last.contains("dig into it"), "{last}");
    assert!(last.contains("child answer"), "{last}");
    assert!(last.contains("and the words"), "{last}");
    // Spend accumulates against the same cap.
    assert!(tree.spend_of("agent-1").tokens() > spent);
    // The second run's report reaches the root as a notification.
    assert!(tree.has_notifications(ROOT_AGENT));
    let notes = tree.take_notifications(ROOT_AGENT);
    assert!(notes[0].text.contains("resumed answer"), "{notes:?}");

    // Resumed first, then the words, the latter intent for that agent alone.
    let records = built.runtime.session.ledger.read().unwrap();
    let kinds: Vec<String> = records
        .iter()
        .filter_map(|entry| match &entry.record {
            fontaine_core::ledger::Record::Agent { event, .. } if event == "resumed" => {
                Some("resumed".to_string())
            }
            fontaine_core::ledger::Record::UserMessage {
                text,
                agent: Some(agent),
            } => Some(format!("{agent}: {text}")),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["resumed", "agent-1: and the words"]);
    assert!(
        built
            .runtime
            .session
            .ledger
            .view()
            .user_messages
            .iter()
            .all(|m| m != "and the words")
    );
}

#[tokio::test]
async fn a_killed_or_budget_spent_child_cannot_be_resumed() {
    let w = world("\n[agents]\nsubagent_token_budget = 1000\n");
    let built = build(&w, Manual, vec![], false);
    let tree = built.runtime.state.agents().clone();

    let killed = tree
        .reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
        .unwrap();
    assert!(tree.kill(&killed));
    assert_eq!(
        built.runtime.message_agent(&killed, "hello".into()),
        Err(MessageRefusal::Ended(ChildStatus::Killed))
    );

    let spent = tree
        .reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
        .unwrap();
    tree.finish(
        &spent,
        fontaine_agents::runner::ChildReport {
            status: ChildStatus::Done,
            text: "done".into(),
            reason: None,
        },
    );
    tree.debit(
        &spent,
        &fontaine_agents::budget::Spend {
            input_tokens: 5000,
            output_tokens: 0,
            cost_usd: None,
        },
    );
    // Done, but its budget is spent: whatever the status says.
    assert_eq!(
        built.runtime.message_agent(&spent, "hello".into()),
        Err(MessageRefusal::BudgetSpent)
    );
    assert_eq!(
        built.runtime.message_agent("agent-99", "hello".into()),
        Err(MessageRefusal::Unknown)
    );
    assert!(
        MessageRefusal::Ended(ChildStatus::Killed)
            .notice("agent-3")
            .contains("agent-3 was killed; it cannot be resumed")
    );
}

#[tokio::test]
async fn a_resume_at_the_parallel_limit_is_refused_with_a_human_notice() {
    let w = world("\n[agents]\nmax_parallel = 1\n");
    let OneChild { built, .. } = with_one_child(&w, vec![]).await;
    let tree = built.runtime.state.agents().clone();
    // Another agent holds the only place.
    tree.reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
        .unwrap();
    let refusal = built
        .runtime
        .message_agent("agent-1", "more".into())
        .unwrap_err();
    assert!(matches!(refusal, MessageRefusal::Limit(_)), "{refusal:?}");
    assert!(refusal.notice("agent-1").contains("max_parallel = 1"));
    assert_eq!(tree.status_of("agent-1"), Some(ChildStatus::Done));
}

#[tokio::test]
async fn a_message_for_a_running_child_is_queued_not_resumed() {
    let w = world("");
    let built = build(&w, Manual, vec![], false);
    let tree = built.runtime.state.agents().clone();
    let running = tree
        .reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
        .unwrap();
    assert_eq!(
        built.runtime.message_agent(&running, "heads up".into()),
        Ok(Delivered::Queued)
    );
    assert_eq!(built.runtime.steering.queued_for(&running), ["heads up"]);
    // Nothing is in the ledger until the model reads it.
    assert!(
        built
            .runtime
            .session
            .ledger
            .view()
            .agent_messages
            .is_empty()
    );
}

#[tokio::test]
async fn a_childs_prompt_and_events_reach_the_sink_under_its_id() {
    let w = world("");
    let (sink, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let OneChild { built, .. } =
        with_one_child_seen(&w, vec![text("resumed answer")], Some(sink)).await;
    let mut events = Vec::new();
    while let Ok(item) = seen.try_recv() {
        events.push(item);
    }
    assert!(
        events.iter().all(|(agent, _)| agent == "agent-1"),
        "{events:?}"
    );
    // The view opens on the task as the parent wrote it.
    assert!(matches!(
        &events[0].1,
        AgentViewEvent::Prompt(task) if task == "dig into it"
    ));
    assert!(events.iter().any(|(_, event)| matches!(
        event,
        AgentViewEvent::Event(SessionEvent::Assistant(reply)) if reply == "child answer"
    )));

    // A resume opens its own prompt: the words the human typed.
    built
        .runtime
        .message_agent("agent-1", "and the words".into())
        .unwrap();
    until_ended(&built, "agent-1").await;
    let mut later = Vec::new();
    while let Ok(item) = seen.try_recv() {
        later.push(item);
    }
    assert!(matches!(
        &later[0].1,
        AgentViewEvent::Prompt(task) if task == "and the words"
    ));
    assert!(later.iter().any(|(_, event)| matches!(
        event,
        AgentViewEvent::Event(SessionEvent::Assistant(reply)) if reply == "resumed answer"
    )));
}
