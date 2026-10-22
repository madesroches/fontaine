//! The non-interactive surface, `fontaine -p`. Its output is a versioned contract: see
//! `docs/headless.md` and `docs/schemas/headless-v1.schema.json`.

use std::io::{IsTerminal, Write};
use std::path::Path;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::{Decision, DecisionRecord};
use fontaine_core::config::PermissionMode;
use fontaine_core::gate::GuardianUsage;
use fontaine_core::ledger::Record;
use fontaine_core::trust;
use fontaine_core::turn::{
    DEFAULT_MAX_TURNS, Flow, SessionEvent, TurnOutcome, TurnParams, run_turn,
};
use rig_core::schemars::Schema;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::cli::{InputFormat, OutputFormat};

pub const SCHEMA_VERSION: u32 = 1;

pub struct Options {
    pub prompt: Option<String>,
    pub input_format: InputFormat,
    pub output_format: OutputFormat,
    pub include_partial_messages: bool,
    pub max_turns: Option<usize>,
    pub max_budget_usd: Option<f64>,
    pub json_schema: Option<Schema>,
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
    /// Every call the pipeline denied during the turn.
    denials: Vec<Value>,
}

/// Session spend: the main model plus the guardian. Null only when neither is priced.
fn session_cost(main: Option<f64>, guardian: Option<f64>) -> Option<f64> {
    match (main, guardian) {
        (None, None) => None,
        (m, g) => Some(m.unwrap_or(0.0) + g.unwrap_or(0.0)),
    }
}

fn result_event(
    session_id: &str,
    outcome: &Outcome,
    totals: &Totals,
    guardian: &GuardianUsage,
) -> Value {
    let mut fields = json!({
        "subtype": outcome.subtype.name(),
        "is_error": outcome.subtype != Subtype::Success,
        "result": outcome.text,
        "error": outcome.error,
        "num_turns": totals.model_calls,
        "duration_ms": outcome.started.elapsed().as_millis() as u64,
        "total_cost_usd": session_cost(totals.cost_usd, guardian.cost_usd),
        "usage": {
            "main": {
                "input_tokens": totals.input_tokens,
                "output_tokens": totals.output_tokens,
                "cost_usd": totals.cost_usd,
            },
            "guardian": {
                "input_tokens": guardian.input_tokens,
                "output_tokens": guardian.output_tokens,
                "cost_usd": guardian.cost_usd,
            },
            "subagents": { "input_tokens": 0, "output_tokens": 0, "cost_usd": null },
        },
        "denials": outcome.denials,
    });
    if let (Some(structured), Some(map)) = (&outcome.structured, fields.as_object_mut()) {
        map.insert("structured_output".into(), structured.clone());
    }
    event("result", session_id, fields)
}

fn init_event(runtime: &AgentRuntime, data_dir: &Path) -> Value {
    let config = &runtime.config;
    // What is in force, not what was configured: the guardian may be the default pick or the
    // main model, and the profile may have fallen back to `full`.
    let guardian = runtime.state.guardian();
    let (guardian_model, guardian_available) = if runtime.models.guardian.is_none() {
        (Value::Null, Value::Null)
    } else {
        (
            json!(guardian.label()),
            json!(guardian.unavailable_reason().is_none()),
        )
    };
    let roots: Vec<String> = runtime
        .roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
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
    let trusted = trust::is_trusted(data_dir, &config.workspace);
    event(
        "init",
        &runtime.session.id,
        json!({
            "model": runtime.models.main.reference.to_string(),
            "guardian_model": guardian_model,
            "guardian_available": guardian_available,
            "guardian_profile": runtime.guardian_profile.as_str(),
            "permission_mode": runtime.mode.as_str(),
            "workspace_roots": roots,
            "config_sources": sources,
            "workspace_trust": if trusted { "trusted" } else { "untrusted" },
            "suspended_allow_rules": if runtime.mode == PermissionMode::Auto {
                runtime.state.suspended_allow_rules()
            } else {
                Vec::new()
            },
            "tools": runtime.tool_names,
        }),
    )
}

fn gate_decision_event(session_id: &str, record: &DecisionRecord) -> Value {
    event(
        "gate_decision",
        session_id,
        json!({
            "tool_use_id": record.tool_use_id.clone().unwrap_or_default(),
            "tool": record.tool,
            "tier": record.tier.as_str(),
            "decision": record.decision.as_str(),
            "rule": record.rule,
            "explanation": record.explanation,
            "confidence": record.confidence,
            "guardian_model": record.guardian_model,
            "fail_closed_reason": record.fail_closed_reason,
        }),
    )
}

/// One user message: runs the agent to its answer, a limit or a failure.
async fn run_one(
    runtime: &AgentRuntime,
    options: &Options,
    text: String,
    totals: &mut Totals,
    stream_events: bool,
) -> Outcome {
    let started = std::time::Instant::now();
    let sid = runtime.session.id.as_str();
    let mut denials: Vec<Value> = Vec::new();
    let emit = |value: Value| {
        if stream_events {
            emit_line(&value);
        }
    };
    let mut on_event = |event_in: SessionEvent| -> Flow {
        match event_in {
            SessionEvent::AssistantDelta(text) => {
                if options.include_partial_messages {
                    emit(event("assistant_delta", sid, json!({ "text": text })));
                }
            }
            SessionEvent::Assistant(text) => {
                emit(event("assistant", sid, json!({ "text": text })));
            }
            SessionEvent::ToolCall { id, tool, args } => emit(event(
                "tool_call",
                sid,
                json!({ "tool_use_id": id, "tool": tool, "args": args }),
            )),
            SessionEvent::Decision(record) => {
                if record.decision == Decision::Deny {
                    denials.push(json!({
                        "tool": record.tool,
                        "tier": record.tier.as_str(),
                        "rule": record.rule,
                        "explanation": record.explanation,
                    }));
                }
                emit(gate_decision_event(sid, &record));
            }
            SessionEvent::ToolResult {
                id,
                tool,
                success,
                text,
            } => emit(event(
                "tool_result",
                sid,
                json!({ "tool_use_id": id, "tool": tool, "success": success, "text": text }),
            )),
            SessionEvent::Usage { usage, calls_tool } => {
                totals.add(&usage);
                // The cap is checked between turns: a spent budget stops the run before its
                // tool calls run, but never discards an answer already complete.
                if calls_tool
                    && let (Some(cap), Some(spent)) = (
                        options.max_budget_usd,
                        session_cost(totals.cost_usd, runtime.state.guardian_usage().cost_usd),
                    )
                    && spent > cap
                {
                    return Flow::Stop;
                }
            }
        }
        Flow::Continue
    };
    let turn = run_turn(
        TurnParams {
            agent: &runtime.agent,
            state: &runtime.state,
            prompt: text,
            max_turns: options.max_turns.unwrap_or(DEFAULT_MAX_TURNS),
        },
        &mut on_event,
    )
    .await;

    let (subtype, text, error) = match turn {
        TurnOutcome::Answer(text) => (Subtype::Success, text, None),
        TurnOutcome::Stopped => (
            Subtype::MaxBudget,
            String::new(),
            Some("the run's spend exceeded --max-budget-usd".to_string()),
        ),
        TurnOutcome::MaxTurns(message) => (Subtype::MaxTurns, String::new(), Some(message)),
        TurnOutcome::Failed(message) => (Subtype::Execution, String::new(), Some(message)),
    };
    let mut outcome = Outcome {
        subtype,
        text,
        structured: None,
        error,
        started,
        denials,
    };
    if outcome.subtype == Subtype::Success && options.json_schema.is_some() {
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

/// Startup notices that matter to a run that can reach the guardian: one stderr line each, and
/// one more when the guardian cannot be asked at all, since every adjudication then fails
/// closed.
fn announce(runtime: &AgentRuntime) {
    if !matches!(runtime.mode, PermissionMode::Auto | PermissionMode::Plan) {
        return;
    }
    for notice in &runtime.notices {
        eprintln!("fontaine: {notice}");
    }
    let guardian = runtime.state.guardian();
    if runtime.models.guardian.is_some()
        && let Some(reason) = guardian.unavailable_reason()
    {
        eprintln!(
            "fontaine: the guardian model {} is unavailable ({reason}); every call that needs it is denied with GuardianUnavailable",
            guardian.label()
        );
    }
}

/// Runs the headless session and returns the process exit code: 0 completed, 1 run error,
/// 2 usage error.
pub async fn run(mut options: Options, runtime: AgentRuntime, data_dir: &Path) -> u8 {
    let mut input = match open_input(&mut options).await {
        Ok(input) => input,
        Err(message) => {
            eprintln!("fontaine: {message}");
            return 2;
        }
    };

    let session_id = runtime.session.id.clone();
    runtime.record_session_start().await;
    announce(&runtime);
    let stream_events = options.output_format == OutputFormat::StreamJson;
    if stream_events {
        emit_line(&init_event(&runtime, data_dir));
    }

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
        runtime.append(Record::UserMessage { text: text.clone() });
        // A text-only answer that crossed the cap ends its own turn as a success, so the cap is
        // enforced here before a further paid call starts. The summarizing call that may
        // precede a turn is paid too, so the cap is checked again after it.
        let over_cap = |totals: &Totals| {
            matches!(
                (
                    options.max_budget_usd,
                    session_cost(totals.cost_usd, runtime.state.guardian_usage().cost_usd)
                ),
                (Some(cap), Some(spent)) if spent > cap
            )
        };
        let budget_stop = || Outcome {
            subtype: Subtype::MaxBudget,
            text: String::new(),
            structured: None,
            error: Some("the run's spend exceeded --max-budget-usd".to_string()),
            started: std::time::Instant::now(),
            denials: Vec::new(),
        };
        let outcome = if over_cap(&totals) {
            budget_stop()
        } else {
            if let Some(usage) = runtime
                .session
                .compact_if_needed(&runtime.models.main.completion)
                .await
            {
                totals.add(&usage);
            }
            if over_cap(&totals) {
                budget_stop()
            } else {
                run_one(&runtime, &options, text, &mut totals, stream_events).await
            }
        };
        let failed = outcome.subtype != Subtype::Success;
        if stream_events {
            emit_line(&result_event(
                &session_id,
                &outcome,
                &totals,
                &runtime.state.guardian_usage(),
            ));
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
        OutputFormat::Json => emit_line(&result_event(
            &session_id,
            &last,
            &totals,
            &runtime.state.guardian_usage(),
        )),
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

#[cfg(test)]
mod tests {
    use super::session_cost;

    #[test]
    fn session_cost_adds_the_guardian_and_is_null_only_when_neither_is_priced() {
        assert_eq!(session_cost(None, None), None);
        assert_eq!(session_cost(Some(0.5), None), Some(0.5));
        assert_eq!(session_cost(None, Some(0.25)), Some(0.25));
        assert_eq!(session_cost(Some(0.5), Some(0.25)), Some(0.75));
    }
}
