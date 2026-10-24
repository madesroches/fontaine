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
    /// Turns (by index) whose answer is held back this long.
    pub delays: Vec<(usize, std::time::Duration)>,
}

impl Route {
    pub fn new(marker: &str, turns: Vec<Turn>) -> Route {
        Route {
            marker: marker.to_string(),
            turns,
            delays: Vec::new(),
        }
    }

    /// Holds back the answer to turn `at` by `millis`.
    pub fn delay(mut self, at: usize, millis: u64) -> Route {
        self.delays
            .push((at, std::time::Duration::from_millis(millis)));
        self
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
    let items = if body["messages"].is_array() {
        &body["messages"]
    } else {
        &body["input"]
    };
    items
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

/// The answer to a request that did not ask for a stream, such as the summarizer's.
fn unary(turn: &Turn, delay: Option<&(usize, std::time::Duration)>) -> ResponseTemplate {
    let message = match turn {
        Turn::Text(text) => json!({"role": "assistant", "content": text}),
        Turn::Tool { id, name, args } => json!({
            "role": "assistant", "content": null,
            "tool_calls": [{"id": id, "type": "function",
                            "function": {"name": name, "arguments": args.to_string()}}],
        }),
    };
    let mut response = ResponseTemplate::new(200).set_body_json(json!({
        "id": "chatcmpl-routed", "object": "chat.completion", "created": 1, "model": "scripted",
        "choices": [{"index": 0, "finish_reason": "stop", "message": message}],
        "usage": {"prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500},
    }));
    if let Some((_, delay)) = delay {
        response = response.set_delay(*delay);
    }
    response
}

fn event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// The Responses wire's answer to a request that did not ask for a stream.
fn responses_unary(
    turn: &Turn,
    at: usize,
    delay: Option<&(usize, std::time::Duration)>,
) -> ResponseTemplate {
    let Turn::Text(text) = turn else {
        return ResponseTemplate::new(500).set_body_string("only text answers are scripted here");
    };
    let mut response = ResponseTemplate::new(200).set_body_json(json!({
        "id": format!("resp_{at}"), "object": "response", "created_at": 1,
        "status": "completed", "model": "scripted",
        "output": [{"type": "message", "id": format!("msg_{at}"), "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": text, "annotations": []}]}],
        "usage": {"input_tokens": 1000, "output_tokens": 500, "total_tokens": 1500},
    }));
    if let Some((_, delay)) = delay {
        response = response.set_delay(*delay);
    }
    response
}

/// The Responses wire's streamed answer.
fn responses_stream(
    turn: &Turn,
    at: usize,
    delay: Option<&(usize, std::time::Duration)>,
) -> ResponseTemplate {
    let mut frames = vec![event(
        "response.created",
        json!({"type": "response.created",
               "response": {"id": format!("resp_{at}"), "status": "in_progress", "output": []}}),
    )];
    let output = match turn {
        Turn::Text(text) => {
            let message = |content: Value, status: &str| {
                json!({"type": "message", "id": format!("msg_{at}"), "role": "assistant",
                       "status": status, "content": content})
            };
            frames.push(event(
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": message(json!([]), "in_progress")}),
            ));
            frames.push(event(
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "output_index": 0,
                       "content_index": 0, "item_id": format!("msg_{at}"), "delta": text}),
            ));
            let done = message(
                json!([{"type": "output_text", "text": text, "annotations": []}]),
                "completed",
            );
            frames.push(event(
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 0, "item": done}),
            ));
            json!([done])
        }
        Turn::Tool { id, name, args } => {
            let call = |arguments: &str, status: &str| {
                json!({"type": "function_call", "id": format!("fc_{at}"), "call_id": id,
                       "name": name, "arguments": arguments, "status": status})
            };
            frames.push(event(
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": call("", "in_progress")}),
            ));
            frames.push(event(
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                       "item_id": format!("fc_{at}"), "delta": args.to_string()}),
            ));
            let done = call(&args.to_string(), "completed");
            frames.push(event(
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 0, "item": done}),
            ));
            json!([done])
        }
    };
    frames.push(event(
        "response.completed",
        json!({"type": "response.completed", "response": {
            "id": format!("resp_{at}"), "status": "completed", "output": output,
            "usage": {"input_tokens": 1000, "output_tokens": 500, "total_tokens": 1500},
        }}),
    ));
    let mut response = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(frames.concat().into_bytes(), "text/event-stream");
    if let Some((_, delay)) = delay {
        response = response.set_delay(*delay);
    }
    response
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
        let delay = route.delays.iter().find(|(i, _)| *i == at);
        if request.url.path().ends_with("/responses") {
            return if body["stream"] == true {
                responses_stream(turn, at, delay)
            } else {
                responses_unary(turn, at, delay)
            };
        }
        if body["stream"] != true {
            return unary(turn, delay);
        }
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
        let mut response = ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(frames.concat().into_bytes(), "text/event-stream");
        if let Some((_, delay)) = route.delays.iter().find(|(turn, _)| *turn == at) {
            response = response.set_delay(*delay);
        }
        response
    }
}

struct SharedRouted(std::sync::Arc<Routed>);

impl Respond for SharedRouted {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.0.respond(request)
    }
}

pub async fn start(routes: Vec<Route>) -> MockServer {
    let server = MockServer::start().await;
    let routed = std::sync::Arc::new(Routed {
        routes: routes
            .into_iter()
            .map(|r| (r, AtomicUsize::new(0)))
            .collect(),
    });
    for route in ["/chat/completions", "/responses"] {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(SharedRouted(routed.clone()))
            .mount(&server)
            .await;
    }
    server
}
