//! A root turn's final answer is recorded as the agent's reply, which the guardian reads beside
//! the user's next message. Nothing else the agent says is.

mod common;

use common::World;
use common::runtime::{self, Built};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::ledger::{AGENT_REPLY_CHARS, Record};
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::ProviderResponseError;
use rig_core::completion::Usage;
use rig_core::test_utils::{MockError, MockStreamEvent, mock_final};
use serde_json::json;

fn answer_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::FinalResponse(mock_final(Usage::new())),
    ]
}

async fn run(
    built: &Built,
    flow: &mut (dyn FnMut(fontaine_core::turn::SessionEvent) -> Flow + Send),
) -> TurnOutcome {
    run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "go".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(5),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: None,
            max_tokens: None,
        },
        flow,
    )
    .await
}

fn replies(built: &Built) -> Vec<String> {
    built
        .runtime
        .state
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.record {
            Record::AgentReply { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_answered_turn_records_one_reply_and_only_its_tail() {
    let w = World::new();
    let long = format!("{}Shall I force-push?", "x".repeat(AGENT_REPLY_CHARS));
    let built = runtime::build(&w, Manual, vec![answer_turn(&long)], false);
    let outcome = run(&built, &mut |_| Flow::Continue).await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let recorded = replies(&built);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].chars().count(), AGENT_REPLY_CHARS);
    assert!(recorded[0].ends_with("Shall I force-push?"));
}

#[tokio::test]
async fn text_before_a_tool_call_is_not_a_reply() {
    let w = World::new();
    w.user_file("[permissions]\nallow = [\"shell\"]\n");
    let turns = vec![
        vec![
            MockStreamEvent::text("Let me check the branch first."),
            MockStreamEvent::ToolCall {
                id: "call-1".into(),
                name: "shell".into(),
                arguments: json!({ "command": "echo hi" }),
                call_id: None,
            },
            MockStreamEvent::FinalResponse(mock_final(Usage::new())),
        ],
        answer_turn("All done."),
    ];
    let built = runtime::build(&w, Manual, turns, false);
    run(&built, &mut |_| Flow::Continue).await;
    assert_eq!(replies(&built), vec!["All done."]);
}

#[tokio::test]
async fn a_blank_failed_or_stopped_turn_records_no_reply() {
    let w = World::new();
    let built = runtime::build(&w, Manual, vec![answer_turn("   \n")], false);
    let outcome = run(&built, &mut |_| Flow::Continue).await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    assert!(replies(&built).is_empty());

    let failing = vec![vec![MockStreamEvent::Error(MockError::ProviderResponse(
        ProviderResponseError::new(http::StatusCode::BAD_REQUEST, "boom"),
    ))]];
    let built = runtime::build(&w, Manual, failing, false);
    let outcome = run(&built, &mut |_| Flow::Continue).await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    assert!(replies(&built).is_empty());

    let built = runtime::build(&w, Manual, vec![answer_turn("never shown")], false);
    let outcome = run(&built, &mut |_| Flow::Stop).await;
    assert_eq!(outcome, TurnOutcome::Stopped);
    assert!(replies(&built).is_empty());
}

fn say(
    built: &Built,
    text: &str,
    origin: fontaine_core::ledger::MessageOrigin,
    agent: Option<&str>,
) {
    built.runtime.state.append(Record::UserMessage {
        origin,
        text: text.into(),
        attachments: Vec::new(),
        agent: agent.map(str::to_string),
    });
}

fn request_for(built: &Built, caller: &str) -> fontaine_guardian::AdjudicationRequest {
    fontaine_core::adjudication::build(
        &built.runtime.state,
        caller,
        "shell",
        &json!({ "command": "git push --force origin 584-x" }),
        Vec::new(),
        None,
    )
}

#[tokio::test]
async fn the_request_carries_the_reply_the_latest_prompt_answers() {
    use fontaine_core::ledger::MessageOrigin::{Other, Prompt};
    let w = World::new();
    let built = runtime::build(
        &w,
        Manual,
        vec![answer_turn("Shall I force-push 584-x?")],
        false,
    );
    say(&built, "rebase PR 584", Prompt, None);
    run(&built, &mut |_| Flow::Continue).await;
    assert_eq!(request_for(&built, "root").agent_message_before_reply, None);
    say(&built, "yes", Prompt, None);
    let expected = Some("Shall I force-push 584-x?".to_string());
    assert_eq!(
        request_for(&built, "root").agent_message_before_reply,
        expected
    );
    // A subagent whose last intent is that main message is judged with it too.
    assert_eq!(
        request_for(&built, "agent-1").agent_message_before_reply,
        expected
    );
    say(&built, "yes", Other, Some("agent-1"));
    assert_eq!(
        request_for(&built, "agent-1").agent_message_before_reply,
        None
    );
    assert_eq!(
        request_for(&built, "root").agent_message_before_reply,
        expected
    );
}

#[tokio::test]
async fn a_dialog_answer_is_left_out_of_the_screens_intent() {
    use fontaine_core::ledger::MessageOrigin::{Dialog, Prompt};
    let w = World::new();
    let built = runtime::build(&w, Manual, Vec::new(), false);
    say(&built, "rebase PR 584", Prompt, None);
    assert_eq!(request_for(&built, "root").screen_intent, None);
    let line = "Answered the agent's question \"Force-push?\": picked \"Yes\" (Overwrite origin)";
    say(&built, line, Dialog, None);
    let request = request_for(&built, "root");
    assert_eq!(request.recent_user_messages, vec!["rebase PR 584", line]);
    assert!(request.task_intent.contains(line));
    let screen = request
        .screen_intent
        .expect("a dialog answer is in the intent");
    assert_eq!(screen.task_intent, "rebase PR 584");
    assert_eq!(screen.recent_user_messages, vec!["rebase PR 584"]);
}
