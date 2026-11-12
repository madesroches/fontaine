//! A `databricks` account against a `wiremock` stand-in for the workspace: each wire's path and
//! credential placement, the `max_tokens` regression, prompt caching, tool-input streaming,
//! the Unity Catalog model listing and the guardian's cache scope.

mod common;

use std::ffi::OsString;

use common::World;
use fontaine_core::config::AccountKind;
use fontaine_core::models::{self, GuardianModel, ModelHandle, ResolveParams};
use rig_core::catalog::Catalog;
use rig_core::completion::CompletionRequest;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "dapi-test-token";

const MODELS: &str = r#"models = ["system.ai.claude-haiku-5-5", "system.ai.claude-haiku-9-9", "system.ai.gpt-5-6-luna", "system.ai.gemini-3-pro", "system.ai.glm-5-3", "system.ai.kimi-k3", "main.team.router"]"#;

fn env(name: &str) -> Option<OsString> {
    (name == "DBX_KEY").then(|| OsString::from(TOKEN))
}

fn world_with(server: &MockServer, extra: &str) -> World {
    let w = World::new();
    w.user_file(&format!(
        "[accounts.dbx]\nkind = \"databricks\"\nbase_url = \"{}/\"\napi_key_env = \"DBX_KEY\"\n{MODELS}\n{extra}",
        server.uri()
    ));
    w
}

fn world(server: &MockServer) -> World {
    world_with(server, "")
}

async fn resolve_ref(w: &World, reference: &str) -> Result<models::Models, models::ModelError> {
    let config = w.load().unwrap();
    models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: std::sync::Arc::new(env),
        model_override: Some(reference),
        session_id: None,
    })
    .await
}

async fn resolve(w: &World, model: &str) -> Result<ModelHandle, models::ModelError> {
    resolve_ref(w, &format!("dbx/{model}"))
        .await
        .map(|resolved| resolved.main)
}

async fn say_hi(handle: &ModelHandle) -> String {
    handle
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap()
        .text()
}

async fn only_request(server: &MockServer, route: &str) -> wiremock::Request {
    let all = server.received_requests().await.unwrap();
    let mut hits = all.into_iter().filter(|r| r.url.path() == route);
    let first = hits
        .next()
        .unwrap_or_else(|| panic!("no request reached {route}"));
    assert!(hits.next().is_none(), "{route} was hit twice");
    first
}

fn header<'a>(request: &'a wiremock::Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|v| v.to_str().ok())
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

fn messages_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-haiku-5-5",
        "content": [{"type": "text", "text": "hi from messages"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 4},
    }))
}

fn chat_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "c1", "object": "chat.completion", "created": 1, "model": "glm-5-3",
        "choices": [{"index": 0, "finish_reason": "stop",
                     "message": {"role": "assistant", "content": "hi from chat"}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
    }))
}

fn responses_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "resp_1", "object": "response", "status": "completed",
        "output": [{"type": "message", "id": "msg_1", "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "hi from responses",
                                 "annotations": []}]}],
        "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7},
    }))
}

fn gemini_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "candidates": [{"content": {"role": "model", "parts": [{"text": "hi from gemini"}]},
                        "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 2,
                          "totalTokenCount": 3},
    }))
}

async fn mount_post(server: &MockServer, route: &str, reply: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(reply)
        .mount(server)
        .await;
}

const MESSAGES: &str = "/ai-gateway/anthropic/v1/messages";
const RESPONSES: &str = "/ai-gateway/openai/v1/responses";
const CHAT: &str = "/ai-gateway/mlflow/v1/chat/completions";
const GEMINI: &str = "/ai-gateway/gemini/v1beta/models/system.ai.gemini-3-pro:generateContent";

#[tokio::test]
async fn claude_uses_the_messages_route_with_a_bearer_token_and_no_api_key_header() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let handle = resolve(&world(&server), "system.ai.claude-haiku-5-5")
        .await
        .unwrap();
    assert_eq!(handle.kind, AccountKind::named("databricks"));
    assert!(!handle.priced(), "the workspace bills in DBUs");
    assert!(!handle.catalog_billed());
    assert_eq!(say_hi(&handle).await, "hi from messages");

    let request = only_request(&server, MESSAGES).await;
    assert_eq!(header(&request, "authorization"), Some(bearer().as_str()));
    assert!(
        header(&request, "x-api-key").is_none(),
        "{:?}",
        request.headers
    );
}

#[tokio::test]
async fn gpt_uses_the_responses_route_with_a_bearer_token() {
    let server = MockServer::start().await;
    mount_post(&server, RESPONSES, responses_reply()).await;
    let handle = resolve(&world(&server), "system.ai.gpt-5-6-luna")
        .await
        .unwrap();
    assert_eq!(say_hi(&handle).await, "hi from responses");
    let request = only_request(&server, RESPONSES).await;
    assert_eq!(header(&request, "authorization"), Some(bearer().as_str()));
}

#[tokio::test]
async fn open_and_custom_models_use_the_chat_route_with_a_bearer_token() {
    let server = MockServer::start().await;
    mount_post(&server, CHAT, chat_reply()).await;
    let w = world(&server);
    for model in ["system.ai.glm-5-3", "main.team.router"] {
        let handle = resolve(&w, model).await.unwrap();
        assert_eq!(say_hi(&handle).await, "hi from chat", "{model}");
    }
    for request in server.received_requests().await.unwrap() {
        assert_eq!(request.url.path(), CHAT);
        assert_eq!(header(&request, "authorization"), Some(bearer().as_str()));
    }
}

#[tokio::test]
async fn gemini_carries_a_bearer_token_and_no_key_query_at_all() {
    let server = MockServer::start().await;
    mount_post(&server, GEMINI, gemini_reply()).await;
    let handle = resolve(&world(&server), "system.ai.gemini-3-pro")
        .await
        .unwrap();
    assert_eq!(say_hi(&handle).await, "hi from gemini");
    let request = only_request(&server, GEMINI).await;
    assert_eq!(header(&request, "authorization"), Some(bearer().as_str()));
    assert!(header(&request, "x-goog-api-key").is_none());
    assert!(
        request.url.query_pairs().all(|(k, _)| k != "key"),
        "{}",
        request.url
    );
}

fn claude_limit() -> u32 {
    Catalog::builtin()
        .resolve("anthropic/claude-haiku-5-5")
        .unwrap()
        .spec
        .max_output_tokens
        .expect("the catalog lists an output limit")
}

/// The live failure: an unlisted `system.ai.` id had no facts, so the turn sent no cap and the
/// Messages wire refused it.
#[tokio::test]
async fn a_claude_turn_without_a_user_cap_sends_the_catalogs_output_limit() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let handle = resolve(&world(&server), "system.ai.claude-haiku-5-5")
        .await
        .unwrap();
    let limit = u64::from(claude_limit());
    assert_eq!(handle.turn_max_tokens(None), Some(limit));
    handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(handle.turn_max_tokens(None).unwrap()))
        .await
        .unwrap();
    let request = only_request(&server, MESSAGES).await;
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["max_tokens"], limit, "{body}");
}

#[tokio::test]
async fn facts_are_bound_under_the_served_id_on_the_responses_and_messages_wires() {
    let server = MockServer::start().await;
    let w = world_with(
        &server,
        "\n[accounts.dbx.wires]\n\"system.ai.claude-haiku-5-5\" = \"responses\"\n",
    );
    let limit = claude_limit();
    let on_responses = resolve(&w, "system.ai.claude-haiku-5-5").await.unwrap();
    assert_eq!(
        on_responses.completion.spec().unwrap().max_output_tokens,
        Some(limit)
    );
    let w = world(&server);
    let on_messages = resolve(&w, "system.ai.claude-haiku-5-5").await.unwrap();
    let spec = on_messages.completion.spec().unwrap();
    assert_eq!(spec.max_output_tokens, Some(limit));
    assert_eq!(spec.pricing, None);
    assert_eq!(on_messages.facts_notice, None);
}

#[tokio::test]
async fn an_unlisted_claude_id_derives_facts_and_says_so() {
    let server = MockServer::start().await;
    let handle = resolve(&world(&server), "system.ai.claude-haiku-9-9")
        .await
        .unwrap();
    assert!(
        handle
            .completion
            .spec()
            .unwrap()
            .max_output_tokens
            .is_some()
    );
    let notice = handle.facts_notice.expect("derived facts are announced");
    assert!(notice.contains("claude-haiku-9-9"), "{notice}");
}

#[tokio::test]
async fn glm_without_a_user_cap_sends_25k_and_a_lower_cap_wins() {
    let server = MockServer::start().await;
    let w = world(&server);
    let glm = resolve(&w, "system.ai.glm-5-3").await.unwrap();
    assert_eq!(glm.turn_max_tokens(None), Some(25_000));
    assert_eq!(glm.turn_max_tokens(Some(4096)), Some(4096));
    assert_eq!(glm.turn_max_tokens(Some(100_000)), Some(25_000));
    let other = resolve(&w, "system.ai.kimi-k3").await.unwrap();
    assert_eq!(other.turn_max_tokens(None), Some(32_000));
}

fn caching_request() -> CompletionRequest {
    CompletionRequest::new("hi")
        .preamble("You are a test.")
        .max_tokens(64)
        .tools([rig_core::completion::ToolDefinition::new(
            rig_core::message::ToolName::new("first").unwrap(),
            "one",
            json!({"type": "object", "properties": {}}),
        )])
}

async fn body_of(server: &MockServer, handle: &ModelHandle, route: &str) -> serde_json::Value {
    handle.completion.call(caching_request()).await.unwrap();
    serde_json::from_slice(&only_request(server, route).await.body).unwrap()
}

#[tokio::test]
async fn claude_requests_carry_cache_breakpoints_by_default() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let handle = resolve(&world(&server), "system.ai.claude-haiku-5-5")
        .await
        .unwrap();
    let body = body_of(&server, &handle, MESSAGES).await;
    assert!(body["tools"][0]["cache_control"].is_object(), "{body}");
    assert!(
        body["system"].as_array().unwrap().last().unwrap()["cache_control"].is_object(),
        "{body}"
    );
    let message = body["messages"].as_array().unwrap().last().unwrap();
    assert!(message.to_string().contains("cache_control"), "{body}");
    assert!(!body.to_string().contains("\"ttl\""), "{body}");
}

#[tokio::test]
async fn a_one_hour_ttl_is_requested_with_cache_ttl() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let w = world_with(&server, "cache_ttl = \"1h\"\n");
    let handle = resolve(&w, "system.ai.claude-haiku-5-5").await.unwrap();
    let body = body_of(&server, &handle, MESSAGES).await;
    assert_eq!(body["tools"][0]["cache_control"]["ttl"], "1h", "{body}");
}

#[tokio::test]
async fn prompt_caching_false_sends_no_breakpoints() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let w = world_with(&server, "prompt_caching = false\n");
    let handle = resolve(&w, "system.ai.claude-haiku-5-5").await.unwrap();
    let body = body_of(&server, &handle, MESSAGES).await;
    assert!(!body.to_string().contains("cache_control"), "{body}");
}

#[tokio::test]
async fn gpt_gemini_and_chat_requests_carry_no_cache_control() {
    let server = MockServer::start().await;
    mount_post(&server, RESPONSES, responses_reply()).await;
    mount_post(&server, GEMINI, gemini_reply()).await;
    mount_post(&server, CHAT, chat_reply()).await;
    let w = world(&server);
    for (model, route) in [
        ("system.ai.gpt-5-6-luna", RESPONSES),
        ("system.ai.gemini-3-pro", GEMINI),
        ("system.ai.glm-5-3", CHAT),
    ] {
        let handle = resolve(&w, model).await.unwrap();
        let body = body_of(&server, &handle, route).await;
        assert!(
            !body.to_string().contains("cache_control"),
            "{model}: {body}"
        );
    }
}

#[tokio::test]
async fn claude_tools_stream_eagerly_and_a_forced_non_claude_model_uses_the_beta_header() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let w = world(&server);
    let claude = resolve(&w, "system.ai.claude-haiku-5-5").await.unwrap();
    let body = body_of(&server, &claude, MESSAGES).await;
    assert_eq!(body["tools"][0]["eager_input_streaming"], true, "{body}");
    let request = only_request(&server, MESSAGES).await;
    assert!(
        !header(&request, "anthropic-beta")
            .unwrap_or_default()
            .contains("fine-grained-tool-streaming"),
        "{:?}",
        request.headers
    );

    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    let w = world_with(
        &server,
        "\n[accounts.dbx.wires]\n\"system.ai.glm-5-3\" = \"messages\"\n",
    );
    let glm = resolve(&w, "system.ai.glm-5-3").await.unwrap();
    let body = body_of(&server, &glm, MESSAGES).await;
    assert!(
        body["tools"][0].get("eager_input_streaming").is_none(),
        "{body}"
    );
    let request = only_request(&server, MESSAGES).await;
    assert!(
        header(&request, "anthropic-beta")
            .unwrap_or_default()
            .contains("fine-grained-tool-streaming"),
        "{:?}",
        request.headers
    );
}

#[tokio::test]
async fn the_guardian_marks_only_its_static_prefix() {
    let server = MockServer::start().await;
    mount_post(&server, MESSAGES, messages_reply()).await;
    mount_post(&server, CHAT, chat_reply()).await;
    let w = world_with(
        &server,
        "\n[guardian]\nmodel = \"dbx/system.ai.claude-haiku-5-5\"\n",
    );
    let resolved = resolve_ref(&w, "dbx/system.ai.glm-5-3").await.unwrap();
    let Some(GuardianModel::Ready(handle)) = &resolved.guardian else {
        panic!("the guardian is ready");
    };
    let body = body_of(&server, handle, MESSAGES).await;
    assert!(body["tools"][0]["cache_control"].is_object(), "{body}");
    assert!(
        body["system"].as_array().unwrap().last().unwrap()["cache_control"].is_object(),
        "{body}"
    );
    let message = body["messages"].as_array().unwrap().last().unwrap();
    assert!(!message.to_string().contains("cache_control"), "{body}");
}

#[tokio::test]
async fn listing_pages_through_the_model_services_with_a_bearer_token() {
    let server = MockServer::start().await;
    let route = "/api/2.1/unity-catalog/model-services";
    Mock::given(method("GET"))
        .and(path(route))
        .and(query_param("parent", "schemas/system.ai"))
        .and(query_param("page_size", "100"))
        .and(query_param_is_missing("page_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model_services": [
                {"name": "model-services/system.ai.glm-5-3",
                 "supported_api_types": ["mlflow/v1/chat/completions"]},
                {"name": "model-services/system.ai.text-embed",
                 "supported_api_types": ["openai/v1/embeddings"]},
            ],
            "next_page_token": "p2",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(route))
        .and(query_param("parent", "schemas/system.ai"))
        .and(query_param("page_size", "100"))
        .and(query_param("page_token", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model_services": [
                {"name": "model-services/system.ai.claude-haiku-5-5",
                 "supported_api_types": ["anthropic/v1/messages"]},
                {"name": "model-services/other.schema.thing"},
            ],
        })))
        .expect(1)
        .mount(&server)
        .await;
    // No `models` list: the listing is the source.
    let w = World::new();
    w.user_file(&format!(
        "[accounts.dbx]\nkind = \"databricks\"\nbase_url = \"{}\"\napi_key_env = \"DBX_KEY\"\n",
        server.uri()
    ));
    let err = resolve(&w, "system.ai.nope").await.unwrap_err();
    let text = err.to_string();
    assert!(text.contains("does not offer model"), "{text}");
    assert!(text.contains("system.ai.glm-5-3"), "{text}");
    assert!(text.contains("system.ai.claude-haiku-5-5"), "{text}");
    assert!(!text.contains("text-embed"), "{text}");
    assert!(!text.contains("other.schema"), "{text}");
    for request in server.received_requests().await.unwrap() {
        assert_eq!(header(&request, "authorization"), Some(bearer().as_str()));
    }
}

#[tokio::test]
async fn a_mapped_custom_endpoint_takes_the_targets_facts_and_no_warning() {
    let server = MockServer::start().await;
    let w = world_with(
        &server,
        "\n[accounts.dbx.model_facts]\n\"main.team.router\" = \"claude-sonnet-5-5\"\n",
    );
    let handle = resolve(&w, "main.team.router").await.unwrap();
    let limit = Catalog::builtin()
        .resolve("anthropic/claude-sonnet-5-5")
        .unwrap()
        .spec
        .max_output_tokens
        .expect("the catalog lists an output limit");
    assert_eq!(
        handle.completion.spec().unwrap().max_output_tokens,
        Some(limit)
    );
    assert_eq!(handle.facts_notice, None);
}

#[tokio::test]
async fn an_unmapped_custom_endpoint_warns_and_an_open_model_does_not() {
    let server = MockServer::start().await;
    let w = world(&server);
    let notice = resolve(&w, "main.team.router")
        .await
        .unwrap()
        .facts_notice
        .expect("a custom endpoint with no facts warns");
    assert!(notice.contains("[accounts.dbx.model_facts]"), "{notice}");
    assert_eq!(
        resolve(&w, "system.ai.kimi-k3").await.unwrap().facts_notice,
        None
    );
}
