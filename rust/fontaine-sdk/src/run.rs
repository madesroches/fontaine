//! The headless run loop: one turn per user message, with the model-list fall-through, usage-limit
//! wait, budget stop, `--json-schema` output, AFK stop, stop-hook continuation, goals and
//! interrupt. `fontaine -p`, the Claude Agent SDK mode and an embedded [`Session`](crate::Session)
//! all run it, each with its own input source and event sink.

mod afk;
mod goal;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use rig_core::schemars::Schema;
use serde_json::{Value, json};

use crate::core::RuntimeError;
use crate::core::afk_report::{AfkOutcome, AfkReport, AfkSource};
use crate::core::agent::AgentRuntime;
use crate::core::audit::Decision;
use crate::core::config::{OnUsageLimit, PermissionMode};
use crate::core::gate::UsageWait;
use crate::core::goal::{Due, GoalArg, PauseReason, Step, first_turn_prompt};
use crate::core::ledger::{MessageOrigin, Record};
use crate::core::model_roles::Tried;
use crate::core::providers::ExternalLogin;
use crate::core::quota::{AccountUsage, Quota, QuotaSource, UsageRole};
use crate::core::turn::{
    Flow, RETRY_NOTE, SessionEvent, TurnFailure, TurnOutcome, TurnParams, run_turn, user_message,
    wait_out_usage_limit,
};
use crate::core::turn_recovery::{Fallback, Unsettled};
use crate::core::usage_pause::PauseEvent;
use crate::core::user_hooks::PromptGate;
use crate::events::{
    GoalReport, Outcome, Subtype, Usage, main_usage, result_event, session_cost,
    usage_limit_object, write_stdout_line,
};
use crate::tools::kills::KILL_SETTLE;

pub use crate::events::SCHEMA_VERSION;

/// How the run's user messages arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    Text,
    StreamJson,
}

/// How the run's result reaches stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

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

/// What the usage-pause observer is: shared with the run's tasks, so it cannot borrow the sink.
pub type PauseObserver = Arc<dyn Fn(&PauseEvent) + Send + Sync>;

/// Where the run's events go. `HeadlessV1Sink` writes the `headless-v1` schema; the Claude Agent
/// SDK surface writes its own messages. Every emission site of a run goes through one of these.
pub trait EventSink: Send {
    /// A line for whoever runs the session: a startup notice, or a failure that ends the run. The
    /// stdout and stderr sinks print it prefixed with `fontaine: `.
    fn notice(&mut self, message: &str);
    /// The session has started and no message has been read yet.
    fn session_start(&mut self, runtime: &AgentRuntime, data_dir: &Path);
    /// A message was read and its turn is about to run.
    fn turn_start(&mut self, _runtime: &AgentRuntime, _data_dir: &Path) {}
    fn event(&mut self, session_id: &str, event: &SessionEvent);
    /// One evaluation of a `/goal`.
    fn goal_evaluation(
        &mut self,
        _session_id: &str,
        _judgment: Option<crate::core::goal::Judgment>,
        _reason: &str,
        _evaluations: u32,
    ) {
    }
    /// Whether `result` wants the usage windows of the accounts, which cost a request each.
    fn wants_account_usage(&self) -> bool;
    fn result(&mut self, result: &ResultInfo<'_>);
    /// What the usage-pause observer calls; it is shared with the run's tasks, so it cannot be a
    /// borrow of the sink.
    fn usage_observer(&self, session_id: &str) -> Option<PauseObserver>;
    /// The assessed report of a stream that ended after its last `result`, which could only say
    /// the assessment was pending.
    fn afk_report(&mut self, _session_id: &str, _report: &AfkReport) {}
    /// The input could not be read. The run ends after this.
    fn input_failed(&mut self, _session_id: &str, _message: &str) {}
}

/// What a turn's result is built from.
pub struct ResultInfo<'a> {
    pub runtime: &'a AgentRuntime,
    pub session_id: &'a str,
    pub outcome: &'a Outcome,
    pub usage: &'a Usage,
    pub goal: Option<&'a GoalReport>,
    pub account_usage: &'a [Value],
    pub afk: Option<&'a AfkReport>,
    pub usage_limit: Option<&'a Value>,
    /// The host interrupted the turn.
    pub interrupted: bool,
}

/// Where the run's user messages come from.
pub trait InputSource {
    /// Whether no further message can follow. A stream of lines cannot say before it ends.
    fn is_exhausted(&self) -> bool;
    /// The next user message, `None` at the end of input. An error is a usage error. Between
    /// messages the runtime is idle, so a source may act on it here.
    fn next(
        &mut self,
        runtime: &mut AgentRuntime,
    ) -> impl std::future::Future<Output = Result<Option<String>, String>>;
    /// A message is already waiting, so notification turns hold back.
    fn pending(&self) -> bool {
        false
    }
    /// A session the host keeps open: a failed turn is reported and the next message still runs,
    /// and an input that ends without a message is a normal end.
    fn long_lived(&self) -> bool {
        false
    }
    /// The exit code of a run whose input could not be read.
    fn failure_code(&self) -> u8 {
        2
    }
}

/// Stops the turn that is running. Set by the surface that owns the input, cleared when the
/// next message starts.
#[derive(Clone, Default)]
pub struct Cancel {
    inner: Arc<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    set: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
}

impl Cancel {
    pub fn trigger(&self) {
        self.inner
            .set
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.wake.notify_waiters();
    }

    pub fn reset(&self) {
        self.inner
            .set
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.inner.set.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        loop {
            let woken = self.inner.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if self.is_set() {
                return;
            }
            woken.await;
        }
    }
}

/// What the loop that runs one message needs besides the runtime.
pub struct Ctx<'a> {
    pub options: &'a Options,
    pub sink: &'a mut dyn EventSink,
    pub cancel: &'a Cancel,
    /// Whether a message is already waiting, so no notification turn starts.
    pub pending: &'a (dyn Fn() -> bool + Sync),
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
                        crate::tools::error_chain::chain(&e)
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
    ctx: &mut Ctx<'_>,
    text: String,
) -> (Outcome, Option<String>) {
    // Goal, notification and stop-hook turns start here without passing the per-message refresh.
    if let Err(e) = runtime.refresh_before_turn() {
        return (refresh_failed(&e), None);
    }
    let mut fell = None;
    let mut tried = Tried::default();
    let (mut outcome, mut note) = run_attempt(runtime, ctx, text, &mut fell).await;
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
                let (next, next_note) =
                    run_attempt(runtime, ctx, RETRY_NOTE.to_string(), &mut fell).await;
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
        let (next, next_note) = run_attempt(runtime, ctx, RETRY_NOTE.to_string(), &mut fell).await;
        outcome = merge(outcome, next);
        note = next_note;
    }
    (outcome, note)
}

fn refresh_failed(e: &(dyn std::error::Error + 'static)) -> Outcome {
    Outcome {
        subtype: Subtype::Execution,
        text: String::new(),
        structured: None,
        error: Some(format!(
            "the provider's web search could not be turned off, so the turn did not run: {}",
            crate::tools::error_chain::chain(e)
        )),
        started: std::time::Instant::now(),
        denials: Vec::new(),
    }
}

/// One run of the agent on `text`. A failure that is the model's quota running out is left in
/// `fell` for the caller to answer.
async fn run_attempt(
    runtime: &AgentRuntime,
    ctx: &mut Ctx<'_>,
    text: String,
    fell: &mut Option<TurnFailure>,
) -> (Outcome, Option<String>) {
    let options = ctx.options;
    let cancel = ctx.cancel.clone();
    let sink = &mut *ctx.sink;
    // After the compaction that may have preceded this run, so the notes it queued are in.
    let text = with_hook_context(runtime, text);
    let started = std::time::Instant::now();
    let sid = runtime.session.id.as_str();
    let mut denials: Vec<Value> = Vec::new();
    let mut on_event = |event_in: SessionEvent| -> Flow {
        sink.event(sid, &event_in);
        match event_in {
            SessionEvent::Decision(record) => {
                if record.decision == Decision::Deny {
                    denials.push(json!({
                        "tool": record.tool,
                        "tier": record.tier.as_str(),
                        "rule": record.rule,
                        "explanation": record.explanation,
                    }));
                }
            }
            SessionEvent::Usage { calls_tool, .. } => {
                // The cap is checked between turns: a spent budget stops the run before its
                // tool calls run, but never discards an answer already complete.
                if calls_tool
                    && let (Some(cap), Some(spent)) = (options.max_budget_usd, spent(runtime))
                    && spent > cap
                {
                    return Flow::Stop;
                }
            }
            _ => {}
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
            cost: runtime.models.main.cost_basis(),
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
    // A host's interrupt drops the turn the same way, which kills its foreground children through
    // their drop guards.
    let mut interrupted = false;
    let turn = tokio::select! {
        biased;
        Ok(()) = stops.changed() => None,
        () = cancel.cancelled() => {
            interrupted = true;
            None
        }
        outcome = turn => Some(outcome),
    };
    if interrupted {
        runtime
            .keep_unsettled_turn(Unsettled::User, Some(fallback))
            .await;
        return (Outcome::interrupted(started, denials), None);
    }
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
fn announce(runtime: &AgentRuntime, sink: &mut dyn EventSink) {
    for notice in runtime.scanner_notices() {
        sink.notice(&notice.to_string());
    }
    if !matches!(
        runtime.start_mode(),
        PermissionMode::Auto | PermissionMode::Afk
    ) {
        // The guardian is never consulted here, but a subagent on its model stays quiet in
        // expectation of this notice, so it is printed even so.
        if let Some(notice) = &runtime.models.main.facts_notice {
            sink.notice(&notice.to_string());
        }
        if let Some(crate::core::models::GuardianModel::Ready(handle)) = &runtime.models.guardian
            && handle.reference != runtime.models.main.reference
            && let Some(notice) = &handle.facts_notice
        {
            sink.notice(&notice.to_string());
        }
        return;
    }
    for notice in runtime.notices() {
        sink.notice(&notice.to_string());
    }
    let guardian = runtime.state.guardian();
    if runtime.models.guardian.is_some()
        && let Some(reason) = guardian.unavailable_reason()
    {
        let label = match &runtime.models.guardian {
            Some(crate::core::models::GuardianModel::Unavailable {
                not_signed_in: true,
                ..
            }) => "GuardianNotSignedIn",
            _ => "GuardianUnavailable",
        };
        sink.notice(&format!(
            "the guardian model {} is unavailable ({reason}); every call that needs it is denied with {label}",
            guardian.label()
        ));
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
/// no subagent is left running. Under a goal (`goal`) it also waits on background jobs, when
/// check-ins are on to end the wait, and returns when a check-in falls due.
async fn wait_for_news(
    runtime: &AgentRuntime,
    over_cap: &(dyn Fn() -> bool + Sync),
    host_waiting: &(dyn Fn() -> bool + Sync),
    goal: bool,
) -> News {
    let waits_on_jobs = goal && runtime.config.goal.checkin_minutes > 0;
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
        // A host's message goes before any notification turn; the children keep running and
        // their reports wait for the next quiet moment.
        if host_waiting() {
            return News::Done;
        }
        if let Some(prompt) = runtime.notification_prompt().await {
            return News::Prompt(prompt);
        }
        if goal && runtime.goal_checkin_wait(false) == Some(std::time::Duration::ZERO) {
            return News::Done;
        }
        let jobs_running = waits_on_jobs && !runtime.jobs().running_background().is_empty();
        if !tree.any_running() && !jobs_running {
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
        let poll = std::time::Duration::from_millis(100);
        let nap = match goal.then(|| runtime.goal_checkin_wait(false)).flatten() {
            Some(until) => until.min(poll),
            None => poll,
        };
        tokio::select! {
            _ = &mut changed => {}
            Ok(()) = stops.changed() => return News::Stopped,
            _ = tokio::time::sleep(nap) => {}
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
        error: Some(format!("{detail}: {}", crate::tools::error_chain::chain(e))),
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
async fn guarded_turn(runtime: &mut AgentRuntime, ctx: &mut Ctx<'_>, prompt: String) -> Outcome {
    if let Some(stop) = stop_before_call(runtime, ctx.options, Some(&prompt)).await {
        runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
        return stop;
    }
    let (outcome, note) = run_one(runtime, ctx, prompt).await;
    let outcome = continue_on_stop(runtime, ctx, outcome, note).await;
    runtime.goal_turn_ended(Some(&goal::turn_end(&outcome)));
    outcome
}

/// Runs further turns while a `stop` hook keeps blocking the end of the run, each through the
/// budget check and compaction a turn of its own gets.
async fn continue_on_stop(
    runtime: &mut AgentRuntime,
    ctx: &mut Ctx<'_>,
    mut outcome: Outcome,
    mut note: Option<String>,
) -> Outcome {
    while let Some(prompt) = note.take() {
        if let Some(stop) = stop_before_call(runtime, ctx.options, Some(&prompt)).await {
            return merge(outcome, stop);
        }
        let (next, again) = run_one(runtime, ctx, prompt).await;
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
    ctx: &mut Ctx<'_>,
    mut outcome: Outcome,
    goal: bool,
) -> Outcome {
    while outcome.subtype == Subtype::Success {
        let options = ctx.options;
        match wait_for_news(
            runtime,
            &|| over_cap(runtime, options),
            &|| (ctx.pending)() || ctx.cancel.is_set(),
            goal,
        )
        .await
        {
            News::Prompt(prompt) => {
                // A turn an earlier fold failed to keep goes before this one; the record
                // run_one writes must not be read as this turn's fallback first.
                runtime
                    .keep_unsettled_turn(Unsettled::SessionEnded, None)
                    .await;
                let (next, note) = run_one(runtime, ctx, prompt).await;
                let next = continue_on_stop(runtime, ctx, next, note).await;
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
    goal: Option<GoalReport>,
    /// The message ran turns under a goal, so how the goal ended says how the run did.
    goal_ran: bool,
    /// The AFK report, while AFK is on or a stop stands.
    afk: Option<AfkReport>,
}

/// One user message. `/goal <condition>` keeps running turns after the first until the
/// evaluator finds the condition met, or the goal ends or pauses.
async fn handle_message(runtime: &mut AgentRuntime, ctx: &mut Ctx<'_>, text: String) -> Handled {
    let options = ctx.options;
    let session_id = runtime.session.id.clone();
    // A stream-json line can be written before the previous reply arrives, so only the one
    // text prompt is known to follow what the user saw.
    let origin = match options.input_format {
        InputFormat::Text => MessageOrigin::Prompt,
        InputFormat::StreamJson => MessageOrigin::Other,
    };
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
                remote: None,
                origin,
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
                    error: Some(crate::tools::error_chain::chain(&e)),
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
                remote: None,
                origin: MessageOrigin::Other,
                text,
                attachments: Vec::new(),
                agent: None,
            });
            first_turn_prompt(&condition)
        }
    };
    runtime.goal_user_prompt();
    let outcome = guarded_turn(runtime, ctx, prompt).await;
    let mut outcome = settle(runtime, ctx, outcome, involved).await;
    let mut ended = None;
    while involved && outcome.subtype == Subtype::Success {
        // Blocked or stopped: no goal turn starts. The goal is paused with the reason below.
        if runtime.unattended_blocked().is_some() {
            break;
        }
        let Some(due) = runtime.next_goal_step(true) else {
            // Evaluation is still deferred behind work this run does not wait for.
            // The wait also ends when the host has a message queued or interrupted; that is not
            // a reason to pause.
            if runtime.goal_deferred() && !(ctx.pending)() && !ctx.cancel.is_set() {
                runtime.pause_goal(PauseReason::BackgroundWait);
            }
            break;
        };
        let pending = match due {
            Due::Evaluate(pending) => pending,
            Due::CheckIn(check) => {
                let prompt = runtime.checkin_prompt(&check);
                let turn = guarded_turn(runtime, ctx, prompt).await;
                outcome = settle(runtime, ctx, merge(outcome, turn), involved).await;
                continue;
            }
            // Nobody is there to send the next prompt, so the wait ends here.
            Due::IdleExhausted => {
                runtime.pause_goal(PauseReason::BackgroundWait);
                break;
            }
            Due::Paused(_) => break,
        };
        if let Some(stop) = stop_before_call(runtime, options, None).await {
            runtime.goal_turn_ended(Some(&goal::turn_end(&stop)));
            outcome = merge(outcome, stop);
            break;
        }
        let Some(applied) = runtime.evaluate_goal(&pending).await else {
            break;
        };
        ctx.sink.goal_evaluation(
            &session_id,
            applied.judgment,
            &applied.reason,
            applied.evaluations,
        );
        match applied.step {
            Step::Continue(next) => {
                let prompt = runtime.goal_prompt(&next).await;
                let turn = guarded_turn(runtime, ctx, prompt).await;
                outcome = settle(runtime, ctx, merge(outcome, turn), involved).await;
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
                .warn(&crate::tools::error_chain::chain(&e));
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

/// The session loop both surfaces share: one turn per user message, until the input ends or a
/// message fails.
pub async fn run_session<I: InputSource + Sync>(
    options: Options,
    runtime: &mut AgentRuntime,
    data_dir: &Path,
    guardian_login: Option<(String, ExternalLogin)>,
    mut input: I,
    sink: &mut dyn EventSink,
    cancel: &Cancel,
) -> u8 {
    if let Some((account, login)) = guardian_login
        && matches!(
            runtime.start_mode(),
            PermissionMode::Auto | PermissionMode::Afk
        )
    {
        sink.notice(&format!(
            "the guardian's account `{account}` is not signed in; run `{}`, then retry",
            login.display
        ));
        return 1;
    }

    let session_id = runtime.session.id.clone();
    runtime.record_session_start().await;
    announce(runtime, sink);
    sink.session_start(runtime, data_dir);
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
    if let Some(observer) = sink.usage_observer(&session_id) {
        runtime.state.usage_pause().observe(observer);
    }

    let mut last: Option<Handled> = None;
    let mut goal_failure: Option<String> = None;
    let mut reviewed = false;
    let mut code = 0u8;
    let mut last_usage_limit: Option<Value> = None;
    let mut usage_warned = HashSet::new();
    loop {
        let text = match input.next(&mut *runtime).await {
            Ok(Some(text)) => text,
            Ok(None) => break,
            Err(message) => {
                sink.notice(&message);
                sink.input_failed(&session_id, &message);
                runtime.jobs().terminate_all().await;
                runtime.lsp().shutdown_all().await;
                return input.failure_code();
            }
        };
        sink.turn_start(runtime, data_dir);
        let mut handled = match runtime.refresh_before_turn() {
            Ok(()) => {
                let pending = || input.pending();
                let mut ctx = Ctx {
                    options: &options,
                    sink: &mut *sink,
                    cancel,
                    pending: &pending,
                };
                handle_message(runtime, &mut ctx, text).await
            }
            Err(e) => Handled {
                outcome: refresh_failed(&e),
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
                crate::core::gate::UsagePauseEnded {
                    end: *end,
                    info: info.clone(),
                }
                .to_string(),
            );
        }
        let usage_limit = usage_limit.map(|(object, ..)| object);
        // A host's interrupt is not a failure of the run: the next message goes on.
        let interrupted = cancel.is_set();
        let failed = handled.outcome.subtype != Subtype::Success && !interrupted;
        // A goal that ended impossible or paused did not finish the task, even when the turn
        // itself answered. A failed turn already says why.
        goal_failure = handled
            .goal
            .as_ref()
            .and_then(goal::failure)
            .filter(|_| handled.goal_ran && !failed);
        // A blocked or stopped AFK run did not finish what it was asked, whether or not a goal
        // ran and whatever its last turn answered.
        // The review is paid for once, with the message that ends the run.
        // A long-lived input is reviewed once, after it ends: a failed turn there is not the end.
        if (input.is_exhausted() || ((failed || goal_failure.is_some()) && !input.long_lived()))
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
        // A long-lived session keeps reading after a failed turn: kill_all latches shutdown.
        if failed && !input.long_lived() {
            runtime.state.agents().kill_all();
            runtime.state.agents().settled(KILL_SETTLE).await;
            runtime.jobs().terminate_all().await;
            runtime.lsp().shutdown_all().await;
        }
        let account_usage = if sink.wants_account_usage() {
            account_usage(runtime, &mut usage_warned).await
        } else {
            Vec::new()
        };
        sink.result(&ResultInfo {
            runtime,
            session_id: &session_id,
            outcome,
            usage: &Usage::of(runtime),
            goal: handled.goal.as_ref(),
            account_usage: &account_usage,
            afk: handled.afk.as_ref(),
            usage_limit: usage_limit.as_ref(),
            interrupted,
        });
        last = Some(handled);
        last_usage_limit = usage_limit.clone();
        if usage_limit.is_some() {
            code = EXIT_USAGE_LIMIT;
            break;
        }
        if (failed || goal_failure.is_some()) && !input.long_lived() {
            code = 1;
            break;
        }
    }
    if !reviewed
        && let Some(last) = last.as_mut()
        && let Some(report) = final_afk_report(runtime).await
    {
        sink.afk_report(&session_id, &report);
        last.afk = Some(report);
    }
    // The run was the AFK stretch: close it in the ledger.
    if runtime.state.afk_on()
        && let Err(e) = runtime.state.set_mode(PermissionMode::Auto, AfkSource::Cli)
    {
        sink.notice(&e.to_string());
    }
    runtime.state.agents().kill_all();
    runtime.state.agents().settled(KILL_SETTLE).await;
    // Jobs still running when the run ends are not waited for; they end with it.
    runtime.jobs().terminate_all().await;
    runtime.lsp().shutdown_all().await;

    let Some(last) = last else {
        if input.long_lived() {
            return 0;
        }
        sink.notice("no prompt: the input held no user message");
        return 2;
    };
    match options.output_format {
        OutputFormat::StreamJson => {}
        OutputFormat::Json => {
            let account_usage = account_usage(runtime, &mut usage_warned).await;
            let event = result_event(
                &session_id,
                &last.outcome,
                &Usage::of(runtime),
                last.goal.as_ref(),
                &account_usage,
                last.afk.as_ref(),
                last_usage_limit.as_ref(),
            );
            write_stdout_line(&serde_json::to_string(&event).expect("an event serializes"));
        }
        OutputFormat::Text => {
            if let Some(failure) = &goal_failure {
                sink.notice(failure);
            } else if let Some(error) = &last.outcome.error {
                sink.notice(error);
            } else {
                write_stdout_line(&last.outcome.text);
            }
        }
    }
    code
}
