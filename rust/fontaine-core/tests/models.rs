//! Config accounts to rig provider clients, one smoke test per provider wire format against a
//! `wiremock` endpoint, and the handle-level behavior around them.

mod common;

use std::collections::HashMap;
use std::ffi::OsString;

use common::World;
use fontaine_core::config::AccountKind;
use fontaine_core::models::{self, GuardianModel, ModelHandle, ModelRef, ResolveParams};
use rig_core::completion::CompletionRequest;
use rig_core::test_utils::MockCompletionModel;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).map(OsString::from)
}

async fn resolve_main(world: &World, env: &dyn Fn(&str) -> Option<OsString>) -> ModelHandle {
    let config = world.load().unwrap();
    models::resolve(&ResolveParams {
        config: &config,
        data_dir: &world.data,
        env,
        model_override: None,
    })
    .await
    .unwrap()
    .main
}

async fn say_hi(handle: &ModelHandle) -> String {
    handle
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap()
        .text()
}

#[tokio::test]
async fn anthropic_uses_the_messages_wire_and_the_env_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "key-from-env"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-haiku-4-5",
            "content": [{"type": "text", "text": "hi from anthropic"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 4},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.work]\nkind = \"anthropic\"\nbase_url = \"{}\"\napi_key_env = \"WORK_KEY\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"work/claude-haiku-4-5\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[("WORK_KEY", "key-from-env")])).await;
    assert_eq!(handle.kind, AccountKind::Anthropic);
    assert_eq!(handle.reference.to_string(), "work/claude-haiku-4-5");
    assert_eq!(say_hi(&handle).await, "hi from anthropic");
}

#[tokio::test]
async fn openai_uses_the_responses_wire_and_a_bearer_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer stored-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_1", "object": "response", "status": "completed",
            "output": [{"type": "message", "id": "msg_1", "role": "assistant",
                        "status": "completed",
                        "content": [{"type": "output_text", "text": "hi from openai",
                                     "annotations": []}]}],
            "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai\"\nbase_url = \"{}\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"main/gpt-4o-mini\"\n",
        server.uri()
    ));
    // No api_key_env: the key comes from the stored-credentials file.
    common::write(
        &w.data.join("credentials.toml"),
        "[accounts.main]\napi_key = \"stored-key\"\n",
    );
    let handle = resolve_main(&w, &env_of(&[])).await;
    assert_eq!(handle.kind, AccountKind::OpenAi);
    assert!(handle.priced(), "gpt-4o-mini has a catalog price");
    assert_eq!(say_hi(&handle).await, "hi from openai");
}

#[tokio::test]
async fn gemini_uses_generate_content_with_the_key_in_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:generateContent"))
        .and(wiremock::matchers::query_param("key", "gem-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "hi from gemini"}]},
                            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 2,
                              "totalTokenCount": 3},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.g]\nkind = \"gemini\"\nbase_url = \"{}\"\napi_key_env = \"GEM\"\nmodels = [\"gemini-2.5-flash\"]\n\n[model]\nmodel = \"g/gemini-2.5-flash\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[("GEM", "gem-key")])).await;
    assert_eq!(handle.kind, AccountKind::Gemini);
    assert_eq!(say_hi(&handle).await, "hi from gemini");
}

#[tokio::test]
async fn ollama_uses_the_native_chat_route_without_a_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3:4b", "created_at": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": "hi from ollama"},
            "done": true, "done_reason": "stop",
            "prompt_eval_count": 1, "eval_count": 2,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.local]\nkind = \"ollama\"\nbase_url = \"{}\"\nmodels = [\"qwen3:4b\"]\n\n[model]\nmodel = \"local/qwen3:4b\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[])).await;
    assert_eq!(handle.kind, AccountKind::Ollama);
    assert_eq!(say_hi(&handle).await, "hi from ollama");
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests[0].headers.contains_key("authorization"),
        "an Ollama account with no credential source is unauthenticated"
    );
}

#[tokio::test]
async fn openai_compatible_uses_chat_completions_and_is_unpriced() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "hi from a gateway"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"openai/gpt-4o-mini\"]\n\n[model]\nmodel = \"gw/openai/gpt-4o-mini\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[])).await;
    assert_eq!(handle.kind, AccountKind::OpenAiCompatible);
    // The model id keeps its own slash: the account is whatever precedes the first one.
    assert_eq!(handle.reference.account, "gw");
    assert_eq!(handle.reference.model, "openai/gpt-4o-mini");
    assert!(
        !handle.priced(),
        "the gateway dialect is priced by its kind name, which the catalog lacks"
    );
    assert_eq!(say_hi(&handle).await, "hi from a gateway");
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests[0].headers.contains_key("authorization"),
        "no credential source, no Authorization header"
    );
}

#[tokio::test]
async fn an_openai_compatible_key_is_sent_as_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer gw-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "ok"}}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\napi_key_env = \"GW\"\nmodels = [\"m\"]\n\n[model]\nmodel = \"gw/m\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[("GW", "gw-key")])).await;
    assert_eq!(say_hi(&handle).await, "ok");
}

#[tokio::test]
async fn the_guardian_model_may_sit_on_another_account() {
    let w = World::new();
    w.user_file(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"qwen3:4b\"]\n\n[accounts.safety]\nkind = \"anthropic\"\napi_key_env = \"SAFETY_KEY\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"main/qwen3:4b\"\n\n[guardian]\nmodel = \"safety/claude-haiku-4-5\"\n",
    );
    let config = w.load().unwrap();
    let env = env_of(&[("SAFETY_KEY", "k")]);
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &env,
        model_override: None,
    })
    .await
    .unwrap();
    assert_eq!(resolved.main.kind, AccountKind::Ollama);
    let Some(GuardianModel::Ready(guardian)) = resolved.guardian else {
        panic!("the guardian is ready: {:?}", resolved.guardian);
    };
    assert_eq!(guardian.kind, AccountKind::Anthropic);
    assert_eq!(guardian.reference.to_string(), "safety/claude-haiku-4-5");
}

#[tokio::test]
async fn no_guardian_model_means_no_guardian_handle() {
    let w = World::new();
    w.user_file(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"qwen3:4b\"]\n\n[model]\nmodel = \"main/qwen3:4b\"\n",
    );
    let config = w.load().unwrap();
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &env_of(&[]),
        model_override: None,
    })
    .await
    .unwrap();
    assert!(resolved.guardian.is_none());
}

#[test]
fn model_references_split_at_the_first_slash() {
    let r = ModelRef::parse("router/anthropic/claude-sonnet-4-6").unwrap();
    assert_eq!(r.account, "router");
    assert_eq!(r.model, "anthropic/claude-sonnet-4-6");
    assert_eq!(r.to_string(), "router/anthropic/claude-sonnet-4-6");
    assert!(ModelRef::parse("noslash").is_none());
    assert!(ModelRef::parse("/model").is_none());
    assert!(ModelRef::parse("account/").is_none());
}

#[tokio::test]
async fn a_handle_over_a_scripted_model_answers_without_any_endpoint() {
    let script = MockCompletionModel::default();
    let handle = ModelHandle::new(
        ModelRef::parse("test/scripted").unwrap(),
        AccountKind::OpenAiCompatible,
        script.into(),
    );
    assert!(!handle.priced());
    assert_eq!(handle.reference.model, "scripted");
}
