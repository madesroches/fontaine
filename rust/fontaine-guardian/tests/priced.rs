//! A guardian on a model billed outside per-token prices reports usage without a cost, even
//! when the provider's response carries one.

mod common;

use common::{allow, guardian_at, request};
use fontaine_guardian::Settings;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn adjudicate_with(priced: bool) -> Option<f64> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-scripted",
            "object": "chat.completion",
            "created": 1,
            "model": "scripted",
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "submit", "arguments": allow(0.99).to_string()},
                }],
            }}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
                      "cost": 0.5},
        })))
        .mount(&server)
        .await;
    let guardian = guardian_at(
        &server.uri(),
        "go/scripted",
        Settings {
            priced,
            cache: false,
            ..Settings::default()
        },
    );
    let result = guardian.adjudicate(&request("ls", false)).await;
    assert_eq!(result.usage.input_tokens, Some(100));
    result.usage.cost.map(|cost| cost.total)
}

#[tokio::test]
async fn a_priced_guardian_reports_the_cost_the_provider_sent() {
    assert_eq!(adjudicate_with(true).await, Some(0.5));
}

#[tokio::test]
async fn an_unpriced_guardian_reports_usage_without_a_cost() {
    assert_eq!(adjudicate_with(false).await, None);
}
