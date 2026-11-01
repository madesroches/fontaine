//! A failed model call keeps its turn: the conversation store holds the interrupted task, and
//! a retryable transport failure is retried before the failure reaches the user.

mod common;

use common::World;
use common::runtime::{Built, build};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::session::CONVERSATION_LOG;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::ProviderResponseError;
use rig_core::completion::Usage;
use rig_core::test_utils::{MockError, MockStreamEvent, mock_final};

fn error_turn(status: u16) -> Vec<MockStreamEvent> {
    vec![MockStreamEvent::Error(MockError::ProviderResponse(
        ProviderResponseError::new(http::StatusCode::from_u16(status).unwrap(), "boom"),
    ))]
}

fn answer_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::FinalResponse(mock_final(Usage::new())),
    ]
}

async fn run(built: &Built, prompt: &str) -> TurnOutcome {
    run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: prompt.to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut |_| Flow::Continue,
    )
    .await
}

async fn stored(built: &Built) -> String {
    let conversation = rig_memory::load_async(&built.runtime.session.store(), CONVERSATION_LOG)
        .await
        .unwrap();
    format!("{:?}", conversation.messages())
}

#[tokio::test]
async fn a_failed_turn_keeps_its_prompt_in_the_conversation() {
    let w = World::new();
    let built = build(&w, Manual, vec![error_turn(400)], true);
    let outcome = run(&built, "hi").await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(built.main.request_count(), 1);
    let stored = stored(&built).await;
    assert!(stored.contains("hi"), "{stored}");
}

#[tokio::test]
async fn a_retryable_failure_is_retried_and_the_task_continues() {
    let w = World::new();
    let built = build(&w, Manual, vec![error_turn(429), answer_turn("done")], true);
    let outcome = run(&built, "hi").await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    assert_eq!(built.main.request_count(), 2);
    // The retry's request reads the failed attempt's prompt from the store and asks only to
    // go on: the continuation note is its new input, not a repeated user message.
    let requests = built.main.requests();
    let retry_history = format!("{:?}", requests[1].chat_history);
    assert!(retry_history.contains("hi"), "{retry_history}");
    assert!(
        retry_history.contains("continue the task"),
        "{retry_history}"
    );
    let stored = stored(&built).await;
    assert!(stored.contains("hi"), "{stored}");
    assert!(stored.contains("continue the task"), "{stored}");
}

#[tokio::test]
async fn a_non_retryable_failure_is_not_retried() {
    let w = World::new();
    let built = build(&w, Manual, vec![error_turn(400), answer_turn("done")], true);
    let outcome = run(&built, "hi").await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(built.main.request_count(), 1);
}

#[tokio::test]
async fn retries_run_out_and_the_failure_is_reported_once() {
    let w = World::new();
    let turns = vec![error_turn(429), error_turn(429), error_turn(429)];
    let built = build(&w, Manual, turns, true);
    let outcome = run(&built, "hi").await;
    let TurnOutcome::Failed(message) = outcome else {
        panic!("expected a failure, got {outcome:?}");
    };
    assert!(message.contains("boom"), "{message}");
    assert_eq!(built.main.request_count(), 3);
}
