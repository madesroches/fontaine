//! A turn whose provider answers a spent usage window: it is not retried with a backoff, it is
//! told apart from a throttle, and every model request waits at the pause's gate.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::build_with_models;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::{AccountKind, PermissionMode};
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::quota::UsageRole;
use fontaine_core::turn::{Flow, TurnFailure, TurnOutcome, TurnParams, run_turn};
use fontaine_core::usage_limit::UsageLimit;
use fontaine_core::usage_pause::PauseInfo;
use http::StatusCode;
use rig_core::ProviderResponseError;
use rig_core::completion::Usage;
use rig_core::test_utils::{MockCompletionModel, MockError, MockStreamEvent, mock_final};

const WINDOW: &str = r#"{"error":{"message":"You've reached your 5-hour usage limit.","type":"exceeded_current_quota_error"}}"#;
const THROTTLE: &str = r#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#;

struct Built {
    runtime: AgentRuntime,
    main: MockCompletionModel,
    _world: World,
}

fn answer() -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text("ok".to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

fn refusal(status: u16, body: &str) -> Vec<MockStreamEvent> {
    vec![MockStreamEvent::Error(MockError::ProviderResponse(
        ProviderResponseError::new(StatusCode::from_u16(status).unwrap(), body),
    ))]
}

fn build(turns: Vec<Vec<MockStreamEvent>>) -> Built {
    let world = World::new();
    world.user_file("[accounts.test]\nkind = \"anthropic\"\n");
    let main = MockCompletionModel::from_stream_turns(turns);
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("anthropic"),
        main.clone().into(),
    );
    let models = Models {
        main_chain: fontaine_core::model_roles::ModelChain::single(handle.reference.clone()),
        main: handle.clone(),
        main_listing: fontaine_core::models::Listing::default(),
        guardian: Some(GuardianModel::Ready(handle)),
        guardian_choice: GuardianChoice::MainFallback,
        notices: Vec::new(),
    };
    let runtime = build_with_models(
        &world,
        PermissionMode::Manual,
        models,
        None,
        true,
        Default::default(),
        None,
    );
    Built {
        runtime,
        main,
        _world: world,
    }
}

async fn turn(built: &Built) -> TurnOutcome {
    run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hello".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: None,
            max_tokens: None,
        },
        &mut |_| Flow::Continue,
    )
    .await
}

#[tokio::test]
async fn a_spent_window_settles_the_turn_at_once_without_a_backoff_retry() {
    let built = build(vec![refusal(429, WINDOW)]);
    let TurnOutcome::Failed(failure) = turn(&built).await else {
        panic!("expected a failure");
    };
    assert_eq!(
        built.main.request_count(),
        1,
        "a spent window is not retried"
    );
    let limit = failure.usage_limit.expect("classified as a usage limit");
    assert_eq!(
        limit.window.as_deref(),
        Some("exceeded_current_quota_error")
    );
    assert!(failure.falls_through, "a model list still moves on first");
}

#[tokio::test]
async fn a_throttle_is_still_retried_and_is_not_a_usage_limit() {
    let built = build(vec![
        refusal(429, THROTTLE),
        refusal(429, THROTTLE),
        refusal(429, THROTTLE),
    ]);
    let TurnOutcome::Failed(TurnFailure { usage_limit, .. }) = turn(&built).await else {
        panic!("expected a failure");
    };
    assert_eq!(
        built.main.request_count(),
        3,
        "the normal retry with a backoff"
    );
    assert_eq!(usage_limit, None);
}

#[tokio::test]
async fn a_request_waits_at_the_gate_while_a_pause_is_in_force_and_goes_after_it() {
    let built = build(vec![answer()]);
    let pause = built.runtime.state.usage_pause().clone();
    pause.engage(PauseInfo::new(UsageRole::Main, "test", None));
    let run = turn(&built);
    tokio::pin!(run);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut run)
            .await
            .is_err(),
        "the turn is held"
    );
    assert_eq!(
        built.main.request_count(),
        0,
        "nothing was sent while paused"
    );
    pause.resume();
    let TurnOutcome::Answer(text) = run.await else {
        panic!("expected an answer");
    };
    assert_eq!(text, "ok");
    assert_eq!(built.main.request_count(), 1);
}

#[tokio::test]
async fn cancelling_the_wait_fails_the_turn_with_the_account_and_never_sends_the_request() {
    let built = build(vec![answer()]);
    let pause = built.runtime.state.usage_pause().clone();
    pause.engage(PauseInfo::new(UsageRole::Main, "test", None));
    let run = turn(&built);
    tokio::pin!(run);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut run)
            .await
            .is_err()
    );
    pause.cancel();
    let TurnOutcome::Failed(failure) = run.await else {
        panic!("expected a failure");
    };
    assert!(failure.message.contains("test"), "{}", failure.message);
    assert!(
        failure.message.contains("usage window"),
        "{}",
        failure.message
    );
    assert_eq!(built.main.request_count(), 0);
}

#[tokio::test]
async fn a_spent_list_pauses_then_the_request_goes_again_after_the_window_opens() {
    use fontaine_core::turn::wait_out_usage_limit;
    let built = build(vec![answer()]);
    let state = built.runtime.state.clone();
    let mut failure = TurnFailure {
        usage_limit: Some(UsageLimit {
            reset_at: None,
            window: None,
        }),
        ..TurnFailure::new("spent")
    };
    let pause = state.usage_pause().clone();
    let waiting = wait_out_usage_limit(&state, &mut failure, UsageRole::Main, "test", None);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err()
    );
    assert!(pause.is_paused());
    pause.resume();
    assert!(waiting.await, "the caller goes again");

    // Cancelled, it fails with the usage-limit message and keeps the end.
    let mut failure = TurnFailure {
        usage_limit: Some(UsageLimit {
            reset_at: None,
            window: None,
        }),
        ..TurnFailure::new("spent")
    };
    {
        let waiting = wait_out_usage_limit(&state, &mut failure, UsageRole::Main, "test", None);
        tokio::pin!(waiting);
        let _ = tokio::time::timeout(Duration::from_millis(100), &mut waiting).await;
        pause.cancel();
        assert!(!waiting.await);
    }
    assert!(
        failure.message.contains("usage window"),
        "{}",
        failure.message
    );
    assert_eq!(failure.usage_limit, None);
}
