//! One user message run to its end. The runner drives the agent's stream and reports what
//! happens as typed session events to a consumer, which decides how to show them and may stop
//! the run between model calls. Headless and the TUI both run on it.

use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::completion::PromptError;
use rig_agent::prelude::*;
use rig_core::completion::Usage;
use rig_core::message::{AssistantContent, ToolResultContent};
use rig_core::streaming::{Item, StreamEvent};
use rig_core::tool::ToolContext;
use serde_json::Value;
use uuid::Uuid;

use crate::audit::DecisionRecord;
use crate::gate::{DecisionFeed, SessionState, spend_of};

/// Without a limit from the caller, a turn is still bounded so a looping model cannot run
/// forever.
pub const DEFAULT_MAX_TURNS: usize = 1000;

#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// A fragment of the answer as it streams.
    AssistantDelta(String),
    /// A complete piece of assistant text.
    Assistant(String),
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
    Failed(String),
}

pub struct TurnParams<'a> {
    pub agent: &'a Agent,
    pub state: &'a SessionState,
    pub prompt: String,
    pub max_turns: usize,
    /// Whether the cost the model's calls report is counted; see `ModelHandle::catalog_billed`.
    pub priced: bool,
    /// The model's id and provider, for the `api_error` event.
    pub model: String,
    pub provider: String,
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
            max_turns: params.max_turns,
            priced: params.priced,
            model: params.model,
            provider: params.provider,
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

/// One agent's run: whose it is, what it is asked and under which span it runs.
pub struct Drive<'a> {
    pub agent: &'a Agent,
    pub state: &'a SessionState,
    pub agent_id: &'a str,
    pub prompt: String,
    pub max_turns: usize,
    /// Whether the cost the model's calls report is counted. An unpriced model's is dropped
    /// here, where usage enters, because rig attaches it inside the wire.
    pub priced: bool,
    /// The model's id and provider, for the `api_error` event.
    pub model: String,
    pub provider: String,
    /// The run adopts this span as its own agent span.
    pub span: tracing::Span,
}

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
fn failure_text(error: &PromptError) -> String {
    let mut text = error.to_string();
    let rejected_search = error
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

async fn next_decision(feed: &mut Option<&mut DecisionFeed>) -> Option<DecisionRecord> {
    match feed {
        Some(feed) => feed.next().await,
        None => std::future::pending().await,
    }
}

/// Drives one agent's stream, reporting what happens to `on_event`. The root's run also
/// forwards the decisions the pipeline makes for the whole tree through `feed`; a subagent's
/// run has none.
pub async fn drive(
    params: Drive<'_>,
    mut feed: Option<&mut DecisionFeed>,
    on_event: &mut (dyn FnMut(SessionEvent) -> Flow + Send),
) -> TurnOutcome {
    let mut tool_context = ToolContext::new();
    // Every tool call must name its agent; an untagged call is refused.
    tool_context
        .insert(AgentId(params.agent_id.to_string()))
        .expect("the agent id serializes");
    let runner = params
        .agent
        .prompt(rig_core::message::Message::user(params.prompt))
        .tool_context(tool_context)
        .max_turns(params.max_turns)
        // One call at a time, in order: the guardian judges each call before the next starts.
        .tool_concurrency(1);
    // The run adopts the span it is built under as its own agent span.
    let mut stream = params.span.in_scope(|| runner.stream());

    let mut turn_calls_tool = false;
    let mut final_response = None;
    let mut failure: Option<PromptError> = None;
    let mut stopped = false;

    'run: loop {
        let item = tokio::select! {
            biased;
            Some(decision) = next_decision(&mut feed) => {
                if on_event(SessionEvent::Decision(Box::new(decision))) == Flow::Stop {
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
        }
        let Some(item) = item else { break };
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let flow = match item {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(event)) => match event {
                StreamEvent::Text { text, .. } => on_event(SessionEvent::AssistantDelta(text)),
                StreamEvent::End { content, .. } => match content {
                    AssistantContent::Text(text) => on_event(SessionEvent::Assistant(text.text)),
                    AssistantContent::ToolCall(_) => {
                        turn_calls_tool = true;
                        Flow::Continue
                    }
                    _ => Flow::Continue,
                },
                _ => Flow::Continue,
            },
            MultiTurnStreamItem::CompletionCall(call) => {
                let mut usage = call.usage;
                if !params.priced {
                    usage.cost = None;
                }
                let spend = spend_of(&usage);
                params.state.agents().debit(params.agent_id, &spend);
                if params.agent_id != ROOT_AGENT {
                    params.state.add_subagent_usage(&usage);
                }
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
    }

    if stopped && failure.is_none() {
        TurnOutcome::Stopped
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
            e @ PromptError::MaxTurns { .. } => TurnOutcome::MaxTurns(e.to_string()),
            other => TurnOutcome::Failed(failure_text(&other)),
        }
    } else if let Some(response) = final_response {
        TurnOutcome::Answer(response.output())
    } else {
        TurnOutcome::Failed("the run ended without an answer".to_string())
    }
}
