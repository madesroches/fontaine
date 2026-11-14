//! `SessionEvent` to Claude SDK messages. The translator is pure: it takes the run's events and
//! returns the lines to write, so the message shapes are tested without a stream.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::Decision;
use fontaine_core::turn::SessionEvent;
use fontaine_core::usage_ledger::Bucket;
use serde_json::{Value, json};
use uuid::Uuid;

use super::cli::{CLAUDE_CODE_PROTOCOL_VERSION, COMPAT_PROTOCOL};
use super::router::Router;
use super::wire::{
    ApiMessage, Assistant, PermissionDenied, ResultMessage, StreamEvent, SystemInit,
    UserToolResults,
};
use crate::headless::{EventSink, Outcome, ResultInfo, Subtype, session_cost};

/// A builtin's canonical Claude name; any other tool keeps its own.
pub fn wire_name(tool: &str) -> String {
    fontaine_tools::descriptor::find(tool)
        .and_then(|descriptor| descriptor.claude.first())
        .map_or_else(|| tool.to_string(), |name| (*name).to_string())
}

/// What the last message of a turn was, for the shape of an interrupted turn's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Last {
    Nothing,
    /// The model was producing its answer or had just finished it as text.
    Text,
    /// A model call asked for tools and they have not all answered.
    Tools,
    /// Every tool answered and the model has not spoken since.
    ToolResults,
}

struct Open {
    id: String,
    /// The index of the text block being streamed, once one opened.
    text_block: Option<usize>,
    blocks: usize,
}

pub struct Translator {
    session_id: String,
    model: String,
    partial: bool,
    /// The model call whose blocks are being written. Fontaine reports a call's usage before its
    /// tool calls, so the call stays open until a tool answers or the turn ends.
    open: Option<Open>,
    /// The usage of the call that finished, which its closing events carry.
    ended: Option<(&'static str, Value)>,
    tool_calls: HashMap<String, (String, Value)>,
    unanswered: usize,
    denials: Vec<Value>,
    last: Last,
    last_stop: Option<&'static str>,
    started: Instant,
    out: Vec<Value>,
}

fn usage_json(usage: &rig_core::completion::Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens.unwrap_or(0),
        "output_tokens": usage.output_tokens.unwrap_or(0),
        "cache_creation_input_tokens": usage.cache_creation_input_tokens.unwrap_or(0),
        "cache_read_input_tokens": usage.cached_input_tokens.unwrap_or(0),
        "service_tier": "standard",
    })
}

fn bucket_usage(bucket: &Bucket) -> Value {
    json!({
        "input_tokens": bucket.input_tokens,
        "output_tokens": bucket.output_tokens,
        "cache_creation_input_tokens": bucket.cache_creation_input_tokens,
        "cache_read_input_tokens": bucket.cache_read_input_tokens,
        "server_tool_use": { "web_search_requests": 0, "web_fetch_requests": 0 },
        "service_tier": "standard",
    })
}

fn bucket_object(bucket: &Bucket) -> Value {
    json!({
        "input_tokens": bucket.input_tokens,
        "output_tokens": bucket.output_tokens,
        "cache_read_input_tokens": bucket.cache_read_input_tokens,
        "cache_creation_input_tokens": bucket.cache_creation_input_tokens,
        "cost_usd": bucket.cost_usd,
    })
}

fn to_value<T: serde::Serialize>(message: &T) -> Value {
    serde_json::to_value(message).expect("a wire message serializes")
}

impl Translator {
    pub fn new(partial: bool) -> Translator {
        Translator {
            session_id: String::new(),
            model: String::new(),
            partial,
            open: None,
            ended: None,
            tool_calls: HashMap::new(),
            unanswered: 0,
            denials: Vec::new(),
            last: Last::Nothing,
            last_stop: None,
            started: Instant::now(),
            out: Vec::new(),
        }
    }

    /// The model and session a turn runs under, and the clock it is timed by.
    pub fn begin(&mut self, session_id: &str, model: &str) {
        self.session_id = session_id.to_string();
        self.model = model.to_string();
        self.started = Instant::now();
        self.last = Last::Nothing;
        self.last_stop = None;
        self.open = None;
        self.ended = None;
        self.unanswered = 0;
        self.tool_calls.clear();
    }

    pub fn take(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.out)
    }

    fn uuid() -> String {
        Uuid::new_v4().to_string()
    }

    fn stream(&mut self, event: Value) {
        if self.partial {
            self.out.push(to_value(&StreamEvent {
                kind: "stream_event",
                event,
                parent_tool_use_id: None,
                session_id: self.session_id.clone(),
                uuid: Self::uuid(),
            }));
        }
    }

    /// The message that the blocks of the current model call belong to, opened by the first.
    fn open_message(&mut self) -> &mut Open {
        if self.open.is_none() {
            let id = format!("msg_{}", Uuid::new_v4().simple());
            let model = self.model.clone();
            self.stream(json!({
                "type": "message_start",
                "message": {
                    "id": id, "type": "message", "role": "assistant", "model": model,
                    "content": [], "stop_reason": null, "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }));
            self.open = Some(Open {
                id,
                text_block: None,
                blocks: 0,
            });
        }
        self.open.as_mut().expect("opened above")
    }

    fn start_block(&mut self, block: Value) -> usize {
        let open = self.open_message();
        let index = open.blocks;
        open.blocks += 1;
        self.stream(
            json!({ "type": "content_block_start", "index": index, "content_block": block }),
        );
        index
    }

    /// One content block as its own `assistant` message, as Claude Code writes them.
    fn assistant(&mut self, block: Value) {
        let id = self.open_message().id.clone();
        let usage = self.ended.as_ref().map_or_else(
            || json!({ "input_tokens": 0, "output_tokens": 0 }),
            |(_, u)| u.clone(),
        );
        self.out.push(to_value(&Assistant {
            kind: "assistant",
            message: ApiMessage {
                id,
                kind: "message",
                role: "assistant",
                model: self.model.clone(),
                content: vec![block],
                stop_reason: None,
                stop_sequence: None,
                usage,
            },
            parent_tool_use_id: None,
            session_id: self.session_id.clone(),
            uuid: Self::uuid(),
        }));
    }

    /// The model call is over, tool calls included: its raw events end.
    fn close_message(&mut self) {
        if self.open.is_none() {
            return;
        }
        let (stop, usage) = self
            .ended
            .take()
            .unwrap_or(("tool_use", json!({ "output_tokens": 0 })));
        self.stream(json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop, "stop_sequence": null },
            "usage": { "output_tokens": usage["output_tokens"] },
        }));
        self.stream(json!({ "type": "message_stop" }));
        self.open = None;
    }

    fn event(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::AssistantDelta(text) => {
                self.last = Last::Text;
                if self.partial {
                    let index = match self.open_message().text_block {
                        Some(index) => index,
                        None => {
                            let index = self.start_block(json!({ "type": "text", "text": "" }));
                            self.open_message().text_block = Some(index);
                            index
                        }
                    };
                    self.stream(json!({
                        "type": "content_block_delta", "index": index,
                        "delta": { "type": "text_delta", "text": text },
                    }));
                }
            }
            SessionEvent::Assistant(text) => {
                self.last = Last::Text;
                if self.partial {
                    let index = match self.open_message().text_block.take() {
                        Some(index) => index,
                        None => {
                            // A provider that does not stream: the whole text is one delta.
                            let index = self.start_block(json!({ "type": "text", "text": "" }));
                            self.stream(json!({
                                "type": "content_block_delta", "index": index,
                                "delta": { "type": "text_delta", "text": text },
                            }));
                            index
                        }
                    };
                    self.stream(json!({ "type": "content_block_stop", "index": index }));
                }
                self.assistant(json!({ "type": "text", "text": text }));
            }
            SessionEvent::ReasoningDelta(_) | SessionEvent::Reasoning(_) => {}
            SessionEvent::ToolCall { id, tool, args } => {
                self.last = Last::Tools;
                let name = wire_name(tool);
                if self.partial {
                    let index = self.start_block(json!({
                        "type": "tool_use", "id": id, "name": name, "input": {},
                    }));
                    self.stream(json!({
                        "type": "content_block_delta", "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": args.to_string() },
                    }));
                    self.stream(json!({ "type": "content_block_stop", "index": index }));
                }
                self.assistant(json!({
                    "type": "tool_use", "id": id, "name": name, "input": args,
                }));
                self.tool_calls.insert(id.clone(), (name, args.clone()));
                self.unanswered += 1;
            }
            SessionEvent::Usage { usage, calls_tool } => {
                let stop = if *calls_tool { "tool_use" } else { "end_turn" };
                self.last_stop = Some(stop);
                self.ended = Some((stop, usage_json(usage)));
                // With no tool calls to follow, the call ends here.
                if !calls_tool {
                    self.close_message();
                }
            }
            SessionEvent::Decision(record) => {
                if record.decision == Decision::Deny {
                    let id = record.tool_use_id.clone().unwrap_or_default();
                    let (name, input) = self
                        .tool_calls
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| (wire_name(&record.tool), Value::Null));
                    let message = record.explanation.clone().unwrap_or_default();
                    self.out.push(to_value(&PermissionDenied {
                        kind: "system",
                        subtype: "permission_denied",
                        tool_name: name.clone(),
                        tool_use_id: id.clone(),
                        tool_input: input.clone(),
                        message,
                        session_id: self.session_id.clone(),
                        uuid: Self::uuid(),
                    }));
                    self.denials.push(json!({
                        "tool_name": name, "tool_use_id": id, "tool_input": input,
                    }));
                }
            }
            SessionEvent::ToolResult {
                id, success, text, ..
            } => {
                // A tool answering means the model call that asked for it is complete.
                self.close_message();
                self.unanswered = self.unanswered.saturating_sub(1);
                self.last = if self.unanswered == 0 {
                    Last::ToolResults
                } else {
                    Last::Tools
                };
                self.out.push(to_value(&UserToolResults {
                    kind: "user",
                    message: json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result", "tool_use_id": id,
                            "content": text, "is_error": !success,
                        }],
                    }),
                    parent_tool_use_id: None,
                    session_id: self.session_id.clone(),
                    uuid: Self::uuid(),
                }));
            }
            SessionEvent::ToolPreview { .. } => {}
        }
    }

    /// The turn ended: a model call still open ends with it.
    fn flush(&mut self) {
        self.close_message();
    }
}

/// How a turn that ended is reported.
struct Finish {
    subtype: &'static str,
    is_error: bool,
    terminal: Option<&'static str>,
    errors: Vec<String>,
}

fn finish_of(translator: &Translator, outcome: &Outcome, interrupted: bool) -> Finish {
    if interrupted {
        // Claude Code reports an interrupt after a finished answer, or after every tool answered
        // with the model done, as a success; anything else is an aborted turn.
        let done = match translator.last {
            Last::Text => true,
            Last::ToolResults => translator.last_stop == Some("end_turn"),
            _ => false,
        };
        if done {
            return Finish {
                subtype: "success",
                is_error: false,
                terminal: Some("completed"),
                errors: Vec::new(),
            };
        }
        let terminal = match translator.last {
            Last::Tools | Last::ToolResults => "aborted_tools",
            _ => "aborted_streaming",
        };
        return Finish {
            subtype: "error_during_execution",
            is_error: true,
            terminal: Some(terminal),
            errors: vec!["[ede_diagnostic] the turn was interrupted by the host".to_string()],
        };
    }
    let subtype = outcome.subtype.name();
    if outcome.subtype == Subtype::Success {
        return Finish {
            subtype,
            is_error: false,
            terminal: Some("completed"),
            errors: Vec::new(),
        };
    }
    Finish {
        subtype,
        is_error: true,
        terminal: None,
        errors: outcome.error.iter().cloned().collect(),
    }
}

/// The per-model usage of the session, keyed by model reference.
fn model_usage(runtime: &AgentRuntime) -> Value {
    let mut totals: std::collections::BTreeMap<String, Bucket> = Default::default();
    for (served, bucket) in runtime.state.usage_ledger().by_model() {
        totals.entry(served.model).or_default().merge(&bucket);
    }
    Value::Object(
        totals
            .into_iter()
            .map(|(model, bucket)| {
                (
                    model,
                    json!({
                        "inputTokens": bucket.input_tokens,
                        "outputTokens": bucket.output_tokens,
                        "cacheReadInputTokens": bucket.cache_read_input_tokens,
                        "cacheCreationInputTokens": bucket.cache_creation_input_tokens,
                        "webSearchRequests": 0,
                        "costUSD": bucket.cost_usd.unwrap_or(0.0),
                        "contextWindow": 0,
                        "maxOutputTokens": 0,
                    }),
                )
            })
            .collect(),
    )
}

/// Writes the Claude SDK messages of a run.
pub struct ClaudeSink {
    router: Arc<Router>,
    translator: Translator,
}

impl ClaudeSink {
    pub fn new(router: Arc<Router>, partial: bool) -> ClaudeSink {
        ClaudeSink {
            router,
            translator: Translator::new(partial),
        }
    }

    fn flush(&mut self) {
        for line in self.translator.take() {
            self.router.send(line);
        }
    }

    fn result_message(&mut self, info: &ResultInfo<'_>) -> ResultMessage {
        let finish = finish_of(&self.translator, info.outcome, info.interrupted);
        let usage = info.usage;
        let cost = session_cost(
            usage.main.cost_usd,
            usage.guardian.cost_usd,
            usage.goal.cost_usd,
            usage.subagents.cost_usd,
        );
        let ms = info.outcome.started.elapsed().as_millis() as u64;
        ResultMessage {
            kind: "result",
            subtype: finish.subtype,
            is_error: finish.is_error,
            duration_ms: ms,
            duration_api_ms: ms,
            num_turns: usage.main.calls,
            result: if finish.is_error {
                String::new()
            } else {
                info.outcome.text.clone()
            },
            stop_reason: (!finish.is_error).then_some("end_turn"),
            session_id: info.session_id.to_string(),
            total_cost_usd: cost,
            usage: bucket_usage(&usage.main),
            model_usage: model_usage(info.runtime),
            permission_denials: std::mem::take(&mut self.translator.denials),
            structured_output: info.outcome.structured.clone(),
            terminal_reason: finish.terminal,
            errors: finish.errors,
            uuid: Translator::uuid(),
            fontaine_usage: json!({
                "main": bucket_object(&usage.main),
                "guardian": bucket_object(&usage.guardian),
                "subagents": bucket_object(&usage.subagents),
                "goal": bucket_object(&usage.goal),
            }),
        }
    }
}

impl EventSink for ClaudeSink {
    fn session_start(&mut self, _runtime: &AgentRuntime, _data_dir: &Path) {}

    fn turn_start(&mut self, runtime: &AgentRuntime, _data_dir: &Path) {
        let model = runtime.models.main.reference.to_string();
        self.translator.begin(&runtime.session.id, &model);
        let mcp_servers = runtime
            .mcp_states()
            .into_iter()
            .map(|(name, state)| {
                let status = match state {
                    fontaine_core::agent::McpServerState::Connected { .. } => "connected",
                    fontaine_core::agent::McpServerState::Reconnecting { .. } => "pending",
                    fontaine_core::agent::McpServerState::Failed(_) => "failed",
                };
                json!({ "name": name, "status": status })
            })
            .collect();
        let tools = runtime
            .tool_names()
            .iter()
            .map(|tool| wire_name(tool))
            .collect();
        let init = SystemInit {
            kind: "system",
            subtype: "init",
            cwd: runtime.primary_root().display().to_string(),
            session_id: runtime.session.id.clone(),
            tools,
            mcp_servers,
            model,
            permission_mode: runtime.state.mode().as_str().to_string(),
            slash_commands: Vec::new(),
            api_key_source: "none",
            claude_code_version: CLAUDE_CODE_PROTOCOL_VERSION,
            output_style: "default",
            agents: runtime
                .agent_types()
                .names()
                .into_iter()
                .map(str::to_string)
                .collect(),
            skills: Vec::new(),
            plugins: Vec::new(),
            uuid: Translator::uuid(),
            fontaine_version: crate::VERSION,
            compat_protocol: COMPAT_PROTOCOL,
        };
        self.router.send(to_value(&init));
    }

    fn event(&mut self, _session_id: &str, event: &SessionEvent) {
        self.translator.event(event);
        self.flush();
    }

    fn wants_account_usage(&self) -> bool {
        false
    }

    fn result(&mut self, info: &ResultInfo<'_>) {
        self.translator.flush();
        self.flush();
        let message = self.result_message(info);
        self.router.send(to_value(&message));
    }

    fn usage_observer(&self, _session_id: &str) -> Option<crate::headless::PauseObserver> {
        None
    }

    fn input_failed(&mut self, session_id: &str, message: &str) {
        let result = ResultMessage {
            kind: "result",
            subtype: "error_during_execution",
            is_error: true,
            duration_ms: 0,
            duration_api_ms: 0,
            num_turns: 0,
            result: String::new(),
            stop_reason: None,
            session_id: session_id.to_string(),
            total_cost_usd: None,
            usage: bucket_usage(&Bucket::default()),
            model_usage: json!({}),
            permission_denials: Vec::new(),
            structured_output: None,
            terminal_reason: None,
            errors: vec![message.to_string()],
            uuid: Translator::uuid(),
            fontaine_usage: json!({}),
        };
        self.router.send(to_value(&result));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::Usage;

    fn usage(input: u64, output: u64) -> Usage {
        let mut usage = Usage::new();
        usage.input_tokens = Some(input);
        usage.output_tokens = Some(output);
        usage
    }

    fn translate(partial: bool, events: &[SessionEvent]) -> Vec<Value> {
        let mut t = Translator::new(partial);
        t.begin("sid", "acct/model");
        for event in events {
            t.event(event);
        }
        t.take()
    }

    fn call(id: &str, tool: &str) -> SessionEvent {
        SessionEvent::ToolCall {
            id: id.into(),
            tool: tool.into(),
            args: json!({"command": "ls"}),
        }
    }

    #[test]
    fn builtins_are_named_as_claude_names_them_and_others_keep_theirs() {
        assert_eq!(wire_name("shell"), "Bash");
        assert_eq!(wire_name("read"), "Read");
        assert_eq!(wire_name("mcp__srv__tool"), "mcp__srv__tool");
        assert_eq!(wire_name("something_new"), "something_new");
    }

    #[test]
    fn each_content_block_is_an_assistant_message_with_a_null_parent() {
        let out = translate(
            false,
            &[
                SessionEvent::Assistant("hello".into()),
                SessionEvent::Usage {
                    usage: usage(3, 4),
                    calls_tool: true,
                },
                call("t1", "shell"),
            ],
        );
        // Fontaine reports a call's usage before its tool calls; the blocks still go out as
        // they come, as Claude Code writes them.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["type"], "assistant");
        assert!(out[0]["parent_tool_use_id"].is_null());
        assert_eq!(out[0]["session_id"], "sid");
        assert_eq!(out[0]["message"]["model"], "acct/model");
        assert_eq!(out[0]["message"]["content"][0]["type"], "text");
        let tool = &out[1];
        assert_eq!(tool["message"]["content"][0]["name"], "Bash");
        // Fontaine's own argument schema, never rewritten.
        assert_eq!(tool["message"]["content"][0]["input"]["command"], "ls");
        assert_eq!(tool["message"]["usage"]["input_tokens"], 3);
        // One model call, one message id.
        assert_eq!(out[0]["message"]["id"], tool["message"]["id"]);
    }

    #[test]
    fn a_tool_result_is_a_user_message_with_a_tool_result_block() {
        let out = translate(
            false,
            &[
                SessionEvent::Usage {
                    usage: usage(1, 1),
                    calls_tool: true,
                },
                call("t1", "shell"),
                SessionEvent::ToolResult {
                    id: "t1".into(),
                    tool: "shell".into(),
                    success: false,
                    text: "boom".into(),
                },
            ],
        );
        let user = out.last().unwrap();
        assert_eq!(user["type"], "user");
        let block = &user["message"]["content"][0];
        assert_eq!(block["tool_use_id"], "t1");
        assert_eq!(block["is_error"], true);
        assert_eq!(block["content"], "boom");
        assert!(user["parent_tool_use_id"].is_null());
    }

    #[test]
    fn partial_messages_are_wrapped_as_anthropic_raw_events() {
        let out = translate(
            true,
            &[
                SessionEvent::AssistantDelta("he".into()),
                SessionEvent::AssistantDelta("llo".into()),
                SessionEvent::Assistant("hello".into()),
                SessionEvent::Usage {
                    usage: usage(1, 2),
                    calls_tool: true,
                },
                call("t1", "read"),
                SessionEvent::ToolResult {
                    id: "t1".into(),
                    tool: "read".into(),
                    success: true,
                    text: "x".into(),
                },
            ],
        );
        let kinds: Vec<&str> = out
            .iter()
            .filter(|m| m["type"] == "stream_event")
            .map(|m| m["event"]["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let args = out
            .iter()
            .find(|m| m["event"]["delta"]["type"] == "input_json_delta")
            .unwrap();
        assert!(
            args["event"]["delta"]["partial_json"]
                .as_str()
                .unwrap()
                .contains("command")
        );
        let stop = out
            .iter()
            .find(|m| m["event"]["type"] == "message_delta")
            .unwrap();
        assert_eq!(stop["event"]["delta"]["stop_reason"], "tool_use");
        assert_eq!(out.last().unwrap()["type"], "user");
    }

    #[test]
    fn no_stream_events_without_the_flag() {
        let out = translate(
            false,
            &[
                SessionEvent::AssistantDelta("x".into()),
                SessionEvent::Assistant("x".into()),
                SessionEvent::Usage {
                    usage: usage(1, 1),
                    calls_tool: false,
                },
            ],
        );
        assert!(out.iter().all(|m| m["type"] != "stream_event"));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_text_only_call_ends_its_raw_events_at_its_usage() {
        let out = translate(
            true,
            &[
                SessionEvent::Assistant("x".into()),
                SessionEvent::Usage {
                    usage: usage(1, 1),
                    calls_tool: false,
                },
            ],
        );
        assert_eq!(out.last().unwrap()["event"]["type"], "message_stop");
        let delta = out
            .iter()
            .find(|m| m["event"]["type"] == "message_delta")
            .unwrap();
        assert_eq!(delta["event"]["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn a_gate_deny_is_a_permission_denied_message_and_is_kept_for_the_result() {
        let mut t = Translator::new(false);
        t.begin("sid", "m");
        t.event(&call("t1", "shell"));
        let mut record = fontaine_core::audit::DecisionRecord::new(
            "sid".into(),
            "root".into(),
            "shell".into(),
            "d".into(),
            fontaine_core::tier::Tier::DenyRule,
            Decision::Deny,
            "manual".into(),
            true,
        );
        record.tool_use_id = Some("t1".into());
        record.explanation = Some("denied by rule".into());
        t.event(&SessionEvent::Decision(Box::new(record)));
        let out = t.take();
        let denied = out
            .iter()
            .find(|m| m["subtype"] == "permission_denied")
            .unwrap();
        assert_eq!(denied["tool_name"], "Bash");
        assert_eq!(denied["tool_use_id"], "t1");
        assert_eq!(t.denials.len(), 1);
        assert_eq!(t.denials[0]["tool_input"]["command"], "ls");
    }

    #[test]
    fn an_interrupt_after_text_is_a_success_and_one_mid_tool_is_an_aborted_turn() {
        let mut t = Translator::new(false);
        t.begin("sid", "m");
        t.event(&SessionEvent::Assistant("done".into()));
        let outcome = crate::headless::interrupted_for_tests();
        let finish = finish_of(&t, &outcome, true);
        assert_eq!(finish.subtype, "success");

        let mut t = Translator::new(false);
        t.begin("sid", "m");
        t.event(&call("t1", "shell"));
        let finish = finish_of(&t, &outcome, true);
        assert_eq!(finish.subtype, "error_during_execution");
        assert_eq!(finish.terminal, Some("aborted_tools"));
        assert!(finish.errors[0].starts_with("[ede_diagnostic]"));

        let mut t = Translator::new(false);
        t.begin("sid", "m");
        t.event(&SessionEvent::AssistantDelta("par".into()));
        // Streaming text that never finished is an answer in progress.
        let finish = finish_of(&t, &outcome, true);
        assert_eq!(finish.subtype, "success");
    }

    #[test]
    fn a_turn_starts_without_the_last_turns_tool_calls() {
        let mut t = Translator::new(false);
        t.begin("sid", "m");
        t.event(&call("t1", "shell"));
        assert_eq!((t.unanswered, t.tool_calls.len()), (1, 1));
        t.begin("sid", "m");
        assert_eq!((t.unanswered, t.tool_calls.len()), (0, 0));
    }

    #[test]
    fn the_unpriced_cost_is_null() {
        assert_eq!(session_cost(None, None, None, None), None);
    }
}
