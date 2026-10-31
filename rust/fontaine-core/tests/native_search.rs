//! The provider's own search tool: offered on the request only when the account supports it and
//! the live mode and rules allow it, and kept in line with them between turns.

mod common;

use common::World;
use common::runtime::build_with_models;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::{AccountKind, PermissionMode};
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::permission_rules::{RuleSource, parse_rule};
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use http::StatusCode;
use rig_core::ProviderResponseError;
use rig_core::completion::Usage;
use rig_core::test_utils::{MockCompletionModel, MockError, MockStreamEvent, mock_final};
use serde_json::{Value, json};

fn answer() -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text("ok".to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

fn entry() -> Value {
    json!({"type": "web_search_20250305", "name": "web_search"})
}

struct Built {
    runtime: AgentRuntime,
    main: MockCompletionModel,
}

fn world(account: &str) -> World {
    let w = World::new();
    w.user_file(account);
    w
}

fn build(w: &World, mode: PermissionMode, kind: &str, turns: Vec<Vec<MockStreamEvent>>) -> Built {
    let main = MockCompletionModel::from_stream_turns(turns);
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named(kind),
        main.clone().into(),
    );
    let models = Models {
        main: handle.clone(),
        main_listing: Vec::new(),
        guardian: Some(GuardianModel::Ready(handle)),
        guardian_choice: GuardianChoice::MainFallback,
        notices: Vec::new(),
    };
    let runtime = build_with_models(w, mode, models, None, true, Default::default(), None);
    Built { runtime, main }
}

async fn turn(built: &Built) -> TurnOutcome {
    run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hello".to_string(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: built.runtime.native_search_active(),
        },
        &mut |_| Flow::Continue,
    )
    .await
}

/// The `tools` entries the last request carried through `additional_params`.
fn sent_tools(built: &Built) -> Vec<Value> {
    built
        .main
        .requests()
        .last()
        .and_then(|r| r.additional_params.clone())
        .and_then(|p| p.get("tools").cloned())
        .and_then(|t| t.as_array().cloned())
        .unwrap_or_default()
}

const ANTHROPIC: &str = "[accounts.test]\nkind = \"anthropic\"\n";

#[tokio::test]
async fn auto_mode_on_the_anthropic_api_carries_the_search_tool() {
    let w = world(ANTHROPIC);
    let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(built.runtime.native_search_active());
    turn(&built).await;
    assert_eq!(sent_tools(&built), vec![entry()]);
}

#[tokio::test]
async fn manual_mode_needs_an_allow_rule_and_a_grant_takes_effect_on_the_next_turn() {
    let w = world(ANTHROPIC);
    let mut built = build(
        &w,
        PermissionMode::Manual,
        "anthropic",
        vec![answer(), answer()],
    );
    assert!(!built.runtime.native_search_active());
    turn(&built).await;
    assert!(sent_tools(&built).is_empty());

    built
        .runtime
        .state
        .grant(parse_rule("websearch").unwrap(), RuleSource::Session);
    built.runtime.refresh_native_search().unwrap();
    assert!(built.runtime.native_search_active());
    turn(&built).await;
    assert_eq!(sent_tools(&built), vec![entry()]);
}

#[tokio::test]
async fn a_mode_switch_adds_and_removes_it_on_the_next_turn() {
    let w = world(ANTHROPIC);
    let mut built = build(
        &w,
        PermissionMode::Auto,
        "anthropic",
        vec![answer(), answer(), answer()],
    );
    turn(&built).await;
    assert_eq!(sent_tools(&built), vec![entry()]);

    built.runtime.state.set_mode(PermissionMode::Manual);
    built.runtime.refresh_native_search().unwrap();
    turn(&built).await;
    assert!(sent_tools(&built).is_empty());

    built.runtime.state.set_mode(PermissionMode::Plan);
    built.runtime.refresh_native_search().unwrap();
    turn(&built).await;
    assert_eq!(sent_tools(&built), vec![entry()]);
}

#[tokio::test]
async fn an_unchanged_gate_does_not_rebuild_the_agent() {
    let w = world(ANTHROPIC);
    common::write(&w.work.join("FONTAINE.md"), "marker-v1\n");
    let mut built = build(
        &w,
        PermissionMode::Auto,
        "anthropic",
        vec![answer(), answer()],
    );
    // A rebuild re-reads the instruction files, so a changed file shows whether one happened.
    common::write(&w.work.join("FONTAINE.md"), "marker-v2\n");
    built.runtime.refresh_native_search().unwrap();
    turn(&built).await;
    let sent = format!("{:?}", built.main.requests().last().unwrap());
    assert!(
        sent.contains("marker-v1") && !sent.contains("marker-v2"),
        "{sent}"
    );

    built.runtime.state.set_mode(PermissionMode::Manual);
    built.runtime.refresh_native_search().unwrap();
    turn(&built).await;
    let sent = format!("{:?}", built.main.requests().last().unwrap());
    assert!(
        sent.contains("marker-v2"),
        "a changed gate rebuilds: {sent}"
    );
}

#[tokio::test]
async fn a_deny_or_ask_rule_withholds_it_even_with_an_allow_rule() {
    for rule in ["deny", "ask"] {
        let w = world(&format!(
            "{ANTHROPIC}[permissions]\n{rule} = [\"websearch\"]\nallow = [\"websearch\"]\n"
        ));
        for mode in [PermissionMode::Auto, PermissionMode::Plan] {
            let built = build(&w, mode, "anthropic", vec![answer()]);
            assert!(!built.runtime.native_search_active(), "{rule} {mode:?}");
            turn(&built).await;
            assert!(sent_tools(&built).is_empty(), "{rule} {mode:?}");
        }
    }
}

#[tokio::test]
async fn a_pre_tool_use_hook_covering_it_withholds_it() {
    for matcher in ["matcher = \"websearch\"\n", ""] {
        let w = world(&format!(
            "{ANTHROPIC}[[hooks.pre_tool_use]]\n{matcher}command = \"true\"\n"
        ));
        let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
        assert!(!built.runtime.native_search_active(), "{matcher:?}");
        turn(&built).await;
        assert!(sent_tools(&built).is_empty(), "{matcher:?}");
    }
}

#[tokio::test]
async fn a_pre_tool_use_hook_for_another_tool_does_not() {
    let w = world(&format!(
        "{ANTHROPIC}[[hooks.pre_tool_use]]\nmatcher = \"shell\"\ncommand = \"true\"\n"
    ));
    let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(built.runtime.native_search_active());
}

#[tokio::test]
async fn other_account_kinds_never_carry_it() {
    let w = world("[accounts.test]\nkind = \"openai\"\n");
    let built = build(&w, PermissionMode::Auto, "openai", vec![answer()]);
    assert!(!built.runtime.native_search_active());
    turn(&built).await;
    assert!(sent_tools(&built).is_empty());
}

#[tokio::test]
async fn a_host_other_than_the_anthropic_api_needs_native_on() {
    let proxy =
        "[accounts.test]\nkind = \"anthropic\"\nbase_url = \"https://gateway.example.com\"\n";
    let w = world(proxy);
    let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(!built.runtime.native_search_active());

    let w = world(&format!("{proxy}[websearch]\nnative = \"on\"\n"));
    let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(built.runtime.native_search_active());
    turn(&built).await;
    assert_eq!(sent_tools(&built), vec![entry()]);

    let official =
        "[accounts.test]\nkind = \"anthropic\"\nbase_url = \"https://api.anthropic.com/\"\n";
    let w = world(official);
    let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(built.runtime.native_search_active());
}

#[tokio::test]
async fn native_off_never_carries_it() {
    let w = world(&format!("{ANTHROPIC}[websearch]\nnative = \"off\"\n"));
    let built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(!built.runtime.native_search_active());
    turn(&built).await;
    assert!(sent_tools(&built).is_empty());
}

fn rejected(status: u16, body: &str) -> Vec<Vec<MockStreamEvent>> {
    vec![vec![MockStreamEvent::Error(MockError::ProviderResponse(
        ProviderResponseError::new(StatusCode::from_u16(status).unwrap(), body),
    ))]]
}

#[tokio::test]
async fn a_400_naming_web_search_gets_the_hint_and_is_not_retried() {
    let w = world(ANTHROPIC);
    let built = build(
        &w,
        PermissionMode::Auto,
        "anthropic",
        rejected(
            400,
            r#"{"error":{"message":"tools.0: web_search_20250305 is not supported"}}"#,
        ),
    );
    let TurnOutcome::Failed(text) = turn(&built).await else {
        panic!("expected a failure");
    };
    assert!(text.contains("native = \"off\""), "{text}");
    assert_eq!(built.main.request_count(), 1, "no retry");
}

#[tokio::test]
async fn a_400_naming_web_search_gets_no_hint_when_it_was_not_offered() {
    let w = world(&format!("{ANTHROPIC}[websearch]\nnative = \"off\"\n"));
    let built = build(
        &w,
        PermissionMode::Auto,
        "anthropic",
        rejected(400, "messages.2: web_search_tool_result is not supported"),
    );
    let TurnOutcome::Failed(text) = turn(&built).await else {
        panic!("expected a failure");
    };
    assert!(!text.contains("native"), "{text}");
}

#[tokio::test]
async fn other_failures_keep_their_text() {
    let w = world(ANTHROPIC);
    for (status, body) in [
        (400, "max_tokens is too large"),
        (500, "web_search is down"),
    ] {
        let built = build(
            &w,
            PermissionMode::Auto,
            "anthropic",
            rejected(status, body),
        );
        let TurnOutcome::Failed(text) = turn(&built).await else {
            panic!("expected a failure");
        };
        assert!(!text.contains("native"), "{status}: {text}");
    }
}

fn tavily_world() -> World {
    let w = world(&format!("{ANTHROPIC}[websearch]\nbackend = \"tavily\"\n"));
    common::write(
        &w.data.join("credentials.toml"),
        "[websearch]\napi_key = \"k\"\n",
    );
    w
}

#[tokio::test]
async fn a_failed_rebuild_that_must_withhold_it_is_an_error() {
    let w = tavily_world();
    let mut built = build(&w, PermissionMode::Auto, "anthropic", vec![answer()]);
    assert!(built.runtime.native_search_active());
    std::fs::remove_file(w.data.join("credentials.toml")).unwrap();

    built.runtime.state.set_mode(PermissionMode::Manual);
    assert!(built.runtime.refresh_native_search().is_err());
    assert!(built.runtime.native_search_active());
}

#[tokio::test]
async fn a_failed_rebuild_that_would_only_offer_it_is_not() {
    let w = tavily_world();
    let mut built = build(&w, PermissionMode::Manual, "anthropic", vec![answer()]);
    assert!(!built.runtime.native_search_active());
    std::fs::remove_file(w.data.join("credentials.toml")).unwrap();

    built.runtime.state.set_mode(PermissionMode::Auto);
    built.runtime.refresh_native_search().unwrap();
    assert!(!built.runtime.native_search_active());
}
