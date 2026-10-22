#![allow(dead_code)]

//! A scripted guardian endpoint: a chat-completions server that answers each adjudication with
//! the verdict whose key occurs in the user message, and keeps what it was sent.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

pub fn verdict(decision: &str, rule: &str, explanation: &str) -> Value {
    json!({
        "decision": decision,
        "rule": rule,
        "explanation": explanation,
        "confidence": 0.95,
    })
}

struct Script {
    /// The first rule whose text occurs in the user message wins.
    rules: Vec<(String, Value)>,
    default: Value,
    calls: AtomicUsize,
}

fn user_text(request: &Request) -> String {
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

impl Respond for Script {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = user_text(request);
        let verdict = self
            .rules
            .iter()
            .find(|(needle, _)| text.contains(needle.as_str()))
            .map_or(&self.default, |(_, v)| v);
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{"id": "g1", "type": "function",
                                "function": {"name": "submit", "arguments": verdict.to_string()}}]}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
        }))
    }
}

pub struct Endpoint {
    pub server: MockServer,
}

impl Endpoint {
    pub async fn start(rules: Vec<(&str, Value)>, default: Value) -> Endpoint {
        let server = MockServer::builder().start().await;
        Mock::given(any())
            .respond_with(Script {
                rules: rules.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                default,
                calls: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        Endpoint { server }
    }

    pub fn url(&self) -> String {
        self.server.uri()
    }

    pub async fn requests(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
            .collect()
    }
}
