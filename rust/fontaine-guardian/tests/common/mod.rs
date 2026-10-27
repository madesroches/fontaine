#![allow(dead_code)]

//! A scripted chat-completions endpoint and the request fixtures the guardian tests share.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fontaine_guardian::{
    AdjudicationRequest, EnvironmentConfig, Guardian, RuleConfig, SessionFacts, Settings,
    ToolCallSummary,
};
use rig_core::DynModel;
use rig_core::operation::Completion;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Auth, Dialect};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const GATEWAY: Dialect = Dialect::gateway("openai-compatible", "", "");

#[derive(Clone, Debug)]
pub enum Reply {
    /// The model calls `submit` with this verdict.
    Verdict(Value),
    /// The model calls `submit` with arguments that are not a verdict.
    Malformed,
    /// The model answers in prose and never calls `submit`.
    Prose,
    /// The model answers in this prose.
    Text(String),
    Fail(u16),
    /// Answers after this long.
    Slow(Duration, Value),
}

pub fn verdict(decision: &str, rule: &str, confidence: f64) -> Value {
    json!({
        "decision": decision,
        "rule": rule,
        "explanation": format!("{decision} because of {rule}"),
        "confidence": confidence,
    })
}

pub fn allow(confidence: f64) -> Value {
    verdict("allow", "allow", confidence)
}

pub fn deny(rule: &str) -> Value {
    verdict("deny", rule, 0.97)
}

fn completion(message: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion",
        "created": 1,
        "model": "scripted",
        "choices": [{"index": 0, "message": message, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    }))
}

fn submit(arguments: &str) -> ResponseTemplate {
    completion(json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "submit", "arguments": arguments},
        }],
    }))
}

struct Scripted {
    replies: Vec<Reply>,
    next: Arc<AtomicUsize>,
}

impl Respond for Scripted {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let at = self.next.fetch_add(1, Ordering::SeqCst);
        let Some(reply) = self.replies.get(at).or(self.replies.last()) else {
            return ResponseTemplate::new(500);
        };
        match reply {
            Reply::Verdict(value) => submit(&value.to_string()),
            Reply::Malformed => submit(r#"{"decision":"maybe"}"#),
            Reply::Prose => {
                completion(json!({"role": "assistant", "content": "Looks fine to me."}))
            }
            Reply::Text(text) => completion(json!({"role": "assistant", "content": text})),
            Reply::Fail(status) => ResponseTemplate::new(*status).set_body_string("scripted"),
            Reply::Slow(delay, value) => submit(&value.to_string()).set_delay(*delay),
        }
    }
}

pub struct Endpoint {
    pub server: MockServer,
    arrivals: Arc<AtomicUsize>,
}

impl Endpoint {
    pub async fn start(replies: Vec<Reply>) -> Endpoint {
        let server = MockServer::builder().start().await;
        let arrivals = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(Scripted {
                replies,
                next: arrivals.clone(),
            })
            .mount(&server)
            .await;
        Endpoint { server, arrivals }
    }

    /// The number of requests that have reached the server, readable from a responder on another
    /// server so that it can hold its answer until this endpoint has been asked.
    pub fn arrivals(&self) -> Arc<AtomicUsize> {
        self.arrivals.clone()
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

    pub async fn calls(&self) -> usize {
        self.requests().await.len()
    }

    pub fn guardian(&self, settings: Settings) -> Guardian {
        guardian_at(&self.url(), "mock/scripted", settings)
    }
}

pub fn model_at(url: &str) -> DynModel<Completion> {
    let client = OpenAIConfig::with_key(&GATEWAY, "key")
        .with_auth(Auth::OptionalBearer)
        .with_base_url(url)
        .client();
    client.completion("scripted").erase()
}

pub fn guardian_at(url: &str, label: &str, settings: Settings) -> Guardian {
    Guardian::new(model_at(url), label, settings)
}

pub fn request(command: &str, destructive: bool) -> AdjudicationRequest {
    let args = json!({"command": command});
    AdjudicationRequest {
        task_intent: "Run the project's tests".into(),
        recent_user_messages: vec!["Run the project's tests".into()],
        project_instructions: Some("Use cargo test".into()),
        environment: EnvironmentConfig::expand(&[]),
        rules: RuleConfig::expand(&[], &[], &[]),
        tool_call: ToolCallSummary {
            tool: "shell".into(),
            args_digest: format!("digest-of-{command}"),
            args,
            destructive,
            annotations: None,
        },
        session_facts: SessionFacts {
            workspace_roots: vec!["/work/proj".into()],
            workspace_trust: "trusted".into(),
            ..SessionFacts::default()
        },
        hook_context: vec![],
        prior_denials: vec![],
    }
}

pub fn settings() -> Settings {
    Settings::default()
}

pub fn shared(guardian: Guardian) -> Arc<Guardian> {
    Arc::new(guardian)
}
