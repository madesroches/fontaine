//! The non-interactive surface, `fontaine -p`. Its output is a versioned contract: see
//! `docs/headless.md` and `docs/schemas/headless-v1.schema.json`.

mod afk;
mod goal;

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::Path;

use fontaine_core::RuntimeError;
use fontaine_core::afk_report::{AfkOutcome, AfkReport, AfkSource};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::{Converted, Decision, DecisionRecord};
use fontaine_core::config::{OnUsageLimit, PermissionMode};
use fontaine_core::gate::UsageWait;
use fontaine_core::goal::{GoalArg, Step, first_turn_prompt};
use fontaine_core::ledger::Record;
use fontaine_core::model_roles::Tried;
use fontaine_core::providers::ExternalLogin;
use fontaine_core::quota::{AccountUsage, Quota, QuotaSource, UsageRole};
use fontaine_core::trust;
use fontaine_core::turn::{
    Flow, RETRY_NOTE, SessionEvent, TurnFailure, TurnOutcome, TurnParams, run_turn, user_message,
    wait_out_usage_limit,
};
use fontaine_core::turn_recovery::{Fallback, Unsettled};
use fontaine_core::usage_ledger::{Bucket, UsageSource};
use fontaine_core::usage_pause::{PauseEnd, PauseEvent, PauseInfo};
use fontaine_core::user_hooks::PromptGate;
use fontaine_tools::kills::KILL_SETTLE;
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
    /// Overrides `[headless] on_usage_limit`.
    pub on_usage_limit: Option<OnUsageLimit>,
    /// Overrides `[headless] max_usage_wait_secs`.
    pub max_usage_wait: Option<std::time::Duration>,
    pub json_schema: Option<Schema>,
}

/// The exit code of a run that ended because an account's usage window stayed spent.
pub const EXIT_USAGE_LIMIT: u8 = 3;

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
    /// Whether no further message can follow. A stream of lines cannot say before it ends.
    fn is_exhausted(&self) -> bool {
        matches!(self, Input::Single(None))
    }

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

/// What the main model has cost, with the summarizer that keeps its context in bounds and the
/// advisor it consults.
fn main_usage(runtime: &AgentRuntime) -> Bucket {
    let ledger = runtime.state.usage_ledger();
    let mut main = ledger.bucket(UsageSource::Main);
    main.merge(&ledger.bucket(UsageSource::Compaction));
    main.merge(&ledger.bucket(UsageSource::Advisor));
    main
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Subtype {
    Success,
    MaxTurns,
    MaxBudget,
    Execution,
    UsageLimit,
}

impl Subtype {
    fn name(self) -> &'static str {
        match self {
            Subtype::Success => "success",
            Subtype::MaxTurns => "error_max_turns",
            Subtype::MaxBudget => "error_max_budget_usd",
            Subtype::Execution => "error_during_execution",
            Subtype::UsageLimit => "error_usage_limit",
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
fn spent(runtime: &AgentRuntime) -> Option<f64> {
    session_cost(
        main_usage(runtime).cost_usd,
        runtime.state.guardian_usage().cost_usd,
        runtime.state.goal_usage().cost_usd,
        runtime.state.subagent_usage().cost_usd,
    )
}

/// What each model of the session has cost.
struct Usage {
    main: Bucket,
    guardian: Bucket,
    goal: Bucket,
    subagents: Bucket,
}

impl Usage {
    fn of(runtime: &AgentRuntime) -> Usage {
        Usage {
            main: main_usage(runtime),
            guardian: runtime.state.guardian_usage(),
            goal: runtime.state.goal_usage(),
            subagents: runtime.state.subagent_usage(),
        }
    }
}

/// The usage-window state of the main and guardian accounts, fetched once because a headless run
/// has no draw loop to poll for. Empty when neither account has a quota source.
async fn account_usage(runtime: &AgentRuntime, warned: &mut HashSet<String>) -> Vec<Value> {
    let main = &runtime.models.main;
    let guardian = runtime
        .guardian_quota()
        .filter(|(account, _)| *account != main.reference.account);
    let (main_quota, guardian_quota) = tokio::join!(
        fetch_quota(main.quota.as_ref()),
        fetch_quota(guardian.as_ref().map(|(_, source)| source)),
    );
    let accounts = [
        (
            UsageRole::Main,
            main.reference.account.as_str(),
            main.quota.as_ref(),
            main_quota,
        ),
        (
            UsageRole::Guardian,
            guardian.as_ref().map_or("", |(account, _)| account),
            guardian.as_ref().map(|(_, source)| source),
            guardian_quota,
        ),
    ];
    let mut entries = Vec::new();
    for (role, account, source, fetched) in accounts {
        let (Some(source), Some(fetched)) = (source, fetched) else {
            continue;
        };
        let quota = match fetched {
            Ok(quota) => Some(quota),
            Err(e) => {
                if warned.insert(account.to_string()) {
                    runtime.approver().warn(&format!(
                        "could not read the {} quota: {}",
                        source.name(),
                        fontaine_tools::error_chain::chain(&e)
                    ));
                }
                None
            }
        };
        let usage = AccountUsage {
            role,
            account: account.to_string(),
            label: source.name().to_string(),
            quota,
        };
        entries.push(usage.json());
    }
    entries
}

async fn fetch_quota(
    source: Option<&std::sync::Arc<dyn QuotaSource>>,
) -> Option<Result<Quota, RuntimeError>> {
    Some(source?.fetch().await)
}

fn result_event(
    session_id: &str,
    outcome: &Outcome,
    usage: &Usage,
    goal: Option<&goal::GoalReport>,
    account_usage: &[Value],
    afk: Option<&AfkReport>,
    usage_limit: Option<&Value>,
) -> Value {
    let (totals, guardian, subagents) = (&usage.main, &usage.guardian, &usage.subagents);
    let mut fields = json!({
        "subtype": outcome.subtype.name(),
        "is_error": outcome.subtype != Subtype::Success,
        "result": outcome.text,
        "error": outcome.error,
        "num_turns": totals.calls,
        "duration_ms": outcome.started.elapsed().as_millis() as u64,
        "total_cost_usd": session_cost(
            totals.cost_usd,
            guardian.cost_usd,
            usage.goal.cost_usd,
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
                "cache_read_input_tokens": guardian.cache_read_input_tokens,
                "cache_creation_input_tokens": guardian.cache_creation_input_tokens,
                "cost_usd": guardian.cost_usd,
            },
            "subagents": {
                "input_tokens": subagents.input_tokens,
                "output_tokens": subagents.output_tokens,
                "cache_read_input_tokens": subagents.cache_read_input_tokens,
                "cache_creation_input_tokens": subagents.cache_creation_input_tokens,
                "cost_usd": subagents.cost_usd,
            },
            "goal": {
                "input_tokens": usage.goal.input_tokens,
                "output_tokens": usage.goal.output_tokens,
                "cache_read_input_tokens": usage.goal.cache_read_input_tokens,
                "cache_creation_input_tokens": usage.goal.cache_creation_input_tokens,
                "cost_usd": usage.goal.cost_usd,
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
    if let (Some(report), Some(map)) = (afk, fields.as_object_mut()) {
        map.insert("afk".into(), afk::result_object(report));
    }
    if let (false, Some(map)) = (account_usage.is_empty(), fields.as_object_mut()) {
        map.insert("account_usage".into(), account_usage.to_vec().into());
    }
    if let (Some(limit), Some(map)) = (usage_limit, fields.as_object_mut()) {
        map.insert("usage_limit".into(), limit.clone());
    }
    event("result", session_id, fields)
}

fn epoch_secs(at: Option<std::time::SystemTime>) -> Option<u64> {
    at.and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// The account and reset time of a usage window that stayed spent, as `result.usage_limit`.
fn usage_limit_object(end: PauseEnd, info: &PauseInfo) -> Value {
    json!({
        "account": info.account,
        "role": info.role.as_str(),
        "reset_at": epoch_secs(info.reset_at),
        "end": end.as_str(),
    })
}

fn usage_event(session_id: &str, pause: &PauseEvent) -> Value {
    match pause {
        PauseEvent::Paused(info) => event(
            "usage_paused",
            session_id,
            json!({
                "account": info.account,
                "role": info.role.as_str(),
                "reset_at": epoch_secs(info.reset_at),
            }),
        ),
        PauseEvent::Resumed { end, info, waited } => event(
            "usage_resumed",
            session_id,
            json!({
                "account": info.account,
                "role": info.role.as_str(),
                "end": end.as_str(),
                "waited_secs": waited.as_secs(),
            }),
        ),
    }
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
            "project_executables": runtime.project_executables(),
            "suspended_allow_rules": if runtime.start_mode().rules() == PermissionMode::Auto {
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
            "agent_name": record.agent_name,
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
            "screen_scores": record.screen_scores,
            "screen_outcome": record.screen_outcome,
            "afk": record.afk,
            "converted": record.converted.map(Converted::as_str),
        }),
    )
}

/// Puts the notes hooks left for the model ahead of `text`.
fn with_hook_context(runtime: &AgentRuntime, text: String) -> String {
    let notes = runtime.take_hook_context();
    if notes.is_empty() {
        return text;
    }
    format!("{}\n\n{text}", notes.join("\n\n"))
}

/// One user message: runs the agent to its answer, a limit or a failure. A model that ran out of
/// quota hands the run to the next entry of its list, and the message carries on there. The note
/// is what a `stop` hook asks the run to continue on, when it blocked the stop of a successful
/// run.
async fn run_one(
    runtime: &mut AgentRuntime,
    options: &Options,
    text: String,
    stream_events: bool,
) -> (Outcome, Option<String>) {
    let mut fell = None;
    let mut tried = Tried::default();
    let (mut outcome, mut note) =
        run_attempt(runtime, options, text, stream_events, &mut fell).await;
    while let Some(failure) = fell.take() {
        if runtime
            .fall_through(failure.class.unwrap_or("quota"), &mut tried)
            .await
            .is_none()
        {
            // Nothing left to try on a spent usage window: the whole run waits for it and the
            // request goes again, or ends when the wait does.
            let mut failure = failure;
            let had_limit = failure.usage_limit.is_some();
            let main = &runtime.models.main;
            if wait_out_usage_limit(
                &runtime.state,
                &mut failure,
                UsageRole::Main,
                &main.reference.account,
                main.quota.clone(),
            )
            .await
            {
                tried = Tried::default();
                let (next, next_note) = run_attempt(
                    runtime,
                    options,
                    RETRY_NOTE.to_string(),
                    stream_events,
                    &mut fell,
                )
                .await;
                outcome = merge(outcome, next);
                note = next_note;
                continue;
            }
            if had_limit && failure.usage_limit.is_none() {
                outcome.error = Some(failure.message.clone());
            }
            if let (Some(spent), Some(error)) = (runtime.fall_through_spent(), &mut outcome.error) {
                error.push('\n');
                error.push_str(&spent);
            }
            runtime.turn_ended(&TurnOutcome::Failed(failure)).await;
            break;
        }
        let (next, next_note) = run_attempt(
            runtime,
            options,
            RETRY_NOTE.to_string(),
            stream_events,
            &mut fell,
        )
        .await;
        outcome = merge(outcome, next);
        note = next_note;
    }
    (outcome, note)
}

/// One run of the agent on `text`. A failure that is the model's quota running out is left in
/// `fell` for the caller to answer.
async fn run_attempt(
    runtime: &AgentRuntime,
    options: &Options,
    text: String,
    stream_events: bool,
    fell: &mut Option<TurnFailure>,
) -> (Outcome, Option<String>) {
    // After the compaction that may have preceded this run, so the notes it queued are in.
    let text = with_hook_context(runtime, text);
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
            // Not in the closed stream-json schema; text output prints no reasoning either.
            SessionEvent::ReasoningDelta(_) | SessionEvent::Reasoning(_) => {}
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
                emit(event(
                    "model_call",
                    sid,
                    json!({
                        "input_tokens": usage.input_tokens.unwrap_or(0),
                        "output_tokens": usage.output_tokens.unwrap_or(0),
                        "cache_read_input_tokens": usage.cached_input_tokens.unwrap_or(0),
                        "cache_creation_input_tokens": usage.cache_creation_input_tokens.unwrap_or(0),
                        "calls_tool": calls_tool,
                    }),
                ));
                // The cap is checked between turns: a spent budget stops the run before its
                // tool calls run, but never discards an answer already complete.
                if calls_tool
                    && let (Some(cap), Some(spent)) = (options.max_budget_usd, spent(runtime))
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
    let mut stops = runtime.state.afk_stop_signal();
    let modalities = main_modalities(runtime);
    // The run's durable record: a stop that drops the run, or a kill before the first
    // model call, leaves the prompt for the fold that keeps the turn.
    let sent = user_message(&text, &[], &modalities);
    let len = match runtime.conversation_len().await {
        Ok(len) => len,
        Err(e) => {
            // The preflight wrote the record before the compaction; the refusal must not
            // leave it for a fold to read as this turn's fallback.
            runtime.clear_run_start().await;
            return (count_stop(&e), None);
        }
    };
    if let Err(e) = runtime.rewrite_run_start(sent.clone(), len).await {
        // The turn must not run unrecorded: the run that follows leaves a window in
        // which a kill would lose the prompt. The rewrite already cleared the record
        // the preflight wrote, so the refusal cannot resurrect the turn as a fold.
        return (record_stop(&e), None);
    }
    // What a stopped run is kept as when it committed nothing.
    let fallback = Fallback {
        prompt: sent,
        log_len_before: len,
    };
    let turn = run_turn(
        TurnParams {
            agent: &runtime.agent,
            state: &runtime.state,
            prompt: text,
            attachments: Vec::new(),
            modalities,
            max_turns: options.max_turns,
            priced: runtime.models.main.catalog_billed(),
            model,
            provider,
            native_search: runtime.native_search_active(),
            generation_options: runtime.turn_generation_options(),
            max_tokens: runtime.turn_max_tokens(),
            served: runtime.served(),
        },
        &mut on_event,
    );
    // The stop comes first: a stopped session gets no further tool-capable model turn.
    let turn = tokio::select! {
        biased;
        Ok(()) = stops.changed() => None,
        outcome = turn => Some(outcome),
    };
    // A stop cuts the turn short: the run is dropped when the signal wins the poll, and when
    // the run wins it the stop has denied its next dispatch as a Cancelled error the recovery
    // hook kept. An answer that landed in the same poll settled and is kept as it is.
    let stopped = runtime.state.afk().stopped.is_some();
    if turn.is_none() || (stopped && matches!(turn, Some(TurnOutcome::Failed(_)))) {
        runtime
            .keep_unsettled_turn(Unsettled::AfkStop, Some(fallback))
            .await;
    }
    // A stop that lands inside the poll that also finishes the turn is still a stop.
    let turn = turn.filter(|_| !stopped);
    let Some(turn) = turn else {
        runtime.handle_afk_stop().await;
        return (afk::stopped_outcome(started, denials), None);
    };

    // A failure the next entry of a model list may answer is reported to the hooks only once the
    // list is spent, by the caller.
    let note = match &turn {
        TurnOutcome::Failed(failure) if failure.falls_through => {
            *fell = Some(failure.clone());
            None
        }
        _ => runtime.turn_ended(&turn).await,
    };
    let (subtype, text, error) = match turn {
        TurnOutcome::Answer(text) => (Subtype::Success, text, None),
        TurnOutcome::Stopped => (
            Subtype::MaxBudget,
            String::new(),
            Some("the run's spend exceeded --max-budget-usd".to_string()),
        ),
        TurnOutcome::MaxTurns(message) => (Subtype::MaxTurns, String::new(), Some(message)),
        TurnOutcome::Failed(failure) => (Subtype::Execution, String::new(), Some(failure.message)),
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
    let note = note.filter(|_| outcome.subtype == Subtype::Success);
    (outcome, note)
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
        PermissionMode::Auto | PermissionMode::Afk | PermissionMode::Plan
    ) {
        // The guardian is never consulted here, but a subagent on its model stays quiet in
        // expectation of this notice, so it is printed even so.
        if let Some(notice) = &runtime.models.main.facts_notice {
            eprintln!("fontaine: {notice}");
        }
        if let Some(fontaine_core::models::GuardianModel::Ready(handle)) = &runtime.models.guardian
            && handle.reference != runtime.models.main.reference
            && let Some(notice) = &handle.facts_notice
        {
            eprintln!("fontaine: {notice}");
        }
        return;
    }
    for notice in runtime.notices() {
        eprintln!("fontaine: {notice}");
    }
    let guardian = runtime.state.guardian();
    if runtime.models.guardian.is_some()
        && let Some(reason) = guardian.unavailable_reason()
    {
        let label = match &runtime.models.guardian {
            Some(fontaine_core::models::GuardianModel::Unavailable {
                not_signed_in: true,
                ..
            }) => "GuardianNotSignedIn",
            _ => "GuardianUnavailable",
        };
        eprintln!(
            "fontaine: the guardian model {} is unavailable ({reason}); every call that needs it is denied with {label}",
            guardian.label()
        );
    }
}

/// What a headless run learns after the main conversation's answer while subagents still run.
enum News {
    /// Reports or messages for the main conversation, as the prompt of a further turn.
    Prompt(String),
    OverBudget,
    /// The AFK temperature ceiling stopped the session.
    Stopped,
    /// Nothing is running and nothing is waiting.
    Done,
}

/// Waits until a subagent has news for the main conversation, the budget is spent, or
/// no subagent is left running.
async fn wait_for_news(runtime: &AgentRuntime, over_cap: &dyn Fn() -> bool) -> News {
    let tree = runtime.state.agents().clone();
    let mut stops = runtime.state.afk_stop_signal();
    loop {
        let changed = tree.changed().notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if runtime.state.afk().stopped.is_some() {
            return News::Stopped;
        }
        // Blocked: no notification turn starts. The children's reports are dropped with the
        // run, which kills what is left.
        if runtime.unattended_blocked().is_some() {
            return News::Done;
        }
        if let Some(prompt) = runtime.notification_prompt().await {
            return News::Prompt(prompt);
        }
        if !tree.any_running() {
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
            Ok(()) = stops.changed() => return News::Stopped,
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
}

fn over_cap(runtime: &AgentRuntime, options: &Options) -> bool {
    matches!(
        (options.max_budget_usd, spent(runtime)),
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

/// The outcome of a turn refused before it runs: its durable record could not be
/// written, and an unrecorded turn must not start.
fn record_stop(e: &(dyn std::error::Error + 'static)) -> Outcome {
    preflight_stop("the run's pending record could not be written", e)
}

/// The outcome of a turn refused before it runs: the conversation's length could not
/// be read, and a fallback counted on a made-up length would misplace the fold that
/// keeps a stopped run.
fn count_stop(e: &(dyn std::error::Error + 'static)) -> Outcome {
    preflight_stop(
        "the conversation's length could not be read, so the turn did not run",
        e,
    )
}

fn preflight_stop(detail: &str, e: &(dyn std::error::Error + 'static)) -> Outcome {
    Outcome {
        subtype: Subtype::Execution,
        text: String::new(),
        structured: None,
        error: Some(format!(
            "{detail}: {}",
            fontaine_tools::error_chain::chain(e)
        )),
        started: std::time::Instant::now(),
        denials: Vec::new(),
    }
}

/// What must hold before a paid call starts: a text-only answer that crossed the cap ends its
/// own turn as a success, so the cap is enforced here, and again after the summarizing call
/// that may precede a turn, which is paid too. `turn` is the prompt about to run, when a turn
/// follows: its run is recorded before the compaction that precedes it, so a process that
/// dies during compaction still leaves the prompt for the fold that resumes the session.
/// `Some` is the stop that replaces the call.
async fn stop_before_call(
    runtime: &AgentRuntime,
    options: &Options,
    turn: Option<&str>,
) -> Option<Outcome> {
    if over_cap(runtime, options) {
        return Some(budget_stop());
    }
    // A leftover journal's fold checks its mark against `main`'s message count; a compaction
    // that ran first would change the count and the fold would append the landed turn again.
    // The fold goes before the compaction, as the TUI's pre-turn fold does.
    runtime
        .keep_unsettled_turn(Unsettled::SessionEnded, None)
        .await;
    if let Some(text) = turn {
        let len = match runtime.conversation_len().await {
            Ok(len) => len,
            // A fallback counted on a made-up length misplaces the fold a stop owes the
            // run, so the turn is refused rather than risk duplicating its prompt.
            Err(e) => return Some(count_stop(&e)),
        };
        if let Err(e) = runtime
            .record_run_start(user_message(text, &[], &main_modalities(runtime)), len)
            .await
        {
            // The turn must not run unrecorded: the summarizing call and the turn that
            // follow leave a long window in which a kill would lose the prompt.
            return Some(record_stop(&e));
        }
    }
    runtime.compact_if_needed().await;
    if over_cap(runtime, options) {
        // The summarizing call crossed the cap: the turn never runs, and its record must
        // not resurrect it as a folded turn.
        if turn.is_some() {
            runtime.clear_run_start().await;
        }
        return Some(budget_stop());
    }
    None
}

/// The main model's input modalities, for the user message a turn sends.
fn main_modalities(runtime: &AgentRuntime) -> rig_core::catalog::Modalities {
    runtime
        .models
        .main
        .completion
        .spec()
        .map_or_else(rig_core::catalog::Modalities::default, |spec| spec.input)
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
    runtime: &mut AgentRuntime,
    options: &Options,
    stream_events: bool,
    prompt: String,
) -> Outcome {
    if let Some(stop) = stop_before_call(runtime, options, Some(&prompt)).await {
        runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
        return stop;
    }
    let (outcome, note) = run_one(runtime, options, prompt, stream_events).await;
    let outcome = continue_on_stop(runtime, options, stream_events, outcome, note).await;
    runtime.goal_turn_ended(Some(&goal::turn_end(&outcome)));
    outcome
}

/// Runs further turns while a `stop` hook keeps blocking the end of the run, each through the
/// budget check and compaction a turn of its own gets.
async fn continue_on_stop(
    runtime: &mut AgentRuntime,
    options: &Options,
    stream_events: bool,
    mut outcome: Outcome,
    mut note: Option<String>,
) -> Outcome {
    while let Some(prompt) = note.take() {
        if let Some(stop) = stop_before_call(runtime, options, Some(&prompt)).await {
            return merge(outcome, stop);
        }
        let (next, again) = run_one(runtime, options, prompt, stream_events).await;
        outcome = merge(outcome, next);
        note = again;
    }
    outcome
}

/// Subagents still running when the answer comes are waited for: their reports
/// reach the main conversation as notifications, each a turn of its own, until none is running.
/// A run stopped by a limit kills them and drops their reports.
async fn settle(
    runtime: &mut AgentRuntime,
    options: &Options,
    stream_events: bool,
    mut outcome: Outcome,
) -> Outcome {
    while outcome.subtype == Subtype::Success {
        match wait_for_news(runtime, &|| over_cap(runtime, options)).await {
            News::Prompt(prompt) => {
                // A turn an earlier fold failed to keep goes before this one; the record
                // run_one writes must not be read as this turn's fallback first.
                runtime
                    .keep_unsettled_turn(Unsettled::SessionEnded, None)
                    .await;
                let (next, note) = run_one(runtime, options, prompt, stream_events).await;
                let next = continue_on_stop(runtime, options, stream_events, next, note).await;
                runtime.goal_turn_ended(Some(&goal::turn_end(&next)));
                outcome = merge(outcome, next);
            }
            News::OverBudget => {
                let stop = budget_stop();
                runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
                outcome = merge(outcome, stop);
            }
            News::Stopped => {
                runtime.handle_afk_stop().await;
                let stop = afk::stopped_outcome(std::time::Instant::now(), Vec::new());
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
    /// The AFK report, while AFK is on or a stop stands.
    afk: Option<AfkReport>,
}

/// One user message. `/goal <condition>` keeps running turns after the first until the
/// evaluator finds the condition met, or the goal ends or pauses.
async fn handle_message(
    runtime: &mut AgentRuntime,
    options: &Options,
    stream_events: bool,
    text: String,
) -> Handled {
    let session_id = runtime.session.id.clone();
    if runtime.state.afk().stopped.is_some() {
        // A stopped session denies every call; no model request is made on its behalf.
        return Handled {
            outcome: afk::stopped_outcome(std::time::Instant::now(), Vec::new()),
            goal: None,
            goal_ran: false,
            afk: afk_report(runtime),
        };
    }
    let mut involved = runtime.session.goal().is_active();
    let command = goal::command(&text);
    if matches!(command, None | Some(GoalArg::Set(_)))
        && let PromptGate::Blocked(message) = runtime.prompt_submit(&text).await
    {
        // The hook said no: nothing is sent, recorded or set, and the run ends as a success
        // that says why, as Claude Code's does.
        return Handled {
            outcome: Outcome {
                subtype: Subtype::Success,
                text: format!("{message}\n\n\nOriginal prompt: {text}"),
                structured: None,
                error: None,
                started: std::time::Instant::now(),
                denials: Vec::new(),
            },
            goal: None,
            goal_ran: false,
            afk: afk_report(runtime),
        };
    }
    let prompt = match command {
        None => {
            // A turn a resume-time fold failed to keep belongs before this one: the
            // checkpoint `begin_turn` records must hold the recovered history, or a
            // rewind to it drops the recovered turn.
            runtime
                .keep_unsettled_turn(Unsettled::SessionEnded, None)
                .await;
            runtime.begin_turn(&text, false);
            runtime.append(Record::UserMessage {
                text: text.clone(),
                attachments: Vec::new(),
                agent: None,
            });
            text
        }
        Some(GoalArg::Status) => {
            let (outcome, goal) = goal::status_outcome(runtime);
            return Handled {
                outcome,
                goal,
                goal_ran: false,
                afk: afk_report(runtime),
            };
        }
        Some(GoalArg::Clear) => {
            let (outcome, goal) = goal::clear_outcome(runtime);
            return Handled {
                outcome,
                goal,
                goal_ran: false,
                afk: afk_report(runtime),
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
                    afk: afk_report(runtime),
                };
            }
            involved = true;
            // As the plain-message branch: the fold goes before the checkpoint this
            // turn's rewind would restore to.
            runtime
                .keep_unsettled_turn(Unsettled::SessionEnded, None)
                .await;
            runtime.begin_turn(&text, false);
            runtime.append(Record::UserMessage {
                text,
                attachments: Vec::new(),
                agent: None,
            });
            first_turn_prompt(&condition)
        }
    };
    runtime.goal_user_prompt();
    let outcome = guarded_turn(runtime, options, stream_events, prompt).await;
    let mut outcome = settle(runtime, options, stream_events, outcome).await;
    let mut ended = None;
    while involved && outcome.subtype == Subtype::Success {
        // Blocked or stopped: no goal turn starts. The goal is paused with the reason below.
        if runtime.unattended_blocked().is_some() {
            break;
        }
        let Some(pending) = runtime.next_goal_step() else {
            break;
        };
        if let Some(stop) = stop_before_call(runtime, options, None).await {
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
                let turn = guarded_turn(runtime, options, stream_events, prompt).await;
                outcome = settle(runtime, options, stream_events, merge(outcome, turn)).await;
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
    // The first time the breaker blocks an AFK run, the goal is paused and the stretch reviewed.
    let blocked = runtime.note_blocked().await;
    let goal = if involved {
        goal::report(runtime, ended)
    } else {
        None
    };
    Handled {
        outcome,
        goal,
        goal_ran: involved,
        afk: match blocked {
            Some(report) => Some(report),
            None => afk_report(runtime),
        },
    }
}

/// The report of the AFK stretch, while AFK is on or a stop stands, built from the ledger alone.
/// The paid evasion review runs once, when the run ends.
fn afk_report(runtime: &AgentRuntime) -> Option<AfkReport> {
    let stopped = runtime.state.afk().stopped.is_some();
    if !runtime.state.afk_on() && !stopped {
        return None;
    }
    match runtime.afk_reports(false) {
        Ok(mut reports) => reports.pop().map(ended),
        Err(e) => {
            runtime
                .approver()
                .warn(&fontaine_tools::error_chain::chain(&e));
            None
        }
    }
}

/// The run is the stretch, so it has ended by the time a consumer reads its result.
fn ended(mut report: AfkReport) -> AfkReport {
    if report.outcome == AfkOutcome::Active {
        report.outcome = AfkOutcome::Ended;
    }
    report
}

/// The report of a stretch still running as the run ends, after its evasion review. A stretch
/// stopped or blocked was reviewed when it ended, and one that is over needs no review.
async fn final_afk_report(runtime: &AgentRuntime) -> Option<AfkReport> {
    if !runtime.state.afk_on()
        || runtime.state.afk().stopped.is_some()
        || runtime.unattended_blocked().is_some()
    {
        return None;
    }
    runtime.finish_afk().await.map(ended)
}

/// Runs the headless session and returns the process exit code: 0 completed, 1 run error or a
/// `/goal` that did not end met, 2 usage error, 3 an account's usage window stayed spent.
///
/// `guardian_login` is the pinned guardian's account and the login that signs it in, when that
/// account has nobody signed in: a run that consults the guardian then stops before it starts,
/// since every call would be denied.
pub async fn run(
    options: Options,
    mut runtime: AgentRuntime,
    data_dir: &Path,
    guardian_login: Option<(String, ExternalLogin)>,
) -> u8 {
    let code = run_session(options, &mut runtime, data_dir, guardian_login).await;
    // One exit point, so an input error ends the session for the hooks too.
    runtime.fire_session_end("other").await;
    code
}

async fn run_session(
    mut options: Options,
    runtime: &mut AgentRuntime,
    data_dir: &Path,
    guardian_login: Option<(String, ExternalLogin)>,
) -> u8 {
    let mut input = match open_input(&mut options).await {
        Ok(input) => input,
        Err(message) => {
            eprintln!("fontaine: {message}");
            return 2;
        }
    };
    if let Some((account, login)) = guardian_login
        && matches!(
            runtime.start_mode(),
            PermissionMode::Auto | PermissionMode::Afk | PermissionMode::Plan
        )
    {
        eprintln!(
            "fontaine: the guardian's account `{account}` is not signed in; run `{}`, then retry",
            login.display
        );
        return 1;
    }

    let session_id = runtime.session.id.clone();
    runtime.record_session_start().await;
    announce(runtime);
    let stream_events = options.output_format == OutputFormat::StreamJson;
    if stream_events {
        emit_line(&init_event(runtime, data_dir));
    }
    let headless = &runtime.config.headless;
    runtime.state.set_usage_wait(UsageWait {
        cap: Some(
            options
                .max_usage_wait
                .unwrap_or(std::time::Duration::from_secs(headless.max_usage_wait_secs)),
        ),
        afk_cap: None,
        exit: options.on_usage_limit.unwrap_or(headless.on_usage_limit) == OnUsageLimit::Exit,
    });
    if stream_events {
        let id = session_id.clone();
        runtime
            .state
            .usage_pause()
            .observe(std::sync::Arc::new(move |pause| {
                emit_line(&usage_event(&id, pause));
            }));
    }

    let mut last: Option<Handled> = None;
    let mut goal_failure: Option<String> = None;
    let mut reviewed = false;
    let mut code = 0u8;
    let mut last_usage_limit: Option<Value> = None;
    let mut usage_warned = HashSet::new();
    loop {
        let text = match input.next().await {
            Ok(Some(text)) => text,
            Ok(None) => break,
            Err(message) => {
                eprintln!("fontaine: {message}");
                runtime.jobs().terminate_all().await;
                runtime.lsp().shutdown_all().await;
                return 2;
            }
        };
        let mut handled = match runtime.refresh_before_turn() {
            Ok(()) => handle_message(runtime, &options, stream_events, text).await,
            Err(e) => Handled {
                outcome: Outcome {
                    subtype: Subtype::Execution,
                    text: String::new(),
                    structured: None,
                    error: Some(format!(
                        "the provider's web search could not be turned off, so the message was not run: {}",
                        fontaine_tools::error_chain::chain(&e)
                    )),
                    started: std::time::Instant::now(),
                    denials: Vec::new(),
                },
                goal: None,
                goal_ran: false,
                afk: afk_report(runtime),
            },
        };
        // A usage window that stayed spent ends the run whatever the turn made of it: a denial
        // alone would let the model keep making gated calls.
        let usage_limit = runtime
            .state
            .usage_pause()
            .failure()
            .map(|(end, info)| (usage_limit_object(end, &info), end, info));
        if let Some((_, end, info)) = &usage_limit {
            handled.outcome.subtype = Subtype::UsageLimit;
            handled.outcome.text.clear();
            handled.outcome.error = Some(
                fontaine_core::gate::UsagePauseEnded {
                    end: *end,
                    info: info.clone(),
                }
                .to_string(),
            );
        }
        let usage_limit = usage_limit.map(|(object, ..)| object);
        let failed = handled.outcome.subtype != Subtype::Success;
        // A goal that ended impossible or paused did not finish the task, even when the turn
        // itself answered. A failed turn already says why.
        goal_failure = handled
            .goal
            .as_ref()
            .and_then(goal::GoalReport::failure)
            .filter(|_| handled.goal_ran && !failed);
        // A blocked or stopped AFK run did not finish what it was asked, whether or not a goal
        // ran and whatever its last turn answered.
        // The review is paid for once, with the message that ends the run.
        if (input.is_exhausted() || failed || goal_failure.is_some())
            && !reviewed
            && let Some(report) = final_afk_report(runtime).await
        {
            handled.afk = Some(report);
            reviewed = true;
        }
        let outcome = &handled.outcome;
        let afk_failure = handled.afk.as_ref().and_then(afk::failure);
        if afk_failure.is_some() {
            goal_failure = goal_failure.or(afk_failure);
        }
        if failed {
            runtime.state.agents().kill_all();
            runtime.state.agents().settled(KILL_SETTLE).await;
            runtime.jobs().terminate_all().await;
            runtime.lsp().shutdown_all().await;
        }
        if stream_events {
            let account_usage = account_usage(runtime, &mut usage_warned).await;
            emit_line(&result_event(
                &session_id,
                outcome,
                &Usage::of(runtime),
                handled.goal.as_ref(),
                &account_usage,
                handled.afk.as_ref(),
                usage_limit.as_ref(),
            ));
        }
        last = Some(handled);
        last_usage_limit = usage_limit.clone();
        if usage_limit.is_some() {
            code = EXIT_USAGE_LIMIT;
            break;
        }
        if failed || goal_failure.is_some() {
            code = 1;
            break;
        }
    }
    if !reviewed
        && let Some(last) = last.as_mut()
        && let Some(report) = final_afk_report(runtime).await
    {
        last.afk = Some(report);
    }
    // The run was the AFK stretch: close it in the ledger.
    if runtime.state.afk_on()
        && let Err(e) = runtime.state.set_mode(PermissionMode::Auto, AfkSource::Cli)
    {
        eprintln!("fontaine: {e}");
    }
    runtime.state.agents().kill_all();
    runtime.state.agents().settled(KILL_SETTLE).await;
    // Jobs still running when the run ends are not waited for; they end with it.
    runtime.jobs().terminate_all().await;
    runtime.lsp().shutdown_all().await;

    let Some(last) = last else {
        eprintln!("fontaine: no prompt: the input held no user message");
        return 2;
    };
    match options.output_format {
        OutputFormat::StreamJson => {}
        OutputFormat::Json => {
            let account_usage = account_usage(runtime, &mut usage_warned).await;
            emit_line(&result_event(
                &session_id,
                &last.outcome,
                &Usage::of(runtime),
                last.goal.as_ref(),
                &account_usage,
                last.afk.as_ref(),
                last_usage_limit.as_ref(),
            ));
        }
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
    use super::{Outcome, Subtype, Usage, result_event, session_cost};
    use fontaine_core::usage_ledger::{Bucket, UsageLedger, UsageSource};

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
        let ledger = UsageLedger::default();
        for (read, written) in [(Some(800), Some(100)), (Some(900), None), (None, None)] {
            let mut usage = rig_core::completion::Usage::new();
            usage.input_tokens = Some(1000);
            usage.cached_input_tokens = read;
            usage.cache_creation_input_tokens = written;
            ledger.record(UsageSource::Main, &usage, true, &Default::default());
        }
        let outcome = Outcome {
            subtype: Subtype::Success,
            text: String::new(),
            structured: None,
            error: None,
            started: std::time::Instant::now(),
            denials: Vec::new(),
        };
        let bucket = |read, written| Bucket {
            cache_read_input_tokens: read,
            cache_creation_input_tokens: written,
            ..Bucket::default()
        };
        let usage = Usage {
            main: ledger.bucket(UsageSource::Main),
            guardian: bucket(10, 11),
            goal: bucket(30, 31),
            subagents: bucket(20, 21),
        };
        let result = result_event("s", &outcome, &usage, None, &[], None, None);
        assert_eq!(result["num_turns"], 3);
        assert_eq!(result["usage"]["main"]["input_tokens"], 3000);
        assert_eq!(result["usage"]["main"]["cache_read_input_tokens"], 1700);
        assert_eq!(result["usage"]["main"]["cache_creation_input_tokens"], 100);
        for (name, read, written) in [
            ("guardian", 10, 11),
            ("subagents", 20, 21),
            ("goal", 30, 31),
        ] {
            let bucket = &result["usage"][name];
            assert_eq!(bucket["cache_read_input_tokens"], read, "{name}");
            assert_eq!(bucket["cache_creation_input_tokens"], written, "{name}");
        }
    }
}
