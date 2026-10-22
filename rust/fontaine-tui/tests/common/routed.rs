#![allow(dead_code)]

//! A Chat Completions endpoint for tests with several agents talking at once. Which
//! conversation a request belongs to is read from its first user message, so the order in which
//! concurrent agents ask does not matter.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone, Debug)]
pub enum Turn {
    Text(String),
    Tool {
        id: String,
        name: String,
        args: Value,
    },
}

impl Turn {
    pub fn text(text: &str) -> Turn {
        Turn::Text(text.to_string())
    }

    pub fn tool(id: &str, name: &str, args: Value) -> Turn {
        Turn::Tool {
            id: id.to_string(),
            name: name.to_string(),
            args,
        }
    }
}

pub struct Route {
    pub marker: String,
    pub turns: Vec<Turn>,
}

impl Route {
    pub fn new(marker: &str, turns: Vec<Turn>) -> Route {
        Route {
            marker: marker.to_string(),
            turns,
        }
    }
}

struct Routed {
    routes: Vec<(Route, AtomicUsize)>,
}

fn chunk(delta: Value, finish: Option<&str>) -> String {
    let chunk = json!({
        "id": "chatcmpl-routed", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    });
    format!("data: {chunk}\n\n")
}

fn first_user(body: &Value) -> String {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "user")
        .find_map(|m| match &m["content"] {
            Value::String(text) => Some(text.clone()),
            Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

impl Respond for Routed {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let first = first_user(&body);
        let Some((route, next)) = self.routes.iter().find(|(r, _)| first.contains(&r.marker))
        else {
            return ResponseTemplate::new(500).set_body_string(format!("no route for: {first}"));
        };
        let at = next.fetch_add(1, Ordering::SeqCst);
        let Some(turn) = route.turns.get(at).or(route.turns.last()) else {
            return ResponseTemplate::new(500).set_body_string("empty route");
        };
        let mut frames = Vec::new();
        match turn {
            Turn::Text(text) => {
                frames.push(chunk(json!({"role": "assistant", "content": text}), None));
                frames.push(chunk(json!({}), Some("stop")));
            }
            Turn::Tool { id, name, args } => {
                frames.push(chunk(
                    json!({"role": "assistant", "tool_calls": [{
                        "index": 0, "id": id, "type": "function",
                        "function": {"name": name, "arguments": args.to_string()},
                    }]}),
                    None,
                ));
                frames.push(chunk(json!({}), Some("tool_calls")));
            }
        }
        frames.push(format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl-routed", "object": "chat.completion.chunk", "created": 1,
                "model": "scripted", "choices": [],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500},
            })
        ));
        frames.push("data: [DONE]\n\n".to_string());
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(frames.concat().into_bytes(), "text/event-stream")
    }
}

pub async fn start(routes: Vec<Route>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Routed {
            routes: routes
                .into_iter()
                .map(|r| (r, AtomicUsize::new(0)))
                .collect(),
        })
        .mount(&server)
        .await;
    server
}
