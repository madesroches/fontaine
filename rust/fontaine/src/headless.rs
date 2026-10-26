//! The non-interactive surface, `fontaine -p`. Its output is a versioned contract: see
//! `docs/headless.md` and `docs/schemas/headless-v1.schema.json`.

mod goal;

use std::io::{IsTerminal, Write};
use std::path::Path;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::{Decision, DecisionRecord};
use fontaine_core::config::PermissionMode;
use fontaine_core::gate::GuardianUsage;
use fontaine_core::goal::{GoalArg, Step, first_turn_prompt};
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
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    cost_usd: Option<f64>,
    model_calls: u64,
}

impl Totals {
    fn add(&mut self, usage: &rig_core::completion::Usage) {
        self.model_calls += 1;
        self.input_tokens += usage.input_tokens.unwrap_or(0);
        self.output_tokens += usage.output_tokens.unwrap_or(0);
        self.cache_read_input_tokens += usage.cached_input_tokens.unwrap_or(0);
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
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

/// Session spend: the main model, the guardian, the goal evaluator and the subagents' models.
/// Null only when none is priced.
fn session_cost(
    main: Option<f64>,
    guardian: Option<f64>,
    goal: Option<f64>,
    subagents: Option<f64>,
) -> Option<f64> {
    match (main, guardian, goal, subagents) {
        (None, None, None, None) => None,
        (m, g, e, s) => {
            Some(m.unwrap_or(0.0) + g.unwrap_or(0.0) + e.unwrap_or(0.0) + s.unwrap_or(0.0))
        }
    }
}

/// What the session has spent so far, subagents included.
fn spent(runtime: &AgentRuntime, totals: &Totals) -> Option<f64> {
    session_cost(
        totals.cost_usd,
        runtime.state.guardian_usage().cost_usd,
        runtime.state.goal_usage().cost_usd,
        runtime.state.subagent_usage().cost_usd,
    )
}

/// What the models other than the main one have cost.
struct Others {
    guardian: GuardianUsage,
    goal: GuardianUsage,
    subagents: GuardianUsage,
}

impl Others {
    fn of(runtime: &AgentRuntime) -> Others {
        Others {
            guardian: runtime.state.guardian_usage(),
            goal: runtime.state.goal_usage(),
            subagents: runtime.state.subagent_usage(),
        }
    }
}

fn result_event(
    session_id: &str,
    outcome: &Outcome,
    totals: &Totals,
    others: &Others,
    goal: Option<&goal::GoalReport>,
) -> Value {
    let (guardian, subagents) = (&others.guardian, &others.subagents);
    let mut fields = json!({
        "subtype": outcome.subtype.name(),
        "is_error": outcome.subtype != Subtype::Success,
        "result": outcome.text,
        "error": outcome.error,
        "num_turns": totals.model_calls,
        "duration_ms": outcome.started.elapsed().as_millis() as u64,
        "total_cost_usd": session_cost(
            totals.cost_usd,
            guardian.cost_usd,
            others.goal.cost_usd,
            subagents.cost_usd,
        ),
        "usage": {
            "main": {
                "input_tokens": totals.input_tokens,
                "output_tokens": totals.output_tokens,
                "cache_read_input_tokens": totals.cache_read_input_tokens,
                "cache_creation_input_tokens": totals.cache_creation_input_tokens,
                "cost_usd": totals.cost_usd,
            },
            "guardian": {
                "input_tokens": guardian.input_tokens,
                "output_tokens": guardian.output_tokens,
                "cost_usd": guardian.cost_usd,
            },
            "subagents": {
                "input_tokens": subagents.input_tokens,
                "output_tokens": subagents.output_tokens,
                "cost_usd": subagents.cost_usd,
            },
            "goal": {
                "input_tokens": others.goal.input_tokens,
                "output_tokens": others.goal.output_tokens,
                "cost_usd": others.goal.cost_usd,
            },
        },
        "denials": outcome.denials,
    });
    if let (Some(structured), Some(map)) = (&outcome.structured, fields.as_object_mut()) {
        map.insert("structured_output".into(), structured.clone());
    }
    if let (Some(goal), Some(map)) = (goal, fields.as_object_mut()) {
        map.insert("goal".into(), goal.json());
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
        .roots()
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
            "guardian_profile": runtime.guardian_profile().as_str(),
            "permission_mode": runtime.start_mode().as_str(),
            "workspace_roots": roots,
            "config_sources": sources,
            "workspace_trust": if trusted { "trusted" } else { "untrusted" },
            "suspended_allow_rules": if runtime.start_mode() == PermissionMode::Auto {
                runtime.state.suspended_allow_rules()
            } else {
                Vec::new()
            },
            "tools": runtime.tool_names(),
        }),
    )
}

fn gate_decision_event(session_id: &str, record: &DecisionRecord) -> Value {
    event(
        "gate_decision",
        session_id,
        json!({
            "tool_use_id": record.tool_use_id.clone().unwrap_or_default(),
            "agent_id": record.agent_id,
            "tool": record.tool,
            "tier": record.tier.as_str(),
            "decision": record.decision.as_str(),
            "rule": record.rule,
            "explanation": record.explanation,
            "confidence": record.confidence,
            "guardian_model": record.guardian_model,
            "fail_closed_reason": record.fail_closed_reason,
            "screen_risk": record.screen_risk,
            "screen_allow_risk": record.screen_allow_risk,
            "screen_steering": record.screen_steering,
            "screen_outcome": record.screen_outcome,
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
            SessionEvent::ToolPreview { .. } => {}
            SessionEvent::Usage { usage, calls_tool } => {
                totals.add(&usage);
                // The cap is checked between turns: a spent budget stops the run before its
                // tool calls run, but never discards an answer already complete.
                if calls_tool
                    && let (Some(cap), Some(spent)) =
                        (options.max_budget_usd, spent(runtime, totals))
                    && spent > cap
                {
                    return Flow::Stop;
                }
            }
        }
        Flow::Continue
    };
    let ids = runtime.models.main.wire_ids();
    let (model, provider) = (ids.model, ids.provider);
    let turn = run_turn(
        TurnParams {
            agent: &runtime.agent,
            state: &runtime.state,
            prompt: text,
            max_turns: options.max_turns.unwrap_or(DEFAULT_MAX_TURNS),
            priced: runtime.models.main.catalog_billed(),
            model,
            provider,
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

/// Startup notices: the scanner's, in every mode, then those that matter to a run that can
/// reach the guardian: one stderr line each, and one more when the guardian cannot be asked at
/// all, since every adjudication then fails closed.
fn announce(runtime: &AgentRuntime) {
    for notice in runtime.scanner_notices() {
        eprintln!("fontaine: {notice}");
    }
    if !matches!(
        runtime.start_mode(),
        PermissionMode::Auto | PermissionMode::Plan
    ) {
        return;
    }
    for notice in runtime.notices() {
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

/// What a headless run learns after the main conversation's answer while subagents still run.
enum News {
    /// Reports or messages for the main conversation, as the prompt of a further turn.
    Prompt(String),
    OverBudget,
    /// Nothing is running and nothing is waiting.
    Done,
}

/// Waits until a background subagent has news for the main conversation, the budget is spent, or
/// no subagent is left running.
async fn wait_for_news(runtime: &AgentRuntime, over_cap: &dyn Fn() -> bool) -> News {
    let tree = runtime.state.agents().clone();
    loop {
        let changed = tree.changed().notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if let Some(prompt) = runtime.notification_prompt().await {
            return News::Prompt(prompt);
        }
        if !tree.background_running() {
            // A report is queued before its child is marked finished, so nothing is missed
            // between the check above and this one.
            return match runtime.notification_prompt().await {
                Some(prompt) => News::Prompt(prompt),
                None => News::Done,
            };
        }
        if over_cap() {
            return News::OverBudget;
        }
        tokio::select! {
            _ = &mut changed => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
}

fn over_cap(runtime: &AgentRuntime, options: &Options, totals: &Totals) -> bool {
    matches!(
        (options.max_budget_usd, spent(runtime, totals)),
        (Some(cap), Some(spent)) if spent > cap
    )
}

fn budget_stop() -> Outcome {
    Outcome {
        subtype: Subtype::MaxBudget,
        text: String::new(),
        structured: None,
        error: Some("the run's spend exceeded --max-budget-usd".to_string()),
        started: std::time::Instant::now(),
        denials: Vec::new(),
    }
}

/// What must hold before a paid call starts: a text-only answer that crossed the cap ends its
/// own turn as a success, so the cap is enforced here, and again after the summarizing call
/// that may precede a turn, which is paid too. `Some` is the stop that replaces the call.
async fn stop_before_call(
    runtime: &AgentRuntime,
    options: &Options,
    totals: &mut Totals,
) -> Option<Outcome> {
    if over_cap(runtime, options, totals) {
        return Some(budget_stop());
    }
    let compaction = runtime.compact_if_needed().await;
    if let Some(usage) = compaction.usage() {
        totals.add(usage);
    }
    over_cap(runtime, options, totals).then(budget_stop)
}

/// Appends a further turn's outcome to the message's: its denials join the earlier ones and the
/// message keeps the time it started.
fn merge(earlier: Outcome, mut next: Outcome) -> Outcome {
    next.started = earlier.started;
    next.denials = earlier.denials.into_iter().chain(next.denials).collect();
    next
}

/// One turn, started once `stop_before_call` finds nothing to stop it.
async fn guarded_turn(
    runtime: &AgentRuntime,
    options: &Options,
    totals: &mut Totals,
    stream_events: bool,
    prompt: String,
) -> Outcome {
    if let Some(stop) = stop_before_call(runtime, options, totals).await {
        runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
        return stop;
    }
    let outcome = run_one(runtime, options, prompt, totals, stream_events).await;
    runtime.goal_turn_ended(Some(&goal::turn_end(&outcome)));
    outcome
}

/// Background subagents still running when the answer comes are waited for: their reports
/// reach the main conversation as notifications, each a turn of its own, until none is running.
/// A run stopped by a limit kills them and drops their reports.
async fn settle(
    runtime: &AgentRuntime,
    options: &Options,
    totals: &mut Totals,
    stream_events: bool,
    mut outcome: Outcome,
) -> Outcome {
    while outcome.subtype == Subtype::Success {
        match wait_for_news(runtime, &|| over_cap(runtime, options, totals)).await {
            News::Prompt(prompt) => {
                let next = run_one(runtime, options, prompt, totals, stream_events).await;
                runtime.goal_turn_ended(Some(&goal::turn_end(&next)));
                outcome = merge(outcome, next);
            }
            News::OverBudget => {
                let stop = budget_stop();
                runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
                outcome = merge(outcome, stop);
            }
            News::Done => break,
        }
    }
    outcome
}

/// What a message came to.
struct Handled {
    outcome: Outcome,
    goal: Option<goal::GoalReport>,
    /// The message ran turns under a goal, so how the goal ended says how the run did.
    goal_ran: bool,
}

/// One user message. `/goal <condition>` keeps running turns after the first until the
/// evaluator finds the condition met, or the goal ends or pauses.
async fn handle_message(
    runtime: &AgentRuntime,
    options: &Options,
    totals: &mut Totals,
    stream_events: bool,
    text: String,
) -> Handled {
    let session_id = runtime.session.id.clone();
    let mut involved = runtime.session.goal().is_active();
    let prompt = match goal::command(&text) {
        None => {
            runtime.append(Record::UserMessage { text: text.clone() });
            text
        }
        Some(GoalArg::Status) => {
            let (outcome, goal) = goal::status_outcome(runtime);
            return Handled {
                outcome,
                goal,
                goal_ran: false,
            };
        }
        Some(GoalArg::Clear) => {
            let (outcome, goal) = goal::clear_outcome(runtime);
            return Handled {
                outcome,
                goal,
                goal_ran: false,
            };
        }
        Some(GoalArg::Set(condition)) => {
            if let Err(e) = runtime.set_goal(&condition) {
                let outcome = Outcome {
                    subtype: Subtype::Execution,
                    text: String::new(),
                    structured: None,
                    error: Some(fontaine_tools::error_chain::chain(&e)),
                    started: std::time::Instant::now(),
                    denials: Vec::new(),
                };
                return Handled {
                    outcome,
                    goal: None,
                    goal_ran: false,
                };
            }
            involved = true;
            runtime.append(Record::UserMessage { text });
            first_turn_prompt(&condition)
        }
    };
    runtime.goal_user_prompt();
    let outcome = guarded_turn(runtime, options, totals, stream_events, prompt).await;
    let mut outcome = settle(runtime, options, totals, stream_events, outcome).await;
    let mut ended = None;
    while involved && outcome.subtype == Subtype::Success {
        let Some(pending) = runtime.next_goal_step() else {
            break;
        };
        if let Some(stop) = stop_before_call(runtime, options, totals).await {
            runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
            outcome = merge(outcome, stop);
            break;
        }
        let Some(applied) = runtime.evaluate_goal(&pending).await else {
            break;
        };
        if stream_events {
            emit_line(&goal::evaluation_event(
                &session_id,
                applied.judgment,
                &applied.reason,
                applied.evaluations,
            ));
        }
        match applied.step {
            Step::Continue(next) => {
                let prompt = runtime.goal_prompt(&next).await;
                let turn = guarded_turn(runtime, options, totals, stream_events, prompt).await;
                outcome = settle(
                    runtime,
                    options,
                    totals,
                    stream_events,
                    merge(outcome, turn),
                )
                .await;
            }
            Step::Done { event, summary } => {
                ended = Some(goal::Ended {
                    event,
                    summary,
                    reason: applied.reason,
                });
                break;
            }
            Step::Pause(_) => break,
        }
    }
    let goal = if involved {
        goal::report(runtime, ended)
    } else {
        None
    };
    Handled {
        outcome,
        goal,
        goal_ran: involved,
    }
}

/// Runs the headless session and returns the process exit code: 0 completed, 1 run error or a
/// `/goal` that did not end met, 2 usage error.
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
    let mut last: Option<Handled> = None;
    let mut goal_failure: Option<String> = None;
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
        let handled = handle_message(&runtime, &options, &mut totals, stream_events, text).await;
        let outcome = &handled.outcome;
        let failed = outcome.subtype != Subtype::Success;
        // A goal that ended impossible or paused did not finish the task, even when the turn
        // itself answered. A failed turn already says why.
        goal_failure = handled
            .goal
            .as_ref()
            .and_then(goal::GoalReport::failure)
            .filter(|_| handled.goal_ran && !failed);
        if failed {
            runtime.state.agents().kill_all();
        }
        if stream_events {
            emit_line(&result_event(
                &session_id,
                outcome,
                &totals,
                &Others::of(&runtime),
                handled.goal.as_ref(),
            ));
        }
        last = Some(handled);
        if failed || goal_failure.is_some() {
            code = 1;
            break;
        }
    }
    runtime.state.agents().kill_all();

    let Some(last) = last else {
        eprintln!("fontaine: no prompt: the input held no user message");
        return 2;
    };
    match options.output_format {
        OutputFormat::StreamJson => {}
        OutputFormat::Json => emit_line(&result_event(
            &session_id,
            &last.outcome,
            &totals,
            &Others::of(&runtime),
            last.goal.as_ref(),
        )),
        OutputFormat::Text => {
            if let Some(failure) = &goal_failure {
                eprintln!("fontaine: {failure}");
            } else if let Some(error) = &last.outcome.error {
                eprintln!("fontaine: {error}");
            } else {
                let _ = writeln!(std::io::stdout().lock(), "{}", last.outcome.text);
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::{Others, Outcome, Subtype, Totals, result_event, session_cost};
    use fontaine_core::gate::GuardianUsage;

    #[test]
    fn session_cost_adds_the_guardian_and_subagents_and_is_null_only_when_none_is_priced() {
        assert_eq!(session_cost(None, None, None, None), None);
        assert_eq!(session_cost(Some(0.5), None, None, None), Some(0.5));
        assert_eq!(session_cost(None, Some(0.25), None, None), Some(0.25));
        assert_eq!(session_cost(Some(0.5), Some(0.25), None, None), Some(0.75));
        assert_eq!(session_cost(None, None, None, Some(0.125)), Some(0.125));
        assert_eq!(session_cost(None, None, Some(0.0625), None), Some(0.0625));
        assert_eq!(
            session_cost(Some(0.5), Some(0.25), Some(0.0625), Some(0.125)),
            Some(0.9375)
        );
    }

    #[test]
    fn cache_tokens_reach_the_result_usage() {
        let mut totals = Totals::default();
        for (read, written) in [(Some(800), Some(100)), (Some(900), None), (None, None)] {
            let mut usage = rig_core::completion::Usage::new();
            usage.input_tokens = Some(1000);
            usage.cached_input_tokens = read;
            usage.cache_creation_input_tokens = written;
            totals.add(&usage);
        }
        let outcome = Outcome {
            subtype: Subtype::Success,
            text: String::new(),
            structured: None,
            error: None,
            started: std::time::Instant::now(),
            denials: Vec::new(),
        };
        let none = || GuardianUsage::default();
        let others = Others {
            guardian: none(),
            goal: none(),
            subagents: none(),
        };
        let result = result_event("s", &outcome, &totals, &others, None);
        assert_eq!(result["usage"]["main"]["input_tokens"], 3000);
        assert_eq!(result["usage"]["main"]["cache_read_input_tokens"], 1700);
        assert_eq!(result["usage"]["main"]["cache_creation_input_tokens"], 100);
    }
}
