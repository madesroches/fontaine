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

impl Respond for Scripted {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let at = self.next.fetch_add(1, Ordering::SeqCst);
        // Past the end of the script the last turn repeats, so a runaway loop is still served.
        let Some(turn) = self.turns.get(at).or(self.turns.last()) else {
            return ResponseTemplate::new(500).set_body_string("empty script");
        };
        match self.wire {
            Wire::Chat => chat_response(turn, self.usage),
            Wire::Responses => responses_response(turn, self.usage, at),
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
        let server = MockServer::start().await;
        let route = match wire {
            Wire::Chat => "/chat/completions",
            Wire::Responses => "/responses",
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

    pub fn url(&self) -> String {
        self.server.uri()
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
        sandbox.user_config(&format!(
            "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{url}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n"
        ));
        sandbox
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
            .env_remove("FONTAINE_SPIKE_ECHO_TOOL")
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
        let mut cmd = self.command(args);
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

    /// A headless run with the echo spike tool and an explicit permission mode.
    pub async fn headless(&self, args: &[&str]) -> Output {
        let mut all = vec!["-p", "--permission-mode", "manual"];
        all.extend_from_slice(args);
        self.run(&all).await
    }

    pub async fn headless_with_echo(&self, args: &[&str]) -> Output {
        let mut all = vec!["-p", "--permission-mode", "manual"];
        all.extend_from_slice(args);
        let mut cmd = self.command(&all);
        cmd.env("FONTAINE_SPIKE_ECHO_TOOL", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd.output().await.unwrap();
        Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
