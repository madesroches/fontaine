//! The output cap a main or subagent turn sends: the model's limit, a fixed fallback, or the
//! user's `[model] max_output_tokens`.

mod common;

use std::ffi::OsString;
use std::sync::Arc;

use common::World;
use common::runtime::{build_with_approver, build_with_models};
use fontaine_core::config::{AccountKind, PermissionMode::Manual};
use fontaine_core::model_roles::ModelChain;
use fontaine_core::models::{
    FALLBACK_MAX_OUTPUT_TOKENS, GuardianChoice, Listing, ModelHandle, ModelRef, Models, Switcher,
};
use fontaine_core::permissions::DenyAll;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::catalog::Catalog;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, mock_final};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LISTED: &str = "openai/gpt-4o-mini";

fn limit_of(id: &str) -> u32 {
    Catalog::builtin()
        .resolve(id)
        .unwrap()
        .spec
        .max_output_tokens
        .expect("the catalog lists an output limit")
}

/// A handle of `kind` on a mock whose catalog entry is `id`; `None` has no entry at all.
fn handle_of(kind: &str, id: Option<&str>) -> (ModelHandle, MockCompletionModel) {
    let mock = MockCompletionModel::from_stream_turns(vec![text_turn("done"), text_turn("done")]);
    let mock = match id {
        Some(id) => mock.priced_as((*Catalog::builtin().resolve(id).unwrap().spec).clone()),
        None => mock,
    };
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named(kind),
        mock.clone().into(),
    );
    (handle, mock)
}

fn text_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(text.to_string()),
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
    ]
}

#[test]
fn the_cap_follows_the_table() {
    let (listed, _) = handle_of("openai-compatible", Some(LISTED));
    let (unlisted, _) = handle_of("openai-compatible", None);
    let limit = limit_of(LISTED);

    assert_eq!(listed.turn_max_tokens(None), Some(u64::from(limit)));
    assert_eq!(
        listed.turn_max_tokens(Some(limit - 1)),
        Some(u64::from(limit - 1))
    );
    assert_eq!(
        listed.turn_max_tokens(Some(limit + 1)),
        Some(u64::from(limit))
    );
    assert_eq!(unlisted.turn_max_tokens(Some(4096)), Some(4096));
    assert_eq!(unlisted.turn_max_tokens(None), Some(32_000));
    assert_eq!(FALLBACK_MAX_OUTPUT_TOKENS, 32_000);
}

#[test]
fn ollama_and_the_messages_kinds_send_nothing_without_a_limit_or_a_cap() {
    for kind in ["ollama", "anthropic", "claude-subscription"] {
        let (unlisted, _) = handle_of(kind, None);
        assert_eq!(unlisted.turn_max_tokens(None), None, "{kind}");
        assert_eq!(unlisted.turn_max_tokens(Some(4096)), Some(4096), "{kind}");
        let (listed, _) = handle_of(kind, Some(LISTED));
        assert_eq!(
            listed.turn_max_tokens(None),
            Some(u64::from(limit_of(LISTED))),
            "{kind}"
        );
    }
}

fn runtime_on(world: &World, handle: ModelHandle) -> fontaine_core::agent::AgentRuntime {
    let models = Models {
        main_chain: ModelChain::single(handle.reference.clone()),
        main: handle,
        main_listing: Listing::default(),
        guardian: None,
        guardian_choice: GuardianChoice::Pinned,
        notices: Vec::new(),
    };
    build_with_approver(
        world,
        Manual,
        models,
        None,
        false,
        Default::default(),
        None,
        Arc::new(DenyAll),
    )
}

async fn root_turn(runtime: &fontaine_core::agent::AgentRuntime) {
    runtime.begin_turn("go", false);
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &runtime.agent,
            state: &runtime.state,
            prompt: "go".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(5),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: runtime.turn_generation_options(),
            max_tokens: runtime.turn_max_tokens(),
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
}

fn sent(mock: &MockCompletionModel) -> Vec<Option<u64>> {
    mock.requests().iter().map(|r| r.max_tokens).collect()
}

#[tokio::test]
async fn a_main_turn_on_a_chat_kind_sends_the_fallback_without_a_spec() {
    let world = World::new();
    let (handle, mock) = handle_of("openai-compatible", None);
    root_turn(&runtime_on(&world, handle)).await;
    assert_eq!(sent(&mock), [Some(32_000)]);
}

#[tokio::test]
async fn a_main_turn_on_a_chat_kind_sends_the_models_limit() {
    let world = World::new();
    let (handle, mock) = handle_of("openai-compatible", Some(LISTED));
    root_turn(&runtime_on(&world, handle)).await;
    assert_eq!(sent(&mock), [Some(u64::from(limit_of(LISTED)))]);
}

#[tokio::test]
async fn the_user_cap_lowers_a_turn_and_never_raises_it_past_the_limit() {
    let world = World::new();
    world.user_file("[model]\nmax_output_tokens = 4096\n");
    let (handle, mock) = handle_of("openai-compatible", Some(LISTED));
    root_turn(&runtime_on(&world, handle)).await;
    assert_eq!(sent(&mock), [Some(4096)]);

    let world = World::new();
    world.user_file("[model]\nmax_output_tokens = 4000000\n");
    let (handle, mock) = handle_of("openai-compatible", Some(LISTED));
    root_turn(&runtime_on(&world, handle)).await;
    assert_eq!(sent(&mock), [Some(u64::from(limit_of(LISTED)))]);

    let world = World::new();
    world.user_file("[model]\nmax_output_tokens = 4096\n");
    let (handle, mock) = handle_of("openai-compatible", None);
    root_turn(&runtime_on(&world, handle)).await;
    assert_eq!(sent(&mock), [Some(4096)]);
}

#[tokio::test]
async fn a_main_turn_on_a_messages_kind_without_a_spec_leaves_the_cap_to_the_wire() {
    let world = World::new();
    let (handle, mock) = handle_of("anthropic", None);
    root_turn(&runtime_on(&world, handle)).await;
    assert_eq!(sent(&mock), [None]);
}

#[test]
fn a_zero_cap_is_refused_naming_the_key() {
    let world = World::new();
    world.user_file("[model]\nmax_output_tokens = 0\n");
    let error = world.load().unwrap_err().to_string();
    assert!(error.contains("model.max_output_tokens"), "{error}");
    let world = World::new();
    world.user_file("[model]\nmax_output_tokens = 4096\n");
    assert_eq!(world.load().unwrap().max_output_tokens, Some(4096));
}

#[tokio::test]
async fn switching_the_model_changes_the_next_turns_cap() {
    let world = World::new();
    let (first, first_mock) = handle_of("openai-compatible", None);
    let mut runtime = runtime_on(&world, first);
    assert_eq!(runtime.turn_max_tokens(), Some(32_000));
    root_turn(&runtime).await;

    let (second, second_mock) = handle_of("openai-compatible", Some(LISTED));
    runtime.models.main = second;
    assert_eq!(runtime.turn_max_tokens(), Some(u64::from(limit_of(LISTED))));
    root_turn(&runtime).await;
    // The agent was built on the first model, so both requests reach it; the second carries the
    // limit of the model the session switched to.
    assert_eq!(
        sent(&first_mock),
        [Some(32_000), Some(u64::from(limit_of(LISTED)))]
    );
    assert!(second_mock.requests().is_empty());
}

fn env(name: &str) -> Option<OsString> {
    (name == "PAID_KEY").then(|| OsString::from("sk-test"))
}

fn sse(chunks: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

async fn chat_bodies(server: &MockServer) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/chat/completions")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

/// A gateway that lists the model `m1` and answers every chat request with a short reply.
async fn child_gateway() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "m1", "object": "model", "created": 0, "owned_by": "x"}],
        })))
        .mount(&server)
        .await;
    let chunk = |delta: serde_json::Value, finish: serde_json::Value| {
        json!({
            "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m1",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    sse(&[
                        chunk(
                            json!({"role": "assistant", "content": "child done"}),
                            json!(null),
                        ),
                        chunk(json!({}), json!("stop")),
                    ]),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_subagent_sends_its_own_models_limit_or_the_cap() {
    for (cap, expected) in [
        ("", 32_000),
        ("[model]\nmax_output_tokens = 20000\n", 20_000),
    ] {
        let server = child_gateway().await;
        let world = World::new();
        world.user_file(&format!(
            "{cap}[accounts.paid]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\
             models = [\"m1\"]\n\n\
             [permissions]\nallow = [\"agent\"]\n\
             [agents.types.scout]\ntools = [\"read\"]\nmodel = \"paid/m1\"\n",
            server.uri()
        ));
        let config = world.load().unwrap();
        let switcher = Switcher::new(config, world.data.clone(), Arc::new(env), None);
        // The main model is a mock with a known, smaller limit than the child's unknown one.
        let main = MockCompletionModel::from_stream_turns(vec![
            vec![
                MockStreamEvent::ToolCall {
                    id: "call-agent".into(),
                    name: "agent".into(),
                    arguments: json!({"agent_type": "scout", "prompt": "look around"}),
                    call_id: None,
                },
                MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
            ],
            text_turn("root done"),
        ])
        .priced_as((*Catalog::builtin().resolve(LISTED).unwrap().spec).clone());
        let handle = ModelHandle::new(
            ModelRef::parse("test/main").unwrap(),
            AccountKind::named("openai-compatible"),
            main.clone().into(),
        );
        let models = Models {
            main_chain: ModelChain::single(handle.reference.clone()),
            main: handle,
            main_listing: Listing::default(),
            guardian: None,
            guardian_choice: GuardianChoice::Pinned,
            notices: Vec::new(),
        };
        let runtime = build_with_models(
            &world,
            Manual,
            models,
            Some(switcher),
            false,
            Default::default(),
            None,
        );
        root_turn(&runtime).await;
        // The child runs in the background: wait for its request.
        let mut bodies = Vec::new();
        for _ in 0..100 {
            bodies = chat_bodies(&server).await;
            if !bodies.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !bodies.is_empty(),
            "the child made no request: {:?}",
            runtime.state.agents().report_of("agent-1")
        );
        for body in &bodies {
            assert_eq!(body["max_tokens"], expected);
        }
        // The parent's requests carry the parent model's limit, not the child's.
        let parent = Some(u64::from(limit_of(LISTED)).min(expected));
        assert_eq!(sent(&main), [parent, parent]);
    }
}
