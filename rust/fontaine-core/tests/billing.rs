//! Cost a call reports is dropped where usage enters when the model is billed outside
//! per-token prices.

mod common;

use common::World;
use common::runtime::build;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::turn::{Drive, Flow, SessionEvent, TurnOutcome, TurnParams, drive, run_turn};
use rig_core::completion::{Cost, Usage};
use rig_core::test_utils::{MockStreamEvent, mock_final};
use serde_json::json;

fn costed_answer() -> Vec<MockStreamEvent> {
    let cost: Cost = serde_json::from_value(json!({"total": 0.25})).unwrap();
    let usage = Usage::new().input_tokens(10).output_tokens(5).cost(cost);
    vec![
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ]
}

async fn root_turn(priced: bool) -> (Option<f64>, Option<f64>) {
    let w = World::new();
    let built = build(&w, Manual, vec![costed_answer()], true);
    let mut reported = None;
    let mut on_event = |event: SessionEvent| {
        if let SessionEvent::Usage { usage, .. } = event {
            reported = Some(usage.cost.map(|c| c.total));
        }
        Flow::Continue
    };
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            max_turns: Some(3),
            priced,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut on_event,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let spent = built
        .runtime
        .state
        .agents()
        .spend_of(fontaine_tools::gate::ROOT_AGENT)
        .cost_usd;
    (reported.expect("a usage event"), spent)
}

#[tokio::test]
async fn a_priced_turn_counts_the_cost_the_call_reported() {
    assert_eq!(root_turn(true).await, (Some(0.25), Some(0.25)));
}

#[tokio::test]
async fn an_unpriced_turn_reports_usage_and_spend_without_a_cost() {
    assert_eq!(root_turn(false).await, (None, None));
}

#[tokio::test]
async fn an_unpriced_subagent_adds_tokens_but_no_cost_to_the_subagent_total() {
    let w = World::new();
    let built = build(&w, Manual, vec![costed_answer()], true);
    let mut seen = |_| Flow::Continue;
    let outcome = drive(
        Drive {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            agent_id: "child-1",
            prompt: "hi".to_string(),
            max_turns: Some(3),
            priced: false,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            span: tracing::Span::none(),
        },
        None,
        &mut seen,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let total = built.runtime.state.subagent_usage();
    assert_eq!(total.input_tokens, 10);
    assert_eq!(total.cost_usd, None);
}

fn reasoned_answer() -> Vec<MockStreamEvent> {
    let usage = Usage::new().input_tokens(10).output_tokens(5);
    vec![
        MockStreamEvent::ReasoningDelta {
            id: "r1".to_string(),
            reasoning: "thinking".to_string(),
        },
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ]
}

async fn writing_during_a_step(stop_at_delta: bool) -> (Vec<u64>, u64) {
    let w = World::new();
    let built = build(&w, Manual, vec![reasoned_answer()], true);
    let agents = built.runtime.state.agents();
    let mut seen = Vec::new();
    let mut on_event = |event: SessionEvent| {
        if matches!(event, SessionEvent::AssistantDelta(_)) {
            seen.push(agents.writing_of(fontaine_tools::gate::ROOT_AGENT));
            if stop_at_delta {
                return Flow::Stop;
            }
        }
        Flow::Continue
    };
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut on_event,
    )
    .await;
    if stop_at_delta {
        assert!(matches!(outcome, TurnOutcome::Stopped), "{outcome:?}");
    } else {
        assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    }
    (seen, agents.writing_of(fontaine_tools::gate::ROOT_AGENT))
}

#[tokio::test]
async fn reasoning_counts_as_writing_mid_step_and_clears_after_the_call() {
    let (seen, after) = writing_during_a_step(false).await;
    assert_eq!(seen, vec!["thinking".len() as u64 + "done".len() as u64]);
    assert_eq!(after, 0);
}

#[tokio::test]
async fn a_stopped_step_leaves_no_writing_behind() {
    let (seen, after) = writing_during_a_step(true).await;
    assert!(seen[0] > 0);
    assert_eq!(after, 0);
}
