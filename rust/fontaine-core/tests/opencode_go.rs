//! An `opencode-go` account against a `wiremock` stand-in for the gateway and the Console:
//! each wire's path and credential placement, listing, billing and the device sign-in.

mod common;

use std::ffi::OsString;

use common::World;
use fontaine_core::config::AccountKind;
use fontaine_core::credentials::{self, Credentials, OAuthTokens, StoredCredential};
use fontaine_core::models::{self, ModelHandle, ResolveParams};
use fontaine_core::providers::opencode_go::sign_in_at;
use fontaine_core::providers::token_source::now_unix;
use rig_core::completion::CompletionRequest;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION: &str = "11111111-2222-4333-8444-555555555555";

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn signed_in_world(server: &MockServer) -> World {
    let w = World::new();
    w.user_file(&format!(
        "[accounts.go]\nkind = \"opencode-go\"\nbase_url = \"{}/\"\n",
        server.uri()
    ));
    credentials::store_oauth(
        &w.data,
        "go",
        &OAuthTokens {
            access: "at-1".to_string(),
            refresh: "rt-1".to_string(),
            expires_at: now_unix() + 3600,
            extra: Default::default(),
        },
    )
    .unwrap();
    w
}

async fn resolve(w: &World, model: &str) -> Result<ModelHandle, models::ModelError> {
    let config = w.load().unwrap();
    let model = format!("go/{model}");
    models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &no_env,
        model_override: Some(&model),
        session_id: Some(SESSION.to_string()),
    })
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

async fn gateway() -> MockServer {
    let server = MockServer::start().await;
    let ids: Vec<_> = [
        "kimi-k3",
        "gpt-5.6-luna",
        "claude-haiku-5-5",
        "gemini-4-argon-contributor",
    ]
    .iter()
    .map(|id| json!({"id": id, "object": "model", "created": 1, "owned_by": "go"}))
    .collect();
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": ids,
        })))
        .expect(1)
        .mount(&server)
        .await;
    server
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

fn common_headers(request: &wiremock::Request) {
    assert!(
        header(request, "user-agent").is_some_and(|ua| ua.starts_with("fontaine/")),
        "{:?}",
        request.headers
    );
    assert_eq!(header(request, "x-opencode-session"), Some(SESSION));
}

#[tokio::test]
async fn chat_models_use_the_chat_route_with_a_bearer_token() {
    let server = gateway().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "kimi-k3",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "hi from chat"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3, "cost": 0.4},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = signed_in_world(&server);
    let handle = resolve(&w, "kimi-k3").await.unwrap();
    assert_eq!(handle.kind, AccountKind::named("opencode-go"));
    assert!(
        !handle.priced(),
        "Go is billed against subscription allowances"
    );
    assert!(!handle.catalog_billed());
    assert_eq!(say_hi(&handle).await, "hi from chat");

    let request = only_request(&server, "/openai/v1/chat/completions").await;
    assert_eq!(header(&request, "authorization"), Some("Bearer at-1"));
    common_headers(&request);
}

#[tokio::test]
async fn responses_models_use_the_responses_route_with_a_bearer_token() {
    let server = gateway().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_1", "object": "response", "status": "completed",
            "output": [{"type": "message", "id": "msg_1", "role": "assistant",
                        "status": "completed",
                        "content": [{"type": "output_text", "text": "hi from responses",
                                     "annotations": []}]}],
            "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = signed_in_world(&server);
    let handle = resolve(&w, "gpt-5.6-luna").await.unwrap();
    assert_eq!(say_hi(&handle).await, "hi from responses");

    let request = only_request(&server, "/openai/v1/responses").await;
    assert_eq!(header(&request, "authorization"), Some("Bearer at-1"));
    common_headers(&request);
}

#[tokio::test]
async fn messages_models_use_the_anthropic_route_with_an_api_key_header() {
    let server = gateway().await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-haiku-5-5",
            "content": [{"type": "text", "text": "hi from messages"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 4},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = signed_in_world(&server);
    let handle = resolve(&w, "claude-haiku-5-5").await.unwrap();
    assert_eq!(say_hi(&handle).await, "hi from messages");

    let request = only_request(&server, "/anthropic/v1/messages").await;
    assert_eq!(header(&request, "x-api-key"), Some("at-1"));
    assert!(header(&request, "authorization").is_none());
    common_headers(&request);
}

#[tokio::test]
async fn gemini_models_use_the_google_route_with_a_key_header_and_no_key_in_the_url() {
    let server = gateway().await;
    Mock::given(method("POST"))
        .and(path(
            "/google/v1beta/models/gemini-4-argon-contributor:generateContent",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "hi from gemini"}]},
                            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 2,
                              "totalTokenCount": 3},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = signed_in_world(&server);
    let handle = resolve(&w, "gemini-4-argon-contributor").await.unwrap();
    assert_eq!(say_hi(&handle).await, "hi from gemini");

    let request = only_request(
        &server,
        "/google/v1beta/models/gemini-4-argon-contributor:generateContent",
    )
    .await;
    assert_eq!(header(&request, "x-goog-api-key"), Some("at-1"));
    assert!(
        request.url.query_pairs().all(|(k, _)| k != "key"),
        "{}",
        request.url
    );
    assert!(header(&request, "authorization").is_none());
    common_headers(&request);
}

#[tokio::test]
async fn listing_goes_to_the_models_route_with_a_bearer_token_and_checks_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "kimi-k3", "object": "model", "created": 1, "owned_by": "go"}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = signed_in_world(&server);
    let err = resolve(&w, "not-a-go-model").await.unwrap_err();
    assert!(err.to_string().contains("does not offer model"), "{err}");
    assert!(err.to_string().contains("kimi-k3"), "{err}");
    let request = only_request(&server, "/v1/models").await;
    assert_eq!(header(&request, "authorization"), Some("Bearer at-1"));
}

#[tokio::test]
async fn an_account_that_never_signed_in_says_how_to() {
    let server = MockServer::start().await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.go]\nkind = \"opencode-go\"\nbase_url = \"{}\"\n",
        server.uri()
    ));
    let err = resolve(&w, "kimi-k3").await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("fontaine auth login go"), "{err}");
}

#[tokio::test]
async fn an_override_sends_a_model_over_the_wire_it_names() {
    let server = gateway().await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "kimi-k3",
            "content": [{"type": "text", "text": "hi over messages"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 4},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = signed_in_world(&server);
    w.user_file(&format!(
        "[accounts.go]\nkind = \"opencode-go\"\nbase_url = \"{}\"\n\n[accounts.go.wires]\n\"kimi-k3\" = \"messages\"\n",
        server.uri()
    ));
    let handle = resolve(&w, "kimi-k3").await.unwrap();
    assert_eq!(say_hi(&handle).await, "hi over messages");
}

#[tokio::test(start_paused = true)]
async fn sign_in_stores_the_tokens_and_names_the_account() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-1",
            "user_code": "ABCD-1234",
            "verification_uri_complete": "/console/device?user_code=ABCD-1234&client_id=opencode-cli",
            "expires_in": 600,
            "interval": 1,
        })))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-new",
            "refresh_token": "rt-new",
            "expires_in": 3600,
            "org_id": "org_9",
        })))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "usr_1", "email": "me@example.com",
        })))
        .expect(1)
        .mount(&console)
        .await;

    let w = World::new();
    let summary = sign_in_at(&console.uri(), "go", &w.data, None)
        .await
        .unwrap();
    assert_eq!(summary, "signed in to OpenCode Go as me@example.com");

    let Some(StoredCredential::OAuth(tokens)) = Credentials::load(&w.data).unwrap().account("go")
    else {
        panic!("expected an oauth entry");
    };
    assert_eq!(
        (tokens.access.as_str(), tokens.refresh.as_str()),
        ("at-new", "rt-new")
    );
    assert_eq!(tokens.extra["server"], console.uri());
    assert_eq!(tokens.extra["org_id"], "org_9");
    assert_eq!(tokens.extra["email"], "me@example.com");

    let user = only_request(&console, "/api/user").await;
    assert_eq!(header(&user, "authorization"), Some("Bearer at-new"));
}

#[tokio::test(start_paused = true)]
async fn a_refused_sign_in_stores_nothing() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-1",
            "user_code": "ABCD-1234",
            "verification_uri_complete": "https://opencode.ai/console/device?code=ABCD-1234",
            "expires_in": 600,
            "interval": 1,
        })))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "access_denied"})))
        .mount(&console)
        .await;
    let w = World::new();
    let err = sign_in_at(&console.uri(), "go", &w.data, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("access_denied"), "{err}");
    assert_eq!(Credentials::load(&w.data).unwrap().account("go"), None);
}

#[tokio::test]
async fn a_stored_api_key_is_not_a_sign_in() {
    let w = World::new();
    w.user_file("[accounts.go]\nkind = \"opencode-go\"\n");
    std::fs::write(
        w.data.join("credentials.toml"),
        "[accounts.go]\napi_key = \"sk-abc123\"\n",
    )
    .unwrap();
    let Err(err) = resolve(&w, "kimi-k3").await else {
        panic!("a key is no sign-in");
    };
    assert!(err.to_string().contains("not signed in"), "{err}");
}
