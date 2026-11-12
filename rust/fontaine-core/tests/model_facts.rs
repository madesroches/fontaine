//! `[accounts.*.model_facts]` on an `anthropic` account against a `wiremock` Messages endpoint:
//! a host's own name for a Claude model takes that model's facts, and a name with none is
//! warned about once.

mod common;

use std::ffi::OsString;

use common::World;
use fontaine_core::models::{self, ModelHandle, ResolveParams};
use rig_core::catalog::Catalog;
use rig_core::completion::CompletionRequest;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env(name: &str) -> Option<OsString> {
    (name == "HOST_KEY").then(|| OsString::from("host-test-key"))
}

fn world(server: &MockServer, extra: &str) -> World {
    let w = World::new();
    w.user_file(&format!(
        "[accounts.foundry]\nkind = \"anthropic\"\nbase_url = \"{}\"\napi_key_env = \"HOST_KEY\"\nmodels = [\"prod-opus\", \"team-haiku\", \"claude-opus-5-5\"]\n{extra}",
        server.uri()
    ));
    w
}

async fn resolve(w: &World, model: &str) -> ModelHandle {
    let config = w.load().unwrap();
    models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: std::sync::Arc::new(env),
        model_override: Some(&format!("foundry/{model}")),
        session_id: None,
    })
    .await
    .unwrap()
    .main
}

fn opus_limit() -> u32 {
    Catalog::builtin()
        .resolve("anthropic/claude-opus-5-5")
        .unwrap()
        .spec
        .max_output_tokens
        .expect("the catalog lists an output limit")
}

const MAPPED: &str = "\n[accounts.foundry.model_facts]\nprod-opus = \"claude-opus-5-5\"\n";

#[tokio::test]
async fn a_mapped_name_sends_the_targets_output_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "prod-opus",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 4},
        })))
        .mount(&server)
        .await;
    let handle = resolve(&world(&server, MAPPED), "prod-opus").await;
    let limit = u64::from(opus_limit());
    assert_eq!(handle.turn_max_tokens(None), Some(limit));
    assert_eq!(handle.facts_notice, None);
    handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(handle.turn_max_tokens(None).unwrap()))
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let request = requests
        .iter()
        .find(|r| r.url.path() == "/v1/messages")
        .expect("a request reached the endpoint");
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "prod-opus", "{body}");
    assert_eq!(body["max_tokens"], limit, "{body}");
}

#[tokio::test]
async fn a_mapped_name_has_the_targets_window_and_no_price() {
    let server = MockServer::start().await;
    let handle = resolve(&world(&server, MAPPED), "prod-opus").await;
    let spec = handle.completion.spec().expect("facts are bound");
    assert_eq!(spec.max_output_tokens, Some(opus_limit()));
    assert!(spec.context_window.is_some());
    assert_eq!(spec.pricing, None);
}

#[tokio::test]
async fn an_unmapped_unknown_name_has_no_facts_and_a_warning_naming_the_key() {
    let server = MockServer::start().await;
    let handle = resolve(&world(&server, ""), "prod-opus").await;
    assert!(handle.completion.spec().is_none());
    assert_eq!(handle.turn_max_tokens(None), None);
    let notice = handle.facts_notice.expect("a model with no facts warns");
    assert!(notice.contains("prod-opus"), "{notice}");
    assert!(
        notice.contains("[accounts.foundry.model_facts]"),
        "{notice}"
    );
}

#[tokio::test]
async fn a_listed_model_keeps_its_own_facts_and_no_notice() {
    let server = MockServer::start().await;
    let handle = resolve(&world(&server, MAPPED), "claude-opus-5-5").await;
    assert_eq!(handle.facts_notice, None);
    assert!(handle.completion.spec().unwrap().pricing.is_some());
}
