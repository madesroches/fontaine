#![allow(dead_code)]

//! The offline driver every headless test shares: a scripted `wiremock` endpoint speaking
//! the Chat Completions wire (the `openai-compatible` kind) or the Responses wire (the
//! catalog-priced `openai` kind), plus a temp config and data dir around the `fontaine`
//! binary.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// One scripted model reply.
#[derive(Clone, Debug)]
pub enum Turn {
    /// Answer text, streamed in two deltas.
    Text(String),
    /// A single tool call.
    Tool {
        id: String,
        name: String,
        args: Value,
    },
    /// An HTTP failure.
    Fail(u16),
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

#[derive(Clone, Copy, Debug)]
pub enum Wire {
    Chat,
    Responses,
}

/// Token counts every scripted reply reports.
#[derive(Clone, Copy, Debug)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

impl Default for Usage {
    fn default() -> Self {
        Usage {
            input: 1000,
            output: 500,
        }
    }
}

struct Scripted {
    wire: Wire,
    turns: Vec<Turn>,
    usage: Usage,
    next: AtomicUsize,
}

fn sse(frames: impl IntoIterator<Item = String>) -> ResponseTemplate {
    let body: String = frames.into_iter().collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(body.into_bytes(), "text/event-stream")
}

fn halves(text: &str) -> (String, String) {
    let mid = text.chars().count() / 2;
    let first: String = text.chars().take(mid).collect();
    let second: String = text.chars().skip(mid).collect();
    (first, second)
}

fn chat_chunk(delta: Value, finish: Option<&str>) -> String {
    let chunk = json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "scripted",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    });
    format!("data: {chunk}\n\n")
}

fn chat_response(turn: &Turn, usage: Usage) -> ResponseTemplate {
    let mut frames = Vec::new();
    match turn {
        Turn::Text(text) => {
            let (a, b) = halves(text);
            frames.push(chat_chunk(json!({"role": "assistant", "content": a}), None));
            frames.push(chat_chunk(json!({"content": b}), None));
            frames.push(chat_chunk(json!({}), Some("stop")));
        }
        Turn::Tool { id, name, args } => {
            frames.push(chat_chunk(
                json!({"role": "assistant", "tool_calls": [{
                    "index": 0,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args.to_string()},
                }]}),
                None,
            ));
            frames.push(chat_chunk(json!({}), Some("tool_calls")));
        }
        Turn::Fail(status) => return ResponseTemplate::new(*status).set_body_string("scripted"),
    }
    let usage = json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "scripted",
        "choices": [],
        "usage": {
            "prompt_tokens": usage.input,
            "completion_tokens": usage.output,
            "total_tokens": usage.input + usage.output,
        },
    });
    frames.push(format!("data: {usage}\n\n"));
    frames.push("data: [DONE]\n\n".to_string());
    sse(frames)
}

/// The reply to a Chat Completions request that did not ask for a stream, such as the
/// guardian's typed-extraction call.
fn chat_json(turn: &Turn, usage: Usage) -> ResponseTemplate {
    let message = match turn {
        Turn::Text(text) => json!({"role": "assistant", "content": text}),
        Turn::Tool { id, name, args } => json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": args.to_string()},
            }],
        }),
        Turn::Fail(status) => return ResponseTemplate::new(*status).set_body_string("scripted"),
    };
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion",
        "created": 1,
        "model": "scripted",
        "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": usage.input,
            "completion_tokens": usage.output,
            "total_tokens": usage.input + usage.output,
        },
    }))
}

fn event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn responses_response(turn: &Turn, usage: Usage, id: usize) -> ResponseTemplate {
    let mut frames = vec![event(
        "response.created",
        json!({"type": "response.created",
               "response": {"id": format!("resp_{id}"), "status": "in_progress", "output": []}}),
    )];
    let output = match turn {
        Turn::Text(text) => {
            let (a, b) = halves(text);
            let message = |content: Value, status: &str| {
                json!({"type": "message", "id": format!("msg_{id}"), "role": "assistant",
                       "status": status, "content": content})
            };
            frames.push(event(
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": message(json!([]), "in_progress")}),
            ));
            for delta in [a, b] {
                frames.push(event(
                    "response.output_text.delta",
                    json!({"type": "response.output_text.delta", "output_index": 0,
                           "content_index": 0, "item_id": format!("msg_{id}"), "delta": delta}),
                ));
            }
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
        Turn::Tool {
            id: call_id,
            name,
            args,
        } => {
            let call = |arguments: &str, status: &str| {
                json!({"type": "function_call", "id": format!("fc_{id}"), "call_id": call_id,
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
                       "item_id": format!("fc_{id}"), "delta": args.to_string()}),
            ));
            let done = call(&args.to_string(), "completed");
            frames.push(event(
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 0, "item": done}),
            ));
            json!([done])
        }
        Turn::Fail(status) => return ResponseTemplate::new(*status).set_body_string("scripted"),
    };
    frames.push(event(
        "response.completed",
        json!({"type": "response.completed", "response": {
            "id": format!("resp_{id}"),
            "status": "completed",
            "output": output,
            "usage": {
                "input_tokens": usage.input,
                "output_tokens": usage.output,
                "total_tokens": usage.input + usage.output,
            },
        }}),
    ));
    sse(frames)
}

/// The reply to a request that did not ask for a stream, such as the compaction summarizer's.
fn responses_json(turn: &Turn, usage: Usage, id: usize) -> ResponseTemplate {
    let Turn::Text(text) = turn else {
        return ResponseTemplate::new(500).set_body_string("only text answers are scripted here");
    };
    ResponseTemplate::new(200).set_body_json(json!({
        "id": format!("resp_{id}"),
        "object": "response",
        "created_at": 1,
        "status": "completed",
        "model": "scripted",
        "output": [{"type": "message", "id": format!("msg_{id}"), "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": text, "annotations": []}]}],
        "usage": {
            "input_tokens": usage.input,
            "output_tokens": usage.output,
            "total_tokens": usage.input + usage.output,
        },
    }))
}

impl Respond for Scripted {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let at = self.next.fetch_add(1, Ordering::SeqCst);
        // Past the end of the script the last turn repeats, so a runaway loop is still served.
        let Some(turn) = self.turns.get(at).or(self.turns.last()) else {
            return ResponseTemplate::new(500).set_body_string("empty script");
        };
        let streamed = serde_json::from_slice::<Value>(&request.body)
            .is_ok_and(|body| body["stream"] == json!(true));
        match self.wire {
            Wire::Chat if !streamed => chat_json(turn, self.usage),
            Wire::Chat => chat_response(turn, self.usage),
            Wire::Responses if !streamed => responses_json(turn, self.usage, at),
            Wire::Responses => responses_response(turn, self.usage, at),
        }
    }
}

/// One conversation of a routed endpoint: the requests whose first user message contains
/// `marker` are answered from `turns` in order, the last repeating, each after `delay`.
#[derive(Clone, Debug)]
pub struct Route {
    pub marker: String,
    pub turns: Vec<Turn>,
    pub delay: std::time::Duration,
}

impl Route {
    pub fn new(marker: &str, turns: Vec<Turn>) -> Route {
        Route {
            marker: marker.to_string(),
            turns,
            delay: std::time::Duration::ZERO,
        }
    }

    pub fn slow(mut self, delay: std::time::Duration) -> Route {
        self.delay = delay;
        self
    }
}

/// An endpoint for tests with several agents talking at once: which conversation a request
/// belongs to is read from its first user message, so the order requests arrive in does not
/// matter.
struct Routed {
    wire: Wire,
    routes: Vec<(Route, AtomicUsize)>,
    usage: Usage,
}

fn text_of(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// The first user message of a request body, on either wire.
fn first_user(body: &Value) -> String {
    let items = body["messages"]
        .as_array()
        .or_else(|| body["input"].as_array());
    items
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "user")
        .find_map(|m| text_of(&m["content"]))
        .unwrap_or_default()
}

impl Respond for Routed {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let first = first_user(&body);
        let Some((route, next)) = self
            .routes
            .iter()
            .find(|(route, _)| first.contains(&route.marker))
        else {
            return ResponseTemplate::new(500).set_body_string(format!("no route for: {first}"));
        };
        let at = next.fetch_add(1, Ordering::SeqCst);
        let turn = route.turns.get(at).or(route.turns.last()).cloned();
        let streamed = body["stream"] == json!(true);
        let reply = match (self.wire, turn) {
            (_, None) => ResponseTemplate::new(500).set_body_string("empty route"),
            (Wire::Chat, Some(turn)) if !streamed => chat_json(&turn, self.usage),
            (Wire::Chat, Some(turn)) => chat_response(&turn, self.usage),
            (Wire::Responses, Some(turn)) if !streamed => responses_json(&turn, self.usage, at),
            (Wire::Responses, Some(turn)) => responses_response(&turn, self.usage, at),
        };
        if route.delay.is_zero() {
            reply
        } else {
            reply.set_delay(route.delay)
        }
    }
}

/// A scripted model endpoint.
pub struct Endpoint {
    pub server: MockServer,
}

impl Endpoint {
    pub async fn start(wire: Wire, turns: Vec<Turn>) -> Endpoint {
        Endpoint::start_with(wire, turns, Usage::default()).await
    }

    pub async fn start_with(wire: Wire, turns: Vec<Turn>, usage: Usage) -> Endpoint {
        Endpoint::start_under(wire, turns, usage, "").await
    }

    /// An endpoint whose route sits under `prefix`, as a gateway's does.
    pub async fn start_under(wire: Wire, turns: Vec<Turn>, usage: Usage, prefix: &str) -> Endpoint {
        let server = MockServer::start().await;
        let route = match wire {
            Wire::Chat => format!("{prefix}/chat/completions"),
            Wire::Responses => format!("{prefix}/responses"),
        };
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(Scripted {
                wire,
                turns,
                usage,
                next: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        Endpoint { server }
    }

    /// An endpoint that answers each conversation from its own route.
    pub async fn start_routed(wire: Wire, routes: Vec<Route>) -> Endpoint {
        let server = MockServer::start().await;
        let path_for = match wire {
            Wire::Chat => "/chat/completions",
            Wire::Responses => "/responses",
        };
        Mock::given(method("POST"))
            .and(path(path_for))
            .respond_with(Routed {
                wire,
                routes: routes
                    .into_iter()
                    .map(|r| (r, AtomicUsize::new(0)))
                    .collect(),
                usage: Usage::default(),
            })
            .mount(&server)
            .await;
        Endpoint { server }
    }

    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// The headers of the requests the model was sent, in order.
    pub async fn request_headers(&self) -> Vec<wiremock::http::HeaderMap> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| r.headers.clone())
            .collect()
    }

    /// The JSON bodies of the requests the model was sent, in order.
    pub async fn requests(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
            .collect()
    }
}

/// A scratch world around the binary.
pub struct Sandbox {
    _tmp: TempDir,
    pub work: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    env: Vec<(String, String)>,
}

pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// The stdout lines parsed as JSON.
    pub fn lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("`{l}`: {e}")))
            .collect()
    }
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let sandbox = Sandbox {
            work: root.join("work"),
            config: root.join("config"),
            data: root.join("data"),
            _tmp: tmp,
            env: Vec::new(),
        };
        for dir in [&sandbox.work, &sandbox.config, &sandbox.data] {
            std::fs::create_dir_all(dir).unwrap();
        }
        sandbox
    }

    /// A sandbox whose only model is the scripted `openai-compatible` endpoint at `url`.
    pub fn compatible(url: &str) -> Sandbox {
        let sandbox = Sandbox::new();
        sandbox.point_at(url);
        sandbox
    }

    /// Makes the scripted `openai-compatible` endpoint at `url` the only model.
    pub fn point_at(&self, url: &str) {
        self.user_config(&format!(
            "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{url}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n"
        ));
    }

    /// A sandbox whose model is a catalog-priced `openai` account at `url`.
    pub fn priced(url: &str) -> Sandbox {
        let mut sandbox = Sandbox::new();
        sandbox.user_config(&format!(
            "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{url}\"\napi_key_env = \"PAID_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n"
        ));
        sandbox.set_env("PAID_KEY", "sk-test-not-a-real-key");
        sandbox
    }

    pub fn user_config(&self, text: &str) {
        write(&self.config.join("config.toml"), text);
    }

    pub fn set_env(&mut self, name: &str, value: &str) {
        self.env.push((name.to_string(), value.to_string()));
    }

    pub fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_fontaine"));
        cmd.args(args)
            .current_dir(&self.work)
            .env("FONTAINE_CONFIG_DIR", &self.config)
            .env("FONTAINE_DATA_DIR", &self.data)
            .env_remove("FONTAINE_LOG")
            .env_remove("MICROMEGAS_TELEMETRY_URL")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .kill_on_drop(true);
        for (name, value) in &self.env {
            cmd.env(name, value);
        }
        cmd
    }

    pub async fn run(&self, args: &[&str]) -> Output {
        self.run_with_stdin(args, "").await
    }

    pub async fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        self.run_in_with_stdin(&self.work, args, stdin).await
    }

    /// A run started from `dir` instead of the sandbox's workspace.
    pub async fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.run_in_with_stdin(dir, args, "").await
    }

    async fn run_in_with_stdin(&self, dir: &Path, args: &[&str], stdin: &str) -> Output {
        let mut cmd = self.command(args);
        cmd.current_dir(dir);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        let text = stdin.to_string();
        let writer = tokio::spawn(async move {
            let _ = input.write_all(text.as_bytes()).await;
            drop(input);
        });
        let out = child.wait_with_output().await.unwrap();
        let _ = writer.await;
        Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// A headless run with an explicit permission mode.
    pub async fn headless(&self, args: &[&str]) -> Output {
        let mut all = vec!["-p", "--permission-mode", "manual"];
        all.extend_from_slice(args);
        self.run(&all).await
    }

    /// Creates `relative` under the launch directory.
    pub fn add_file(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.work.join(relative);
        write(&path, text);
        path
    }
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
