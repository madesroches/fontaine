//! The non-interactive surface, `fontaine -p`. Its output is a versioned contract: see
//! `docs/headless.md` and `docs/schemas/headless-v1.schema.json`.

use std::io::{IsTerminal, Write};
use std::path::Path;

use fontaine_core::config::{Config, PermissionMode};
use fontaine_core::models::{GuardianModel, Models};
use fontaine_core::trust;
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::completion::PromptError;
use rig_agent::prelude::*;
use rig_core::message::{AssistantContent, ToolResultContent};
use rig_core::schemars::Schema;
use rig_core::streaming::{Item, StreamEvent};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use uuid::Uuid;

use crate::cli::{InputFormat, OutputFormat};
use crate::echo::Echo;

pub const SCHEMA_VERSION: u32 = 1;

/// Without `--max-turns`, a turn is still bounded so a looping model cannot run forever.
pub const DEFAULT_MAX_TURNS: usize = 1000;

const BASE_PROMPT: &str = "You are Fontaine, a terminal coding agent.";

pub struct Options {
    pub prompt: Option<String>,
    pub input_format: InputFormat,
    pub output_format: OutputFormat,
    pub include_partial_messages: bool,
    pub max_turns: Option<usize>,
    pub max_budget_usd: Option<f64>,
    pub json_schema: Option<Schema>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub mode: PermissionMode,
    pub echo_tool: bool,
}

pub struct Context<'a> {
    pub config: &'a Config,
    pub models: Models,
    pub data_dir: &'a Path,
}

pub fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "manual",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::Auto => "auto",
    }
}

fn emit_line(value: &Value) {
    let mut out = std::io::stdout().lock();
    // A closed pipe is the consumer's decision; the run's exit code is unaffected.
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn event(kind: &str, session_id: &str, mut fields: Value) -> Value {
    if let Some(map) = fields.as_object_mut() {
        map.insert("type".into(), json!(kind));
        map.insert("schema_version".into(), json!(SCHEMA_VERSION));
        map.insert("session_id".into(), json!(session_id));
    }
    fields
}

enum Input {
    Single(Option<String>),
    Lines(tokio::io::Lines<BufReader<tokio::io::Stdin>>),
}

impl Input {
    /// The next user message, `None` at the end of input. An error is a usage error.
    async fn next(&mut self) -> Result<Option<String>, String> {
        match self {
            Input::Single(prompt) => Ok(prompt.take()),
            Input::Lines(lines) => loop {
                let line = lines
                    .next_line()
                    .await
                    .map_err(|e| format!("cannot read stdin: {e}"))?;
                let Some(line) = line else { return Ok(None) };
                if line.trim().is_empty() {
                    continue;
                }
                return parse_user_line(&line).map(Some);
            },
        }
    }
}

fn parse_user_line(line: &str) -> Result<String, String> {
    let text = user_line_text(line)?;
    if text.trim().is_empty() {
        return Err("input line holds an empty prompt".to_string());
    }
    Ok(text)
}

fn user_line_text(line: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| format!("input line is not JSON: {e}"))?;
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return Err("input line is not a `user` message".to_string());
    }
    let content = value
        .pointer("/message/content")
        .ok_or("input line has no `message.content`")?;
    match content {
        Value::String(text) => Ok(text.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(t) = block.get("text").and_then(Value::as_str)
                {
                    text.push_str(t);
                }
            }
            Ok(text)
        }
        _ => Err("`message.content` must be a string or an array of text blocks".to_string()),
    }
}

/// How long a prompt argument waits for piped stdin to start before running without it.
const STDIN_FIRST_DATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Reads stdin on a detached thread: a blocking read on a pipe that never closes would
/// otherwise keep the runtime from shutting down.
async fn read_piped_stdin() -> Result<String, String> {
    use std::io::Read;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(Vec::new()));
                    return;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    let read_error = |e: std::io::Error| format!("cannot read the prompt from stdin: {e}");
    let mut bytes = Vec::new();
    match tokio::time::timeout(STDIN_FIRST_DATA_TIMEOUT, rx.recv()).await {
        Err(_) => {
            eprintln!(
                "Warning: no stdin data received in {}s, proceeding without it. If piping from a slow command, redirect stdin explicitly: < /dev/null to skip, or wait longer.",
                STDIN_FIRST_DATA_TIMEOUT.as_secs()
            );
            return Ok(String::new());
        }
        Ok(None) => return Ok(String::new()),
        Ok(Some(chunk)) => bytes.extend(chunk.map_err(read_error)?),
    }
    while !bytes.is_empty() {
        match rx.recv().await {
            Some(chunk) => {
                let chunk = chunk.map_err(read_error)?;
                if chunk.is_empty() {
                    break;
                }
                bytes.extend(chunk);
            }
            None => break,
        }
    }
    String::from_utf8(bytes).map_err(|e| format!("cannot read the prompt from stdin: {e}"))
}

/// Reads the prompt the run will use, or the stream the prompts come from.
async fn open_input(options: &mut Options) -> Result<Input, String> {
    match options.input_format {
        InputFormat::StreamJson => Ok(Input::Lines(BufReader::new(tokio::io::stdin()).lines())),
        InputFormat::Text => {
            let piped = if options.prompt.is_none() {
                if std::io::stdin().is_terminal() {
                    return Err("no prompt: pass it as an argument or on stdin".to_string());
                }
                let mut text = String::new();
                tokio::io::stdin()
                    .read_to_string(&mut text)
                    .await
                    .map_err(|e| format!("cannot read the prompt from stdin: {e}"))?;
                text
            } else if !std::io::stdin().is_terminal() {
                read_piped_stdin().await?
            } else {
                String::new()
            };
            let prompt = match options.prompt.take() {
                Some(prompt) if piped.trim().is_empty() => prompt,
                Some(prompt) => format!("{prompt}\n{}", piped.trim_end()),
                None => piped,
            };
            if prompt.trim().is_empty() {
                return Err("no prompt: pass it as an argument or on stdin".to_string());
            }
            Ok(Input::Single(Some(prompt)))
        }
    }
}

#[derive(Default)]
struct Totals {
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
    model_calls: u64,
}

impl Totals {
    fn add(&mut self, usage: &rig_core::completion::Usage) {
        self.model_calls += 1;
        self.input_tokens += usage.input_tokens.unwrap_or(0);
        self.output_tokens += usage.output_tokens.unwrap_or(0);
        if let Some(cost) = usage.cost {
            *self.cost_usd.get_or_insert(0.0) += cost.total;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Subtype {
    Success,
    MaxTurns,
    MaxBudget,
    Execution,
}

impl Subtype {
    fn name(self) -> &'static str {
        match self {
            Subtype::Success => "success",
            Subtype::MaxTurns => "error_max_turns",
            Subtype::MaxBudget => "error_max_budget_usd",
            Subtype::Execution => "error_during_execution",
        }
    }
}

struct Outcome {
    subtype: Subtype,
    text: String,
    structured: Option<Value>,
    error: Option<String>,
    started: std::time::Instant,
}

fn result_event(session_id: &str, outcome: &Outcome, totals: &Totals) -> Value {
    let mut fields = json!({
        "subtype": outcome.subtype.name(),
        "is_error": outcome.subtype != Subtype::Success,
        "result": outcome.text,
        "error": outcome.error,
        "num_turns": totals.model_calls,
        "duration_ms": outcome.started.elapsed().as_millis() as u64,
        "total_cost_usd": totals.cost_usd,
        "usage": {
            "main": {
                "input_tokens": totals.input_tokens,
                "output_tokens": totals.output_tokens,
                "cost_usd": totals.cost_usd,
            },
            "guardian": { "input_tokens": 0, "output_tokens": 0, "cost_usd": null },
            "subagents": { "input_tokens": 0, "output_tokens": 0, "cost_usd": null },
        },
        "denials": [],
    });
    if let (Some(structured), Some(map)) = (&outcome.structured, fields.as_object_mut()) {
        map.insert("structured_output".into(), structured.clone());
    }
    event("result", session_id, fields)
}

fn init_event(session_id: &str, ctx: &Context<'_>, options: &Options, tools: &[&str]) -> Value {
    let config = ctx.config;
    let (guardian_model, guardian_available) = match &ctx.models.guardian {
        None => (Value::Null, Value::Null),
        Some(g) => (
            json!(g.reference().to_string()),
            json!(matches!(g, GuardianModel::Ready(_))),
        ),
    };
    let mut roots = vec![config.workspace.display().to_string()];
    roots.extend(config.permissions.additional_directories.iter().cloned());
    let sources: Vec<Value> = config
        .scopes
        .iter()
        .map(|scope| {
            json!({
                "scope": scope.slot.label(),
                "path": scope.path.as_ref().map(|p| p.display().to_string()),
                "loaded": scope.loaded,
            })
        })
        .collect();
    let trusted = trust::is_trusted(ctx.data_dir, &config.workspace);
    event(
        "init",
        session_id,
        json!({
            "model": ctx.models.main.reference.to_string(),
            "guardian_model": guardian_model,
            "guardian_available": guardian_available,
            "guardian_profile": match config.guardian.profile {
                fontaine_core::config::GuardianProfile::Full => "full",
                fontaine_core::config::GuardianProfile::Minimal => "minimal",
            },
            "permission_mode": mode_name(options.mode),
            "workspace_roots": roots,
            "config_sources": sources,
            "workspace_trust": if trusted { "trusted" } else { "untrusted" },
            "suspended_allow_rules": [],
            "tools": tools,
        }),
    )
}

fn build_agent(ctx: &Context<'_>, options: &Options) -> Agent {
    let mut preamble = options
        .system_prompt
        .clone()
        .unwrap_or_else(|| BASE_PROMPT.to_string());
    if let Some(extra) = &options.append_system_prompt {
        preamble.push_str("\n\n");
        preamble.push_str(extra);
    }
    let telemetry = &ctx.config.telemetry;
    let content = telemetry.enabled
        && (telemetry.log_user_prompts
            || telemetry.log_assistant_responses
            || telemetry.log_tool_details
            || telemetry.log_tool_content);
    let mut builder = AgentBuilder::new(ctx.models.main.completion.clone())
        .preamble(preamble)
        .record_content_telemetry(content);
    if options.echo_tool {
        builder = builder.tool(Echo);
    }
    if let Some(schema) = &options.json_schema {
        builder = builder.output_schema_raw(schema.clone());
    }
    builder.build()
}

fn tool_text(content: &[ToolResultContent]) -> String {
    content
        .iter()
        .map(|part| match part {
            ToolResultContent::Text(text) => text.text.clone(),
            ToolResultContent::Json { value } => value.to_string(),
            ToolResultContent::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct TurnRun<'a> {
    agent: &'a Agent,
    options: &'a Options,
    session_id: &'a str,
    stream_events: bool,
}

impl TurnRun<'_> {
    fn emit(&self, value: Value) {
        if self.stream_events {
            emit_line(&value);
        }
    }

    /// One user message: runs the agent to its answer, a limit or a failure.
    async fn run(
        &self,
        history: &mut Vec<rig_core::message::Message>,
        text: String,
        totals: &mut Totals,
    ) -> Outcome {
        let started = std::time::Instant::now();
        let max_turns = self.options.max_turns.unwrap_or(DEFAULT_MAX_TURNS);
        let interaction = tracing::info_span!(
            target: "fontaine::telemetry",
            "fontaine.interaction",
            prompt.id = %Uuid::new_v4(),
            prompt.length = text.len() as u64,
        );
        let runner = self
            .agent
            .prompt(rig_core::message::Message::user(text))
            .history(history.iter().cloned())
            .max_turns(max_turns);
        // The run adopts the span it is built under as its own agent span.
        let mut stream = interaction.in_scope(|| runner.stream());

        let sid = self.session_id;
        let mut turn_calls_tool = false;
        let mut over_budget = false;
        let mut final_response = None;
        let mut failure: Option<PromptError> = None;

        let consume = async {
            while let Some(item) = stream.next().await {
                let item = match item {
                    Ok(item) => item,
                    Err(e) => {
                        failure = Some(e);
                        return None;
                    }
                };
                match item {
                    MultiTurnStreamItem::StreamAssistantItem(Item::Event(ev)) => match ev {
                        StreamEvent::Text { text, .. } if self.options.include_partial_messages => {
                            self.emit(event("assistant_delta", sid, json!({ "text": text })));
                        }
                        StreamEvent::End { content, .. } => match content {
                            AssistantContent::Text(text) => {
                                self.emit(event("assistant", sid, json!({ "text": text.text })));
                            }
                            AssistantContent::ToolCall(_) => turn_calls_tool = true,
                            _ => {}
                        },
                        _ => {}
                    },
                    MultiTurnStreamItem::CompletionCall(call) => {
                        totals.add(&call.usage);
                        if let (Some(cap), Some(spent)) =
                            (self.options.max_budget_usd, totals.cost_usd)
                            && spent > cap
                        {
                            over_budget = true;
                        }
                        // The cap is checked between turns: a spent budget stops the run before
                        // its tool calls run, but never discards an answer already complete.
                        if over_budget && turn_calls_tool {
                            return None;
                        }
                        turn_calls_tool = false;
                    }
                    MultiTurnStreamItem::ToolCall { tool_call } => {
                        self.emit(event(
                            "tool_call",
                            sid,
                            json!({
                                "tool_use_id": tool_call.id.to_string(),
                                "tool": tool_call.function.name.to_string(),
                                "args": Value::Object(tool_call.function.arguments.clone()),
                            }),
                        ));
                    }
                    MultiTurnStreamItem::ToolResult { tool_result } => {
                        self.emit(event(
                            "tool_result",
                            sid,
                            json!({
                                "tool_use_id": tool_result.call.to_string(),
                                "tool": tool_result.name.to_string(),
                                "success": !tool_result.is_error,
                                "text": tool_text(&tool_result.content),
                            }),
                        ));
                    }
                    MultiTurnStreamItem::FinalResponse(response) => {
                        final_response = Some(response);
                    }
                    _ => {}
                }
            }
            Some(())
        };
        let finished = consume.await;
        drop(stream);

        let (subtype, text, error) = if finished.is_none() && failure.is_none() {
            (
                Subtype::MaxBudget,
                String::new(),
                Some("the run's spend exceeded --max-budget-usd".to_string()),
            )
        } else if let Some(e) = failure {
            let subtype = match e {
                PromptError::MaxTurns { .. } => Subtype::MaxTurns,
                _ => Subtype::Execution,
            };
            (subtype, String::new(), Some(e.to_string()))
        } else if let Some(response) = &final_response {
            history.extend(response.messages.iter().cloned());
            (Subtype::Success, response.output(), None)
        } else {
            (
                Subtype::Execution,
                String::new(),
                Some("the run ended without an answer".to_string()),
            )
        };

        let mut outcome = Outcome {
            subtype,
            text,
            structured: None,
            error,
            started,
        };
        if outcome.subtype == Subtype::Success && self.options.json_schema.is_some() {
            match serde_json::from_str::<Value>(&outcome.text) {
                Ok(value) => outcome.structured = Some(value),
                Err(e) => {
                    outcome.subtype = Subtype::Execution;
                    outcome.text.clear();
                    outcome.error = Some(format!("the final answer is not valid JSON: {e}"));
                }
            }
        }
        outcome
    }
}

/// Runs the headless session and returns the process exit code: 0 completed, 1 run error,
/// 2 usage error.
pub async fn run(mut options: Options, ctx: Context<'_>) -> u8 {
    let mut input = match open_input(&mut options).await {
        Ok(input) => input,
        Err(message) => {
            eprintln!("fontaine: {message}");
            return 2;
        }
    };

    let session_id = Uuid::new_v4().to_string();
    let agent = build_agent(&ctx, &options);
    let stream_events = options.output_format == OutputFormat::StreamJson;
    let tools: &[&str] = if options.echo_tool { &["echo"] } else { &[] };
    if stream_events {
        emit_line(&init_event(&session_id, &ctx, &options, tools));
    }

    let turn = TurnRun {
        agent: &agent,
        options: &options,
        session_id: &session_id,
        stream_events,
    };
    let mut history = Vec::new();
    let mut totals = Totals::default();
    let mut last: Option<Outcome> = None;
    let mut code = 0u8;
    loop {
        let text = match input.next().await {
            Ok(Some(text)) => text,
            Ok(None) => break,
            Err(message) => {
                eprintln!("fontaine: {message}");
                return 2;
            }
        };
        // A text-only answer that crossed the cap ends its own turn as a success, so the cap is
        // enforced here before a further paid call starts.
        let spent_over_cap = matches!(
            (options.max_budget_usd, totals.cost_usd),
            (Some(cap), Some(spent)) if spent > cap
        );
        let outcome = if spent_over_cap {
            Outcome {
                subtype: Subtype::MaxBudget,
                text: String::new(),
                structured: None,
                error: Some("the run's spend exceeded --max-budget-usd".to_string()),
                started: std::time::Instant::now(),
            }
        } else {
            turn.run(&mut history, text, &mut totals).await
        };
        let failed = outcome.subtype != Subtype::Success;
        if stream_events {
            emit_line(&result_event(&session_id, &outcome, &totals));
        }
        last = Some(outcome);
        if failed {
            code = 1;
            break;
        }
    }

    let Some(last) = last else {
        eprintln!("fontaine: no prompt: the input held no user message");
        return 2;
    };
    match options.output_format {
        OutputFormat::StreamJson => {}
        OutputFormat::Json => emit_line(&result_event(&session_id, &last, &totals)),
        OutputFormat::Text => {
            if let Some(error) = &last.error {
                eprintln!("fontaine: {error}");
            } else {
                let _ = writeln!(std::io::stdout().lock(), "{}", last.text);
            }
        }
    }
    code
}
