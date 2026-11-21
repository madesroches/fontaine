//! Cost a call reports is dropped where usage enters when the model is billed outside
//! per-token prices.

#[path = "core_common/mod.rs"]
mod common;

use common::World;
use common::runtime::{build, build_with_document};
use fontaine_sdk::agents::budget::Spend;
use fontaine_sdk::core::config::PermissionMode::Manual;
use fontaine_sdk::core::models::CostBasis;
use fontaine_sdk::core::turn::{
    Drive, Flow, SessionEvent, TurnOutcome, TurnParams, drive, run_turn,
};
use fontaine_sdk::core::usage_ledger::{Bucket, UsageSource};
use rig_core::completion::{Cost, Usage};
use rig_core::test_utils::{MockError, MockStreamEvent, mock_final};
use serde_json::json;

fn costed_answer() -> Vec<MockStreamEvent> {
    let cost: Cost = serde_json::from_value(json!({"total": 0.25})).unwrap();
    let usage = Usage::new().input_tokens(10).output_tokens(5).cost(cost);
    vec![
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ]
}

async fn root_turn(cost: CostBasis) -> (Option<f64>, Spend, Bucket) {
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
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut on_event,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let spent = built
        .runtime
        .state
        .agents()
        .spend_of(fontaine_sdk::tools::gate::ROOT_AGENT);
    let bucket = built.runtime.state.usage_ledger().bucket(UsageSource::Main);
    (reported.expect("a usage event"), spent, bucket)
}

#[tokio::test]
async fn a_priced_turn_counts_the_cost_the_call_reported() {
    let (reported, spent, _) = root_turn(CostBasis::Billed).await;
    assert_eq!(reported, Some(0.25));
    assert_eq!(spent.cost_usd, Some(0.25));
    assert!(!spent.estimated);
}

#[tokio::test]
async fn an_unpriced_turn_reports_usage_and_spend_without_a_cost() {
    let (reported, spent, _) = root_turn(CostBasis::Unpriced).await;
    assert_eq!(reported, None);
    assert_eq!(spent.cost_usd, None);
    assert!(!spent.estimated);
}

#[tokio::test]
async fn an_estimated_turn_keeps_the_cost_and_marks_the_spend_and_the_bucket() {
    let (reported, spent, bucket) = root_turn(CostBasis::Estimated).await;
    assert_eq!(reported, Some(0.25));
    assert_eq!(spent.cost_usd, Some(0.25));
    assert!(spent.estimated);
    assert_eq!(bucket.cost_usd, Some(0.25));
    assert!(bucket.estimated);
}

#[tokio::test]
async fn an_unpriced_subagent_adds_tokens_but_no_cost_to_the_subagent_total() {
    let w = World::new();
    let built = build(&w, Manual, vec![costed_answer()], true);
    let mut seen = |_| Flow::Continue;
    let outcome = drive(
        Drive {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            agent_id: "child-1",
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Unpriced,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
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
            seen.push(agents.writing_of(fontaine_sdk::tools::gate::ROOT_AGENT));
            if stop_at_delta {
                return Flow::Stop;
            }
        }
        Flow::Continue
    };
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Billed,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut on_event,
    )
    .await;
    if stop_at_delta {
        assert!(matches!(outcome, TurnOutcome::Stopped), "{outcome:?}");
    } else {
        assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    }
    (
        seen,
        agents.writing_of(fontaine_sdk::tools::gate::ROOT_AGENT),
    )
}

#[tokio::test]
async fn reasoning_counts_as_writing_mid_step_and_clears_after_the_call() {
    let (seen, after) = writing_during_a_step(false).await;
    assert_eq!(seen, vec!["thinking".len() as u64 + "done".len() as u64]);
    assert_eq!(after, 0);
}

#[tokio::test]
async fn a_failed_step_leaves_no_writing_behind() {
    let w = World::new();
    let turn = vec![
        MockStreamEvent::ReasoningDelta {
            id: "r1".to_string(),
            reasoning: "thinking".to_string(),
        },
        MockStreamEvent::Error(MockError::provider("stream broke")),
    ];
    let built = build(&w, Manual, vec![turn], true);
    let agents = built.runtime.state.agents();
    let mut on_event = |_: SessionEvent| Flow::Continue;
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Billed,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut on_event,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(agents.writing_of(fontaine_sdk::tools::gate::ROOT_AGENT), 0);
}

#[tokio::test]
async fn a_stopped_step_leaves_no_writing_behind() {
    let (seen, after) = writing_during_a_step(true).await;
    assert!(seen[0] > 0);
    assert_eq!(after, 0);
}

fn argument_call() -> Vec<MockStreamEvent> {
    let usage = Usage::new().input_tokens(10).output_tokens(5);
    vec![
        MockStreamEvent::ToolCallNameDelta {
            id: "call-1".to_string(),
            name: "read".to_string(),
        },
        MockStreamEvent::ToolCallArgumentsDelta {
            id: "call-1".to_string(),
            arguments: "{\"path\":".to_string(),
        },
        MockStreamEvent::ToolCallArgumentsDelta {
            id: "call-1".to_string(),
            arguments: "\"a.txt\"}".to_string(),
        },
        MockStreamEvent::ToolCallEnd {
            id: "call-1".to_string(),
        },
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ]
}

fn plain_answer() -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::new())),
    ]
}

async fn writing_across_a_tool_call() -> Vec<u64> {
    let w = World::new();
    let built = build(&w, Manual, vec![argument_call(), plain_answer()], true);
    let agents = built.runtime.state.agents();
    let mut at_tool = Vec::new();
    let mut on_event = |event: SessionEvent| {
        let writing = agents.writing_of(fontaine_sdk::tools::gate::ROOT_AGENT);
        match event {
            SessionEvent::ToolCall { .. } | SessionEvent::ToolResult { .. } => {
                at_tool.push(writing)
            }
            _ => {}
        }
        Flow::Continue
    };
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Billed,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut on_event,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    at_tool
}

#[tokio::test]
async fn a_finished_request_has_no_writing_when_its_tool_runs() {
    let at_tool = writing_across_a_tool_call().await;
    assert!(!at_tool.is_empty());
    assert!(at_tool.iter().all(|w| *w == 0), "{at_tool:?}");
}

#[tokio::test]
async fn argument_characters_count_toward_writing_at_the_next_delta() {
    let w = World::new();
    let mut turn = argument_call();
    let last = turn.pop().expect("a final response");
    turn.push(MockStreamEvent::Text("done".to_string()));
    turn.push(last);
    let built = build(&w, Manual, vec![turn, plain_answer()], true);
    let agents = built.runtime.state.agents();
    let mut seen = Vec::new();
    let mut on_event = |event: SessionEvent| {
        if matches!(event, SessionEvent::AssistantDelta(_)) {
            seen.push(agents.writing_of(fontaine_sdk::tools::gate::ROOT_AGENT));
        }
        Flow::Continue
    };
    run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Billed,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut on_event,
    )
    .await;
    let arguments = "{\"path\":\"a.txt\"}".len() as u64;
    assert_eq!(seen[0], arguments + "done".len() as u64);
}

async fn reasoning_events(turn: Vec<MockStreamEvent>) -> Vec<String> {
    let w = World::new();
    let built = build(&w, Manual, vec![turn], true);
    let mut seen = Vec::new();
    let mut on_event = |event: SessionEvent| {
        match event {
            SessionEvent::ReasoningDelta(text) => seen.push(format!("reasoning delta {text}")),
            SessionEvent::Reasoning(text) => seen.push(format!("reasoning {text}")),
            SessionEvent::AssistantDelta(text) => seen.push(format!("delta {text}")),
            SessionEvent::Assistant(text) => seen.push(format!("assistant {text}")),
            _ => {}
        }
        Flow::Continue
    };
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Billed,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut on_event,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    seen
}

fn reasoning_delta(text: &str) -> MockStreamEvent {
    MockStreamEvent::ReasoningDelta {
        id: "r1".to_string(),
        reasoning: text.to_string(),
    }
}

#[tokio::test]
async fn reasoning_streams_then_settles_before_the_answer() {
    let usage = Usage::new().input_tokens(10).output_tokens(5);
    let seen = reasoning_events(vec![
        reasoning_delta("let me "),
        reasoning_delta("think"),
        MockStreamEvent::Reasoning {
            id: "r1".to_string(),
            text: "let me think".to_string(),
        },
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ])
    .await;
    assert_eq!(
        seen,
        [
            "reasoning delta let me ",
            "reasoning delta think",
            "reasoning let me think",
            "delta done",
            "assistant done",
        ]
    );
}

#[tokio::test]
async fn reasoning_that_settles_after_the_text_arrives_last() {
    let usage = Usage::new().input_tokens(10).output_tokens(5);
    let seen = reasoning_events(vec![
        reasoning_delta("hm"),
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ])
    .await;
    assert_eq!(
        seen,
        [
            "reasoning delta hm",
            "delta done",
            "assistant done",
            "reasoning hm",
        ]
    );
}

async fn ledger_after_a_turn(
    document: Option<serde_json::Value>,
) -> fontaine_sdk::core::usage_ledger::Bucket {
    let w = World::new();
    let usage = Usage::new()
        .input_tokens(10)
        .output_tokens(5)
        .cache_creation_input_tokens(12u64);
    let turn = vec![
        MockStreamEvent::Text("done".to_string()),
        MockStreamEvent::FinalResponse(mock_final(usage)),
    ];
    let built = match document {
        Some(document) => build_with_document(&w, Manual, vec![turn], document),
        None => build(&w, Manual, vec![turn], true),
    };
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hi".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            cost: CostBasis::Billed,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            deferral: None,
            generation_options: None,
            max_tokens: None,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    built.runtime.state.usage_ledger().bucket(UsageSource::Main)
}

#[tokio::test]
async fn a_reply_that_splits_its_cache_writes_lands_in_the_split_buckets() {
    let bucket = ledger_after_a_turn(Some(json!({"usage": {"cache_creation": {
        "ephemeral_5m_input_tokens": 2, "ephemeral_1h_input_tokens": 10}}})))
    .await;
    assert_eq!(bucket.cache_creation_input_tokens, 12);
    assert_eq!(bucket.cache_write_5m_tokens, 2);
    assert_eq!(bucket.cache_write_1h_tokens, 10);
}

#[tokio::test]
async fn a_reply_without_a_split_leaves_the_split_buckets_zero() {
    let bucket = ledger_after_a_turn(Some(json!({"usage": {"input_tokens": 10}}))).await;
    assert_eq!(bucket.cache_creation_input_tokens, 12);
    assert_eq!(bucket.cache_write_5m_tokens, 0);
    assert_eq!(bucket.cache_write_1h_tokens, 0);
    let bucket = ledger_after_a_turn(None).await;
    assert_eq!(bucket.cache_creation_input_tokens, 12);
    assert_eq!(bucket.cache_write_5m_tokens, 0);
    assert_eq!(bucket.cache_write_1h_tokens, 0);
}
