//! One user message run to its end. The runner drives the agent's stream and reports what
//! happens as typed session events to a consumer, which decides how to show them and may stop
//! the run between model calls. Headless and the TUI both run on it.

use base64::Engine;
use fontaine_tools::gate::{AgentId, InputModalities, ROOT_AGENT};
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::completion::PromptError;
use rig_agent::prelude::*;
use rig_core::catalog::Modalities;
use rig_core::completion::{GenerationOptions, Usage};
use rig_core::error::{ErrorReport, ProviderError};
use rig_core::message::{
    AssistantContent, DocumentMediaType, Message, ToolResultContent, UserContent,
};
use rig_core::streaming::{Item, StreamEvent};
use rig_core::tool::ToolContext;
use serde_json::Value;
use uuid::Uuid;

use crate::audit::DecisionRecord;
use crate::gate::{DecisionFeed, Published, SessionState};
use crate::ledger::Record;
use crate::preview::DiffPreview;
use crate::usage_ledger::{Served, UsageSource};
use crate::usage_limit::{UsageLimit, is_window_code, prompt_usage_limit};

/// The retries a transient transport failure earns, on top of the first attempt.
const MAX_RETRIES: usize = 2;

/// The pause before the first retry; it doubles each time. A reset connection needs a
/// moment, not a hammer.
const BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

/// A retry's prompt. The failed attempt's committed messages are already saved by the
/// recovery hook, so the run only needs to be told to go on. Model context, never a ledger
/// user message.
pub const RETRY_NOTE: &str = "[Harness note, not the user's words: the previous model call failed \
before it finished; continue the task.]";

#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// A fragment of the answer as it streams.
    AssistantDelta(String),
    /// A complete piece of assistant text.
    Assistant(String),
    /// A fragment of the model's reasoning as it streams. Display only.
    ReasoningDelta(String),
    /// The model's reasoning as it settled; may restate what the deltas carried.
    Reasoning(String),
    ToolCall {
        id: String,
        tool: String,
        args: Value,
    },
    /// The pipeline decided a call; a prompt that a human answers produces a second one.
    Decision(Box<DecisionRecord>),
    ToolResult {
        id: String,
        tool: String,
        success: bool,
        text: String,
    },
    /// The change an allowed `edit` or `write` call is about to make, for call `id`. Display
    /// only, and always ahead of that call's result.
    ToolPreview {
        id: String,
        preview: Box<DiffPreview>,
    },
    /// One model call finished. `calls_tool` says its turn asked for tools, which have not run
    /// yet: stopping now spares them.
    Usage { usage: Usage, calls_tool: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Answer(String),
    MaxTurns(String),
    /// The consumer stopped the run.
    Stopped,
    Failed(TurnFailure),
}

/// Why a turn failed, with whether the failure is the account's quota running out, which a model
/// list answers by moving to its next entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnFailure {
    pub message: String,
    pub falls_through: bool,
    /// The stable error class of the failed request, as the `api_error` event carries it.
    pub class: Option<&'static str>,
    /// The class a `stop_failure` hook is told; one of [`crate::config::STOP_FAILURE_KINDS`].
    pub kind: &'static str,
    /// Set when the failure is a spent usage window, which pauses the run once the model list
    /// has no entry left to try.
    pub usage_limit: Option<UsageLimit>,
}

impl TurnFailure {
    /// A failure that is not a quota failure and did not come from a provider call, so it is of
    /// the `unknown` kind.
    pub fn new(message: impl Into<String>) -> TurnFailure {
        TurnFailure {
            message: message.into(),
            falls_through: false,
            class: None,
            kind: "unknown",
            usage_limit: None,
        }
    }
}

impl From<String> for TurnFailure {
    fn from(message: String) -> Self {
        TurnFailure::new(message)
    }
}

impl From<&str> for TurnFailure {
    fn from(message: &str) -> Self {
        TurnFailure::new(message)
    }
}

impl std::fmt::Display for TurnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The `stop_failure` class of a failed model request.
fn failure_kind(error: &PromptError) -> &'static str {
    use crate::providers::claude_subscription::{ErrorKind as Claude, claude_error};
    if let PromptError::Provider(e) = error
        && let Some(claude) = claude_error(e)
    {
        return match claude.kind {
            Claude::Auth | Claude::NotSignedIn => "authentication_failed",
            Claude::Limit => "rate_limit",
            Claude::UsageCredits => "billing_error",
            Claude::Catalog => "model_not_found",
            Claude::Unsupported => "invalid_request",
            _ => "unknown",
        };
    }
    let body = error.provider_response_body().unwrap_or("").to_lowercase();
    match error.provider_response_status().map(|s| s.as_u16()) {
        Some(401) => "authentication_failed",
        Some(403) if body.contains("organization") => "oauth_org_not_allowed",
        Some(403) => "authentication_failed",
        Some(402) => "billing_error",
        Some(404) => "model_not_found",
        Some(400 | 413 | 422) if body.contains("max_tokens") => "max_output_tokens",
        Some(429) if body.contains("credit") || body.contains("billing") => "billing_error",
        Some(429) => "rate_limit",
        Some(529) => "overloaded",
        Some(500..=599) if body.contains("overloaded") => "overloaded",
        Some(500..=599) => "server_error",
        Some(400..=499) => "invalid_request",
        _ => "unknown",
    }
}

pub struct TurnParams<'a> {
    pub agent: &'a Agent,
    pub state: &'a SessionState,
    pub prompt: String,
    /// Inline files the prompt carries: a clipboard image or a dropped PDF. Gated by
    /// `modalities` at the message build; what the model does not accept collapses into a
    /// text note.
    pub attachments: Vec<Attachment>,
    /// What the current model accepts; decides which attachments become content parts.
    pub modalities: Modalities,
    /// The most model calls the turn may make; `None` for no limit.
    pub max_turns: Option<usize>,
    /// Whether the cost the model's calls report is counted; see `ModelHandle::catalog_billed`.
    pub priced: bool,
    /// The model's id and provider, for the `api_error` event.
    pub model: String,
    pub provider: String,
    /// Whether the request carries the provider's built-in search tool.
    pub native_search: bool,
    /// The turn's reasoning effort, from `ModelHandle::generation_options` on the current model;
    /// `None` sends nothing.
    pub generation_options: Option<GenerationOptions>,
    /// The output cap the turn's calls send, from `ModelHandle::turn_max_tokens`; `None` sends
    /// nothing.
    pub max_tokens: Option<u64>,
    /// The role and model the turn's calls are recorded against in the usage ledger.
    pub served: Served,
}

/// What an inline attachment is.
#[derive(Clone, Debug, PartialEq)]
pub enum AttachmentMedia {
    /// A clipboard image, sniffed at attach time.
    Image(rig_core::message::ImageMediaType),
    /// A dropped PDF file.
    Pdf,
}

/// An inline file a user message carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// What the user knows it as: a file name.
    pub label: String,
    pub bytes: Vec<u8>,
    pub media: AttachmentMedia,
}

/// The user message for one run: the prompt's text part, then one content part per
/// attachment the model accepts. An attachment the model does not accept becomes a text
/// note naming it, so nothing the user sent reaches the model unseen.
pub fn user_message(prompt: &str, attachments: &[Attachment], modalities: &Modalities) -> Message {
    let mut content = vec![UserContent::text(prompt)];
    for attachment in attachments {
        let data = base64::engine::general_purpose::STANDARD.encode(&attachment.bytes);
        match &attachment.media {
            AttachmentMedia::Image(media) if modalities.image => {
                content.push(UserContent::image_base64(data, Some(media.clone()), None));
            }
            AttachmentMedia::Pdf if modalities.pdf => {
                content.push(UserContent::document_base64(
                    data,
                    Some(DocumentMediaType::PDF),
                ));
            }
            media => {
                let kind = match media {
                    AttachmentMedia::Image(_) => "an image",
                    AttachmentMedia::Pdf => "a PDF",
                };
                content.push(UserContent::text(format!(
                    "[attachment: {kind} \"{}\" · {} not sent: the current model does not accept \
                     it]",
                    attachment.label,
                    fontaine_tools::notebook::human_size(attachment.bytes.len() as u64),
                )));
            }
        }
    }
    Message::User { content }
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

/// Runs `params.prompt` to an answer, a limit or a failure. The permission rules a skill lent
/// for the turn end with it.
pub async fn run_turn(
    params: TurnParams<'_>,
    on_event: &mut (dyn FnMut(SessionEvent) -> Flow + Send),
) -> TurnOutcome {
    let interaction = tracing::info_span!(
        target: "fontaine::telemetry",
        "fontaine.interaction",
        prompt.id = %Uuid::new_v4(),
        prompt.length = params.prompt.len() as u64,
        model.priced = params.priced,
    );
    let mut feed = params.state.feed().await;
    // A guard, not a trailing call: an interrupted turn drops this future at an await point.
    let _grants = EndTurnOnDrop(params.state);
    drive(
        Drive {
            agent: params.agent,
            state: params.state,
            agent_id: ROOT_AGENT,
            prompt: params.prompt,
            attachments: params.attachments,
            modalities: params.modalities,
            max_turns: params.max_turns,
            priced: params.priced,
            model: params.model,
            provider: params.provider,
            native_search: params.native_search,
            generation_options: params.generation_options,
            max_tokens: params.max_tokens,
            served: params.served,
            span: interaction,
        },
        Some(&mut feed),
        on_event,
    )
    .await
}

/// Ends the loan of skill rules when dropped, however the turn ends.
pub struct EndTurnOnDrop<'a>(pub &'a SessionState);

impl Drop for EndTurnOnDrop<'_> {
    fn drop(&mut self) {
        self.0.end_turn();
    }
}

/// One model request's progress. The tree shows the characters written so far; a drop guard
/// clears them and logs the request's end, because an interrupt drops the run's future at an
/// await and nothing after the loop runs.
struct ModelStep<'a> {
    state: &'a SessionState,
    agent: &'a str,
    started: std::time::Instant,
    chars: u64,
    open: bool,
    awaiting_next: bool,
}

impl<'a> ModelStep<'a> {
    fn begin(state: &'a SessionState, agent: &'a str) -> Self {
        tracing::debug!(agent.id = agent, "model request start");
        ModelStep {
            state,
            agent,
            started: std::time::Instant::now(),
            chars: 0,
            open: true,
            awaiting_next: false,
        }
    }

    fn note(&mut self, chars: usize) {
        let chars = chars as u64;
        self.chars += chars;
        self.state.agents().note_writing(self.agent, chars);
    }

    /// Ends the logged request. The next one starts at its first stream item, not here: the
    /// tools run (and may wait on approval) before the engine sends it.
    fn end_request(&mut self) {
        self.log_end();
        self.state.agents().reset_writing(self.agent);
        self.chars = 0;
        self.open = false;
        self.awaiting_next = true;
    }

    fn start_pending(&mut self) {
        if self.awaiting_next {
            self.awaiting_next = false;
            self.started = std::time::Instant::now();
            self.open = true;
            tracing::debug!(agent.id = self.agent, "model request start");
        }
    }

    /// The run is over and no further request follows.
    fn close(&mut self) {
        self.open = false;
    }

    fn log_end(&self) {
        if self.open {
            tracing::debug!(
                agent.id = self.agent,
                elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                chars = self.chars,
                "model request end"
            );
        }
    }
}

impl Drop for ModelStep<'_> {
    fn drop(&mut self) {
        self.log_end();
        self.state.agents().reset_writing(self.agent);
    }
}

/// One agent's run: whose it is, what it is asked and under which span it runs.
pub struct Drive<'a> {
    pub agent: &'a Agent,
    pub state: &'a SessionState,
    pub agent_id: &'a str,
    pub prompt: String,
    /// Inline files the prompt carries; sent as content parts the model accepts.
    pub attachments: Vec<Attachment>,
    /// What the current model accepts; decides which attachments become content parts.
    pub modalities: Modalities,
    /// The most model calls the run may make; `None` for no limit.
    pub max_turns: Option<usize>,
    /// Whether the cost the model's calls report is counted. An unpriced model's is dropped
    /// here, where usage enters, because rig attaches it inside the wire.
    pub priced: bool,
    /// The model's id and provider, for the `api_error` event.
    pub model: String,
    pub provider: String,
    /// Whether the request carries the provider's built-in search tool.
    pub native_search: bool,
    /// The reasoning effort this run's model calls send, already checked against the model;
    /// `None` sends nothing.
    pub generation_options: Option<GenerationOptions>,
    /// The output cap this run's model calls send; `None` sends nothing.
    pub max_tokens: Option<u64>,
    /// The role and model this run's calls are recorded against in the usage ledger.
    pub served: Served,
    /// The run adopts this span as its own agent span.
    pub span: tracing::Span,
}

/// The most of a failure's text the ledger keeps.
const FAILURE_RECORD_CHARS: usize = 2_000;

/// The stable class of a failed model request. Other failures (the turn limit, a cancel, the
/// memory store, an unknown tool) are not failed API requests. The message is not exported: a
/// provider's can echo the request.
fn error_class(error: &PromptError) -> Option<&'static str> {
    match error {
        PromptError::Provider(e) => Some(e.kind().code()),
        PromptError::Report(report) => Some(report.kind.code()),
        _ => None,
    }
}

/// The failure as shown to the user. A 400 whose body names `web_search` means the endpoint
/// rejected the provider's built-in search tool; nothing retries without it, so the text says
/// how to turn it off.
fn failure_text(error: &PromptError, offered_search: bool) -> String {
    // rig renders every hook stop as "the run was cancelled", the user's Esc
    // wording; a stop is the harness's own doing (the journal guard, the AFK
    // stop), and its reason already says why.
    if let PromptError::Cancelled { reason, .. } = error {
        return reason.clone();
    }
    let mut text = error.to_string();
    let rejected_search = offered_search
        && error
            .provider_response_status()
            .is_some_and(|status| status.as_u16() == 400)
        && error
            .provider_response_body()
            .is_some_and(|body| body.contains("web_search"));
    if rejected_search {
        text.push_str(
            "\nThe request carried the provider's built-in web search, which this endpoint \
             rejected. Set `native = \"off\"` under `[websearch]` to use the configured search \
             backend instead.",
        );
    }
    text
}

async fn next_fed(feed: &mut Option<&mut DecisionFeed>) -> Option<Published> {
    match feed {
        Some(feed) => feed.next_published().await,
        None => std::future::pending().await,
    }
}

/// Whether a failed attempt is a transient transport failure worth trying again: a reset
/// connection, a cut stream, a timeout, throttling or a 5xx. rig does the classification.
fn retryable(error: &PromptError) -> bool {
    match error {
        PromptError::Provider(e) => e.is_retryable(),
        PromptError::Report(report) => report.is_retryable(),
        _ => false,
    }
}

/// Names a provider's rate-limit, quota and overload replies that carry only a machine code,
/// because rig delivers mid-stream errors without a status. The numeric envelope codes are
/// decimal strings.
const QUOTA_CODES: &[&str] = &[
    "402",
    "429",
    "529",
    "rate_limit_exceeded",
    "too_many_requests",
    "resource_exhausted",
    "ThrottlingException",
    "quota_exceeded",
    "billing_hard_limit_reached",
    "overloaded_error",
    "rate_limit_error",
    "insufficient_quota",
];

/// Whether a reply with this status and machine code is the account's quota running out or the
/// endpoint overloaded: what a model list answers by moving to its next entry. A 400 or a 500 is
/// a bug or an outage that another model would hide.
fn quota_reply(status: Option<u16>, code: Option<&str>) -> bool {
    matches!(status, Some(402 | 429 | 529))
        || code.is_some_and(|code| QUOTA_CODES.contains(&code) || is_window_code(code))
}

/// [`falls_through`] for a provider error, which is also what a one-shot completion returns.
pub fn provider_falls_through(error: &ProviderError) -> bool {
    use crate::providers::claude_subscription::{ErrorKind, claude_error};
    let status = error.provider_response_status().map(|s| s.as_u16());
    let code = error.provider_response().and_then(|r| r.machine_code());
    quota_reply(status, code.as_deref())
        || claude_error(error).is_some_and(|e| e.kind == ErrorKind::Limit)
}

/// [`falls_through`] for a relayed report. A plan limit that only a report's text describes is
/// not recognised: nothing matches on text.
pub fn report_falls_through(report: &ErrorReport) -> bool {
    let code = report.code.clone().or_else(|| {
        report
            .provider_response
            .as_ref()
            .and_then(|r| r.machine_code())
    });
    let status = report.http_status.or_else(|| {
        report
            .provider_response
            .as_ref()
            .and_then(|r| r.status)
            .map(|s| s.as_u16())
    });
    quota_reply(status, code.as_deref())
}

/// Whether a failed run is a quota-class failure of its model: a 402, 429 or 529, a code that
/// names one, or a `claude-subscription` plan limit. Fontaine's own token and cost budgets are
/// the user's limit, not the account's, and never come through here.
pub fn falls_through(error: &PromptError) -> bool {
    match error {
        PromptError::Provider(e) => provider_falls_through(e),
        PromptError::Report(report) => report_falls_through(report),
        _ => false,
    }
}

/// [`falls_through`] for a one-shot job's error: whichever provider or run error is in its
/// `source()` chain. The guardian crate wraps rig's error without naming the core's types.
pub fn error_falls_through(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(e) = current {
        if let Some(run) = e.downcast_ref::<PromptError>() {
            return falls_through(run);
        }
        if let Some(provider) = e.downcast_ref::<ProviderError>() {
            return provider_falls_through(provider);
        }
        current = e.source();
    }
    false
}

/// Holds the run until the usage window `failure` reports resets, for a caller whose model list
/// has nothing left to try. `true` means the wait ended and the caller should run the request
/// again with its list's tried entries forgotten. `false` leaves `failure` as it was, or, when
/// the wait was cancelled or ran out, rewritten to say so.
pub async fn wait_out_usage_limit(
    state: &SessionState,
    failure: &mut TurnFailure,
    role: crate::quota::UsageRole,
    account: &str,
    source: Option<std::sync::Arc<dyn crate::quota::QuotaSource>>,
) -> bool {
    let Some(limit) = failure.usage_limit.clone() else {
        return false;
    };
    match state.pause_for_usage(role, account, &limit, source).await {
        Ok(()) => true,
        Err(ended) => {
            failure.message = ended.to_string();
            failure.usage_limit = None;
            failure.falls_through = false;
            false
        }
    }
}

/// How a single attempt ended: a settled turn, or a model failure the caller may retry.
enum DriveEnd {
    Outcome(TurnOutcome),
    Failed(Box<PromptError>),
}

/// Drives one agent's stream, reporting what happens to `on_event`. The root's run also
/// forwards the decisions the pipeline makes for the whole tree through `feed`; a subagent's
/// run has none. A retryable transport failure is retried a bounded number of times before
/// the failure settles the turn; the recovery hook has saved the failed attempt by then, so
/// a retry continues the task on a continuation note instead of starting over.
pub async fn drive(
    mut params: Drive<'_>,
    mut feed: Option<&mut DecisionFeed>,
    on_event: &mut (dyn FnMut(SessionEvent) -> Flow + Send),
) -> TurnOutcome {
    for attempt in 0..=MAX_RETRIES {
        // A retry continues on a note: the failed attempt's message, attachments included,
        // is already saved by the recovery hook, so the retry sends the note alone.
        let attachments = if attempt == 0 {
            std::mem::take(&mut params.attachments)
        } else {
            Vec::new()
        };
        let end = drive_once(
            Drive {
                agent: params.agent,
                state: params.state,
                agent_id: params.agent_id,
                prompt: std::mem::take(&mut params.prompt),
                attachments,
                modalities: params.modalities,
                max_turns: params.max_turns,
                priced: params.priced,
                model: params.model.clone(),
                provider: params.provider.clone(),
                native_search: params.native_search,
                generation_options: params.generation_options.clone(),
                max_tokens: params.max_tokens,
                served: params.served.clone(),
                span: params.span.clone(),
            },
            feed.as_deref_mut(),
            on_event,
        )
        .await;
        let error = match end {
            DriveEnd::Outcome(outcome) => return outcome,
            DriveEnd::Failed(error) => error,
        };
        // A spent window is not retried with a backoff however retryable its status says it is,
        // and a provider that never retries still reaches here: the list's owner decides.
        let usage_limit = prompt_usage_limit(&error, std::time::SystemTime::now());
        if usage_limit.is_some() || attempt == MAX_RETRIES || !retryable(&error) {
            let message = failure_text(&error, params.native_search);
            if let Some(class) = error_class(&error) {
                params.state.append(Record::RequestFailed {
                    agent: params.agent_id.to_string(),
                    class: class.to_string(),
                    message: message.chars().take(FAILURE_RECORD_CHARS).collect(),
                });
            }
            return TurnOutcome::Failed(TurnFailure {
                message,
                falls_through: falls_through(&error),
                class: error_class(&error),
                kind: failure_kind(&error),
                usage_limit,
            });
        }
        tokio::time::sleep(BACKOFF * (1 << attempt)).await;
        params.prompt = RETRY_NOTE.to_string();
    }
    unreachable!("the last attempt always returns")
}

/// One attempt of `drive`, with the model failure kept typed for the retry decision.
async fn drive_once(
    params: Drive<'_>,
    mut feed: Option<&mut DecisionFeed>,
    on_event: &mut (dyn FnMut(SessionEvent) -> Flow + Send),
) -> DriveEnd {
    let mut tool_context = ToolContext::new();
    // Every tool call must name its agent; an untagged call is refused.
    tool_context
        .insert(AgentId(params.agent_id.to_string()))
        .expect("the agent id serializes");
    // Read tools ask the context what the model accepts, so a mid-session model switch
    // takes effect on this turn without a rebuild.
    tool_context
        .insert(InputModalities(params.modalities))
        .expect("the modalities serialize");
    let runner = params
        .agent
        .prompt(user_message(
            &params.prompt,
            &params.attachments,
            &params.modalities,
        ))
        .tool_context(tool_context)
        // rig has no unbounded setting: left unset, a run makes a single model call.
        .max_turns(params.max_turns.unwrap_or(usize::MAX))
        // One call at a time, in order: the guardian judges each call before the next starts.
        .tool_concurrency(1);
    // Per turn, not at build, so `/effort` and `/model` take effect without a rebuild.
    let runner = match params.generation_options {
        Some(options) => runner.options(options),
        None => runner,
    };
    let runner = match params.max_tokens {
        Some(n) => runner.max_tokens(n),
        None => runner,
    };
    // The run adopts the span it is built under as its own agent span.
    let mut stream = params.span.in_scope(|| runner.stream());
    let mut step = ModelStep::begin(params.state, params.agent_id);

    let mut turn_calls_tool = false;
    let mut final_response = None;
    let mut failure: Option<PromptError> = None;
    let mut stopped = false;

    'run: loop {
        let item = tokio::select! {
            biased;
            Some(fed) = next_fed(&mut feed) => {
                let event = match fed {
                    Published::Decision(decision) => SessionEvent::Decision(decision),
                    Published::Preview(id, preview) => SessionEvent::ToolPreview {
                        id,
                        preview: Box::new(preview),
                    },
                };
                if on_event(event) == Flow::Stop {
                    stopped = true;
                    break 'run;
                }
                continue 'run;
            }
            item = stream.next() => item,
        };
        // A call's decision is made before its result exists, so the decisions already waiting
        // belong ahead of whatever the stream just produced.
        if let Some(feed) = feed.as_deref_mut() {
            while let Some(decision) = feed.try_next() {
                if on_event(SessionEvent::Decision(Box::new(decision))) == Flow::Stop {
                    stopped = true;
                    break 'run;
                }
            }
            while let Some((id, preview)) = feed.try_next_preview() {
                let event = SessionEvent::ToolPreview {
                    id,
                    preview: Box::new(preview),
                };
                if on_event(event) == Flow::Stop {
                    stopped = true;
                    break 'run;
                }
            }
        }
        let Some(item) = item else { break };
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        if matches!(item, MultiTurnStreamItem::StreamAssistantItem(_)) {
            step.start_pending();
        }
        let flow = match item {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(event)) => match event {
                StreamEvent::Text { text, .. } => {
                    step.note(text.chars().count());
                    on_event(SessionEvent::AssistantDelta(text))
                }
                StreamEvent::Reasoning { text, .. } => {
                    step.note(text.chars().count());
                    on_event(SessionEvent::ReasoningDelta(text))
                }
                StreamEvent::Arguments { json, .. } => {
                    step.note(json.chars().count());
                    Flow::Continue
                }
                StreamEvent::End { content, .. } => match content {
                    AssistantContent::Text(text) => on_event(SessionEvent::Assistant(text.text)),
                    AssistantContent::Reasoning(reasoning) => {
                        on_event(SessionEvent::Reasoning(reasoning.text))
                    }
                    AssistantContent::ToolCall(_) => {
                        turn_calls_tool = true;
                        Flow::Continue
                    }
                    _ => Flow::Continue,
                },
                _ => Flow::Continue,
            },
            MultiTurnStreamItem::CompletionCall(call) => {
                step.end_request();
                let mut usage = call.usage;
                if !params.priced {
                    usage.cost = None;
                }
                let source = if params.agent_id == ROOT_AGENT {
                    UsageSource::Main
                } else {
                    UsageSource::Subagent
                };
                params
                    .state
                    .record_usage(source, params.agent_id, &usage, true, &params.served);
                params.state.usage_pause().note_success();
                let flow = on_event(SessionEvent::Usage {
                    usage,
                    calls_tool: turn_calls_tool,
                });
                turn_calls_tool = false;
                flow
            }
            MultiTurnStreamItem::ToolCall { tool_call } => on_event(SessionEvent::ToolCall {
                id: tool_call.id.to_string(),
                tool: tool_call.function.name.to_string(),
                args: Value::Object(tool_call.function.arguments.clone()),
            }),
            MultiTurnStreamItem::ToolResult { tool_result } => on_event(SessionEvent::ToolResult {
                id: tool_result.call.to_string(),
                tool: tool_result.name.to_string(),
                success: !tool_result.is_error,
                text: tool_text(&tool_result.content),
            }),
            MultiTurnStreamItem::FinalResponse(response) => {
                step.close();
                final_response = Some(response);
                Flow::Continue
            }
            _ => Flow::Continue,
        };
        if flow == Flow::Stop {
            stopped = true;
            break;
        }
    }
    drop(stream);
    // Decisions made by calls that were cut short still belong in the record of the turn.
    if let Some(feed) = feed {
        while let Some(decision) = feed.try_next() {
            let _ = on_event(SessionEvent::Decision(Box::new(decision)));
        }
        while let Some((id, preview)) = feed.try_next_preview() {
            let _ = on_event(SessionEvent::ToolPreview {
                id,
                preview: Box::new(preview),
            });
        }
    }

    if stopped && failure.is_none() {
        DriveEnd::Outcome(TurnOutcome::Stopped)
    } else if let Some(e) = failure {
        if let Some(class) = error_class(&e) {
            let status = e.provider_response_status().map(|s| u64::from(s.as_u16()));
            tracing::info!(
                target: "fontaine::telemetry",
                model = params.model.as_str(),
                provider = params.provider.as_str(),
                query_source = if params.agent_id == ROOT_AGENT { "main" } else { "subagent" },
                agent.id = params.agent_id,
                error_class = class,
                status_code = status,
                "api_error"
            );
        }
        match e {
            e @ PromptError::MaxTurns { .. } => {
                DriveEnd::Outcome(TurnOutcome::MaxTurns(e.to_string()))
            }
            other => DriveEnd::Failed(Box::new(other)),
        }
    } else if let Some(response) = final_response {
        DriveEnd::Outcome(TurnOutcome::Answer(response.output()))
    } else {
        DriveEnd::Outcome(TurnOutcome::Failed(TurnFailure::new(
            "the run ended without an answer",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;
    use rig_core::error::ErrorReport;
    use rig_core::message::ImageMediaType;
    use rig_core::{ProviderError, ProviderResponseError};

    fn modalities(image: bool, pdf: bool) -> Modalities {
        let mut accepted = Modalities::default();
        accepted.text = true;
        accepted.image = image;
        accepted.pdf = pdf;
        accepted
    }

    fn png() -> Attachment {
        Attachment {
            label: "screenshot.png".into(),
            bytes: b"\x89PNG\r\n\x1a\nnot really".to_vec(),
            media: AttachmentMedia::Image(ImageMediaType::PNG),
        }
    }

    fn pdf() -> Attachment {
        Attachment {
            label: "notes.pdf".into(),
            bytes: b"%PDF-1.7 not really".to_vec(),
            media: AttachmentMedia::Pdf,
        }
    }

    fn parts(message: &Message) -> &Vec<UserContent> {
        let Message::User { content } = message else {
            panic!("a user message");
        };
        content
    }

    #[test]
    fn the_prompt_reads_as_the_first_text_part() {
        let message = user_message("hello", &[], &modalities(false, false));
        let content = parts(&message);
        assert_eq!(content.len(), 1);
        assert_eq!(content[0], UserContent::text("hello"));
    }

    #[test]
    fn an_image_attachment_becomes_an_image_part_for_a_model_that_accepts_images() {
        let message = user_message("look", &[png()], &modalities(true, false));
        let content = parts(&message);
        assert_eq!(content.len(), 2);
        let UserContent::Image(image) = &content[1] else {
            panic!("an image part, got {:?}", content[1]);
        };
        assert_eq!(image.media_type, Some(ImageMediaType::PNG));
        assert!(
            !format!("{:?}", image.data).contains("not really"),
            "base64, not raw bytes"
        );
    }

    #[test]
    fn a_pdf_attachment_becomes_a_document_part_for_a_model_that_accepts_pdfs() {
        let message = user_message("read", &[pdf()], &modalities(false, true));
        let content = parts(&message);
        assert_eq!(content.len(), 2);
        let UserContent::Document(document) = &content[1] else {
            panic!("a document part, got {:?}", content[1]);
        };
        assert_eq!(document.media_type, Some(DocumentMediaType::PDF));
    }

    #[test]
    fn an_attachment_the_model_rejects_becomes_a_text_note() {
        for (attachment, name) in [(png(), "screenshot.png"), (pdf(), "notes.pdf")] {
            let message = user_message("hey", &[attachment], &modalities(false, false));
            let content = parts(&message);
            assert_eq!(content.len(), 2, "the note replaces the part");
            let UserContent::Text(note) = &content[1] else {
                panic!("a text note, got {:?}", content[1]);
            };
            assert!(note.text.contains(name), "{}", note.text);
            assert!(note.text.contains("does not accept"), "{}", note.text);
            assert!(!note.text.contains("iVBOR"), "{}", note.text);
            assert!(!note.text.contains("JVBER"), "{}", note.text);
        }
    }

    #[test]
    fn the_modalities_gate_applies_per_attachment() {
        let message = user_message("both", &[png(), pdf()], &modalities(true, false));
        let content = parts(&message);
        assert!(matches!(content[1], UserContent::Image(_)));
        assert!(
            matches!(content[2], UserContent::Text(_)),
            "the PDF falls back"
        );
    }

    #[test]
    fn tool_text_marks_images_without_their_bytes() {
        let text = tool_text(&[
            ToolResultContent::text("header"),
            ToolResultContent::Image(rig_core::message::Image {
                data: rig_core::message::DocumentSourceKind::base64("aGk="),
                media_type: Some(ImageMediaType::PNG),
                detail: None,
                native: None,
            }),
        ]);
        assert!(text.contains("header"), "{text}");
        assert!(text.contains("[image]"), "{text}");
        assert!(!text.contains("aGk="), "{text}");
    }

    fn provider_error(status: StatusCode) -> PromptError {
        PromptError::Provider(ProviderError::ProviderResponse(ProviderResponseError::new(
            status, "boom",
        )))
    }

    #[test]
    fn throttling_and_server_errors_are_retried_client_errors_are_not() {
        assert!(retryable(&provider_error(StatusCode::TOO_MANY_REQUESTS)));
        assert!(retryable(&provider_error(
            StatusCode::INTERNAL_SERVER_ERROR
        )));
        assert!(!retryable(&provider_error(StatusCode::BAD_REQUEST)));
    }

    #[test]
    fn a_relayed_report_keeps_its_classification() {
        let error = ProviderError::ProviderResponse(ProviderResponseError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "boom",
        ));
        assert!(retryable(&PromptError::Report(ErrorReport::from(&error))));
    }

    #[test]
    fn quota_statuses_fall_through_and_other_failures_do_not() {
        for status in [402, 429, 529] {
            let status = StatusCode::from_u16(status).unwrap();
            assert!(falls_through(&provider_error(status)), "{status}");
        }
        for status in [400, 401, 500, 503] {
            let status = StatusCode::from_u16(status).unwrap();
            assert!(!falls_through(&provider_error(status)), "{status}");
        }
    }

    #[test]
    fn a_stream_error_with_only_a_code_falls_through_for_each_quota_code() {
        let by_code = |code: &str| {
            PromptError::Provider(ProviderError::ProviderResponse(
                ProviderResponseError::without_status("stream error")
                    .with_code(Some(code.to_string())),
            ))
        };
        for code in QUOTA_CODES {
            assert!(falls_through(&by_code(code)), "{code}");
        }
        assert!(!falls_through(&by_code("invalid_request_error")));
        assert!(!falls_through(&by_code("server_error")));
        // The code the body's envelope names counts too, numeric or named.
        let body = |text: &str| {
            PromptError::Provider(ProviderError::ProviderResponse(
                ProviderResponseError::without_status(text),
            ))
        };
        assert!(falls_through(&body(
            r#"{"error":{"code":429,"message":"slow down"}}"#
        )));
        assert!(falls_through(&body(
            r#"{"error":{"type":"insufficient_quota","message":"out"}}"#
        )));
        assert!(!falls_through(&body(
            r#"{"error":{"code":400,"message":"bad"}}"#
        )));
    }

    #[test]
    fn a_relayed_report_falls_through_on_its_status_or_code_only() {
        let report = |status: Option<u16>, code: Option<&str>, message: &str| {
            let mut report = ErrorReport::new(rig_core::error::ErrorKind::Provider, message);
            report.http_status = status;
            report.code = code.map(str::to_string);
            PromptError::Report(report)
        };
        assert!(falls_through(&report(Some(429), None, "x")));
        assert!(falls_through(&report(None, Some("rate_limit_error"), "x")));
        assert!(!falls_through(&report(Some(500), None, "x")));
        // A plan limit that only the text describes is not recognised: nothing matches on text.
        assert!(!falls_through(&report(
            None,
            None,
            "you have hit your usage limit"
        )));
    }

    #[test]
    fn a_claude_subscription_plan_limit_falls_through_in_the_provider_form() {
        use crate::providers::claude_subscription::{ClaudeError, ErrorKind};
        let limit = |kind| {
            PromptError::Provider(ProviderError::Request(std::sync::Arc::new(
                ClaudeError::new(kind, "the plan's limit was reached"),
            )))
        };
        assert!(falls_through(&limit(ErrorKind::Limit)));
        assert!(!falls_through(&limit(ErrorKind::Auth)));
    }

    #[test]
    fn a_cancel_and_the_run_s_own_limits_do_not_fall_through() {
        let cancelled = PromptError::Cancelled {
            chat_history: Vec::new(),
            reason: "test".to_string(),
        };
        assert!(!falls_through(&cancelled));
        let max = PromptError::MaxTurns {
            max_turns: 1,
            chat_history: Vec::new(),
            prompt: Message::user("x"),
        };
        assert!(!falls_through(&max));
    }

    #[test]
    fn a_job_error_falls_through_by_the_run_error_in_its_source_chain() {
        #[derive(Debug)]
        struct Wrapper(Box<dyn std::error::Error + Send + Sync>);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the model call failed")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self.0.as_ref())
            }
        }
        let quota = Wrapper(Box::new(provider_error(StatusCode::TOO_MANY_REQUESTS)));
        assert!(error_falls_through(&quota));
        let bad = Wrapper(Box::new(provider_error(StatusCode::BAD_REQUEST)));
        assert!(!error_falls_through(&bad));
        let io = Wrapper(Box::new(std::io::Error::other("disk")));
        assert!(!error_falls_through(&io));
    }

    #[test]
    fn a_window_code_falls_through_even_without_a_status() {
        let report = |code: &str| {
            PromptError::Report(
                ErrorReport::new(rig_core::error::ErrorKind::Request, "x").with_code(code),
            )
        };
        assert!(falls_through(&report("exceeded_current_quota_error")));
        assert!(falls_through(&report("GoUsageLimitError")));
        assert!(!falls_through(&report("invalid_request_error")));
    }

    #[test]
    fn a_cancel_is_not_retried() {
        let cancelled = PromptError::Cancelled {
            chat_history: Vec::new(),
            reason: "test".to_string(),
        };
        assert!(!retryable(&cancelled));
    }
}
