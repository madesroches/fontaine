//! Anthropic's own deferred tool loading, applied to the encoded Messages body. Every deferred
//! MCP tool rides in `tools[]` once, marked `defer_loading`, and a `tool_search` result goes out
//! as `tool_reference` blocks, so loading a tool never changes the prefix the cache is keyed on.
//! rig passes `defer_loading` through only on raw `additional_params` tools and does not model
//! `tool_reference`, so this patches the body after rig encodes it, as `CacheWire` does for
//! system markers; rig's own bytes for the tools stay as they are.

use std::collections::{HashMap, HashSet};

use rig_core::completion::CompletionRequest;
use rig_core::error::EncodeError;
use rig_core::operation::Completion;
use rig_core::wire::{Body, Descriptor, Encoded, Mode, Wire};
use serde_json::{Value, json};

use super::warn::Warn;
use crate::tools::mcp::{Deferral, TOOL_SEARCH};
use crate::tools::tool_search::loaded_names;

#[derive(Clone)]
pub(super) struct DeferWire<W> {
    inner: W,
    deferral: Deferral,
    /// The calling agent's decision from its account and model; the session-wide setting and
    /// the retry state are read from `deferral` on each request.
    route: bool,
    warn: Warn,
}

impl<W> DeferWire<W> {
    pub(super) fn new(inner: W, deferral: Deferral, route: bool, warn: Warn) -> Self {
        DeferWire {
            inner,
            deferral,
            route,
            warn,
        }
    }
}

fn tool_names(body: &Value) -> Vec<&str> {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name")?.as_str())
        .collect()
}

fn deferred(tool: &Value) -> bool {
    tool.get("defer_loading") == Some(&Value::Bool(true))
}

/// Marks every registered MCP tool deferred. A cache marker rig put on one of them moves to the
/// last tool left non-deferred, because the API refuses a marker on a deferred tool.
fn defer_tools(body: &mut Value, deferral: &Deferral) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    let mut moved = None;
    for tool in tools.iter_mut().filter_map(Value::as_object_mut) {
        let registered = tool
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| deferral.is_registered(name));
        if registered {
            tool.insert("defer_loading".to_owned(), Value::Bool(true));
            if let Some(marker) = tool.remove("cache_control") {
                moved = Some(marker);
            }
        }
    }
    let Some(marker) = moved else {
        return;
    };
    if let Some(last) = tools
        .iter_mut()
        .rfind(|tool| !deferred(tool))
        .and_then(Value::as_object_mut)
    {
        last.entry("cache_control").or_insert(marker);
    }
}

/// The `tool_use` ids in the request that called `tool_search`.
fn search_calls(messages: &[Value]) -> HashSet<String> {
    messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_use" && block["name"] == TOOL_SEARCH)
        .filter_map(|block| block["id"].as_str().map(str::to_owned))
        .collect()
}

/// The text of a `tool_result` whose content is exactly one text block; any other shape stays
/// as it is.
fn sole_text(content: &Value) -> Option<&str> {
    let [block] = content.as_array()?.as_slice() else {
        return None;
    };
    if block["type"] != "text" {
        return None;
    }
    block["text"].as_str()
}

/// Rewrites each `tool_search` result that loaded tools into `tool_reference` blocks. A name the
/// request no longer defines is dropped, since the API rejects a reference without a definition;
/// a result left with none stays text.
fn reference_results(body: &mut Value, defined: &HashSet<String>) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let calls = search_calls(messages);
    if calls.is_empty() {
        return;
    }
    let mut rewrites: HashMap<(usize, usize), Vec<String>> = HashMap::new();
    for (m, message) in messages.iter().enumerate() {
        if message["role"] != "user" {
            continue;
        }
        let Some(blocks) = message["content"].as_array() else {
            continue;
        };
        for (b, block) in blocks.iter().enumerate() {
            if block["type"] != "tool_result"
                || !block["tool_use_id"]
                    .as_str()
                    .is_some_and(|id| calls.contains(id))
            {
                continue;
            }
            let Some(names) = sole_text(&block["content"]).and_then(loaded_names) else {
                continue;
            };
            let kept: Vec<String> = names
                .into_iter()
                .filter(|name| defined.contains(name))
                .collect();
            if !kept.is_empty() {
                rewrites.insert((m, b), kept);
            }
        }
    }
    for ((m, b), names) in rewrites {
        let references: Vec<Value> = names
            .into_iter()
            .map(|name| json!({"type": "tool_reference", "tool_name": name}))
            .collect();
        if let Some(block) = messages[m]["content"][b].as_object_mut() {
            block.insert("content".to_owned(), Value::Array(references));
        }
    }
}

/// Applies the native route to an encoded body; whether it changed anything.
fn rewrite(body: &mut Value, deferral: &Deferral) -> bool {
    if !tool_names(body).contains(&TOOL_SEARCH) {
        return false;
    }
    defer_tools(body, deferral);
    let defined: HashSet<String> = tool_names(body).into_iter().map(str::to_owned).collect();
    reference_results(body, &defined);
    true
}

impl<W> Wire for DeferWire<W>
where
    W: Wire<Op = Completion, Payload = Encoded>,
{
    type Op = Completion;
    type Payload = Encoded;
    type Frame = W::Frame;
    type Event = W::Event;
    type Decoder<'id> = W::Decoder<'id>;
    type Reassembler = W::Reassembler;

    fn describe(&self) -> Descriptor<'_> {
        self.inner.describe()
    }

    fn encode(&self, request: CompletionRequest, mode: Mode) -> Result<Encoded, EncodeError> {
        let mut encoded = self.inner.encode(request, mode)?;
        if !(self.route && self.deferral.native_allowed()) {
            return Ok(encoded);
        }
        if let Body::Bytes(bytes) = encoded.request.body_mut() {
            match serde_json::from_slice::<Value>(bytes) {
                Ok(mut body) => {
                    if rewrite(&mut body, &self.deferral) {
                        *bytes = serde_json::to_vec(&body).expect("a parsed JSON value serializes");
                    }
                }
                Err(error) => {
                    self.warn.emit(&format!(
                        "the request body is not JSON; deferred tools are sent as usual: {}",
                        crate::tools::error_chain::chain(&error)
                    ));
                }
            }
        }
        Ok(encoded)
    }

    fn decoder<'id>(&self) -> Self::Decoder<'id> {
        self.inner.decoder()
    }

    fn reassembler(&self) -> Self::Reassembler {
        self.inner.reassembler()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::DeferMode;
    use rig_core::completion::{Message, ToolDefinition};
    use rig_core::message::{
        AssistantContent, AssistantMessage, CallId, ToolCall, ToolFunction, ToolName, ToolResult,
        ToolResultContent, UserContent,
    };
    use rig_core::providers::anthropic::{AnthropicConfig, Messages};
    use rig_core::wire::Operation;

    fn name(name: &str) -> ToolName {
        ToolName::new(name).expect("a test tool name")
    }

    fn def(tool: &str) -> ToolDefinition {
        ToolDefinition::new(name(tool), "a stub", json!({"type": "object"}))
    }

    fn deferral() -> Deferral {
        let deferral = Deferral::default();
        deferral.set_policy(DeferMode::On, None);
        let connection = deferral.begin_connection();
        deferral.record("gh", connection, vec![def("mcp__gh__a"), def("mcp__gh__b")]);
        deferral
    }

    fn wire(deferral: &Deferral, route: bool) -> DeferWire<Messages> {
        let wire = AnthropicConfig::new("sk-test")
            .completion("claude-sonnet-4-5")
            .with_prompt_caching();
        DeferWire::new(wire, deferral.clone(), route, Warn::to(|_| {}))
    }

    impl DeferWire<Messages> {
        /// The same wire with the native route off: what rig alone encodes.
        fn inner_only(&self) -> DeferWire<Messages> {
            DeferWire {
                route: false,
                ..self.clone()
            }
        }
    }

    fn sent(wire: &DeferWire<Messages>, request: CompletionRequest) -> Value {
        let request = Completion::prepare(request, &wire.describe()).expect("a valid request");
        let encoded = wire.encode(request, Mode::Unary).expect("encodable");
        let Body::Bytes(bytes) = encoded.request.body() else {
            panic!("a Messages body is JSON");
        };
        serde_json::from_slice(bytes).expect("JSON")
    }

    /// A conversation in which `tool` was called as `c1` and answered with `text`.
    fn answered(tool: &str, text: &str) -> Vec<Message> {
        let call = ToolCall::from_wire("c1", ToolFunction::new(name(tool), json!({"query": "a"})));
        let result = ToolResult {
            call: CallId::from_wire("c1"),
            name: name(tool),
            content: vec![ToolResultContent::text(text)],
            is_error: false,
        };
        vec![
            Message::user("find it"),
            Message::Assistant(AssistantMessage::new(vec![AssistantContent::ToolCall(
                call,
            )])),
            Message::User {
                content: vec![UserContent::ToolResult(result)],
            },
        ]
    }

    fn request(tools: &[&str], history: Vec<Message>) -> CompletionRequest {
        let mut request = CompletionRequest::from(history).max_tokens(16);
        request.tools = tools
            .iter()
            .map(|tool| std::sync::Arc::new(def(tool)))
            .collect();
        request
    }

    fn plain(tools: &[&str]) -> CompletionRequest {
        request(tools, vec![Message::user("hi")])
    }

    fn tool<'a>(body: &'a Value, name: &str) -> &'a Value {
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name} in {body}"))
    }

    const LOADED: &str = "Loaded 1 tool(s); call them by name from your next turn.\n[\n  {\n    \"name\": \"mcp__gh__a\"\n  }\n]";

    #[test]
    fn only_deferred_mcp_tools_are_marked() {
        let wire = wire(&deferral(), true);
        let body = sent(
            &wire,
            plain(&["read", "mcp__gh__a", TOOL_SEARCH, "mcp__gh__b"]),
        );
        assert_eq!(tool(&body, "mcp__gh__a")["defer_loading"], true);
        assert_eq!(tool(&body, "mcp__gh__b")["defer_loading"], true);
        assert!(tool(&body, TOOL_SEARCH).get("defer_loading").is_none());
        assert!(tool(&body, "read").get("defer_loading").is_none());
    }

    #[test]
    fn a_cache_marker_on_a_deferred_tool_moves_to_the_last_non_deferred_one() {
        let wire = wire(&deferral(), true);
        let tools = ["read", TOOL_SEARCH, "mcp__gh__a", "mcp__gh__b"];
        // rig marks the last tool, which is a deferred one here.
        let unpatched = sent(&wire.inner_only(), plain(&tools));
        assert!(
            tool(&unpatched, "mcp__gh__b")
                .get("cache_control")
                .is_some()
        );
        let body = sent(&wire, plain(&tools));
        let marked: Vec<&Value> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t.get("cache_control").is_some())
            .collect();
        assert_eq!(marked.len(), 1, "{body}");
        assert_eq!(marked[0]["name"], TOOL_SEARCH);
        assert!(marked[0].get("defer_loading").is_none());
    }

    #[test]
    fn a_request_without_tool_search_is_left_as_rig_encoded_it() {
        let wire = wire(&deferral(), true);
        let body = sent(&wire, plain(&["read", "mcp__gh__a"]));
        assert_eq!(
            body,
            sent(&wire.inner_only(), plain(&["read", "mcp__gh__a"]))
        );
        assert!(!body.to_string().contains("defer_loading"));
    }

    #[test]
    fn a_non_native_route_or_a_retry_leaves_the_body_untouched() {
        let deferral = deferral();
        let tools = ["read", TOOL_SEARCH, "mcp__gh__a"];
        let history = || answered(TOOL_SEARCH, LOADED);
        let off = wire(&deferral, false);
        let body = sent(&off, request(&tools, history()));
        assert_eq!(body, sent(&off.inner_only(), request(&tools, history())));
        deferral.disable_native();
        let retried = wire(&deferral, true);
        let body = sent(&retried, request(&tools, history()));
        assert_eq!(
            body,
            sent(&retried.inner_only(), request(&tools, history()))
        );
        assert!(!body.to_string().contains("defer_loading"));
        assert!(!body.to_string().contains("tool_reference"));
    }

    #[test]
    fn a_tool_search_result_becomes_tool_references() {
        let wire = wire(&deferral(), true);
        let body = sent(
            &wire,
            request(
                &["read", TOOL_SEARCH, "mcp__gh__a"],
                answered(TOOL_SEARCH, LOADED),
            ),
        );
        let result = &body["messages"][2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(
            result["content"],
            json!([{"type": "tool_reference", "tool_name": "mcp__gh__a"}])
        );
    }

    #[test]
    fn a_result_with_a_note_or_a_miss_stays_text() {
        let wire = wire(&deferral(), true);
        for text in [
            format!("{LOADED}\n\n[a hook note]"),
            "No deferred tool matches `zebra`. 2 tools are deferred.".to_string(),
        ] {
            let body = sent(
                &wire,
                request(
                    &["read", TOOL_SEARCH, "mcp__gh__a"],
                    answered(TOOL_SEARCH, &text),
                ),
            );
            assert!(!body.to_string().contains("tool_reference"), "{body}");
            assert_eq!(
                body["messages"][2]["content"][0]["content"][0]["type"],
                "text"
            );
        }
    }

    #[test]
    fn a_vanished_name_is_dropped_and_a_result_left_empty_stays_text() {
        let wire = wire(&deferral(), true);
        let two = "Loaded 2 tool(s); call them by name from your next turn.\n[{\"name\": \"mcp__gh__a\"}, {\"name\": \"mcp__gh__gone\"}]";
        let body = sent(
            &wire,
            request(
                &["read", TOOL_SEARCH, "mcp__gh__a"],
                answered(TOOL_SEARCH, two),
            ),
        );
        assert_eq!(
            body["messages"][2]["content"][0]["content"],
            json!([{"type": "tool_reference", "tool_name": "mcp__gh__a"}])
        );
        let body = sent(
            &wire,
            request(&["read", TOOL_SEARCH], answered(TOOL_SEARCH, LOADED)),
        );
        assert!(!body.to_string().contains("tool_reference"), "{body}");
    }

    #[test]
    fn another_tools_result_is_untouched() {
        let wire = wire(&deferral(), true);
        let body = sent(
            &wire,
            request(
                &["read", TOOL_SEARCH, "mcp__gh__a"],
                answered("read", LOADED),
            ),
        );
        assert!(!body.to_string().contains("tool_reference"), "{body}");
    }
}
