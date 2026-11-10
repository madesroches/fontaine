//! Some Chat Completions hosts stream Claude reasoning as `content` parts of
//! `{"type": "reasoning", "summary": [{"type": "summary_text", "text", "signature"}]}`. The first
//! chunk is an empty placeholder and the signature arrives in a later one. rig keeps each chunk as
//! its own part and replays them all, which the host refuses as a thinking block with no thinking.
//! The request is rewritten so the fragments of one reasoning part are one part again.

use rig_core::completion::CompletionRequest;
use rig_core::error::EncodeError;
use rig_core::operation::Completion;
use rig_core::wire::{Body, Descriptor, Encoded, Mode, Wire};
use serde_json::{Map, Value};

/// Merges the streamed fragments of a Chat `reasoning` content part back into one part before
/// replay; see [`merge_reasoning_parts`].
#[derive(Clone)]
pub(super) struct ReasoningPartsWire<W> {
    inner: W,
}

impl<W> ReasoningPartsWire<W> {
    pub(super) fn new(inner: W) -> Self {
        ReasoningPartsWire { inner }
    }
}

const MARKER: &[u8] = b"\"type\":\"reasoning\"";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

impl<W> Wire for ReasoningPartsWire<W>
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
        // rig serializes compact JSON, so a body without reasoning parts is never parsed.
        if let Body::Bytes(bytes) = encoded.request.body_mut()
            && contains(bytes, MARKER)
            && let Ok(mut body) = serde_json::from_slice::<Value>(bytes)
            && merge_reasoning_parts(&mut body)
        {
            *bytes = serde_json::to_vec(&body).expect("a parsed JSON value serializes");
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

fn is_reasoning(part: &Value) -> bool {
    part.get("type").and_then(Value::as_str) == Some("reasoning")
}

/// A field the host would count as not there.
fn missing(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(_) => false,
    }
}

fn filled(value: Option<&Value>) -> bool {
    !missing(value)
}

fn summaries(part: &Value) -> impl Iterator<Item = &Value> {
    part.get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn signed(part: &Value) -> bool {
    filled(part.get("signature"))
        || filled(part.get("encrypted_content"))
        || summaries(part).any(|entry| filled(entry.get("signature")))
}

fn has_text(part: &Value) -> bool {
    summaries(part).any(|entry| filled(entry.get("text")))
}

/// Fills in what `earlier` lacks from `later`; a string `text` is appended instead.
fn merge_fields(earlier: &mut Map<String, Value>, later: &Map<String, Value>, skip: &[&str]) {
    for (key, value) in later {
        if skip.contains(&key.as_str()) {
            continue;
        }
        match (earlier.get_mut(key), value) {
            (Some(Value::String(have)), Value::String(add)) if key == "text" => {
                have.push_str(add);
            }
            (have, _) if missing(have.as_deref()) => {
                earlier.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
}

/// Continues `earlier` with `later`, both reasoning parts.
fn merge_part(earlier: &mut Value, later: &Value) {
    let (Some(earlier), Some(later)) = (earlier.as_object_mut(), later.as_object()) else {
        return;
    };
    merge_fields(earlier, later, &["summary"]);
    let Some(Value::Array(extra)) = later.get("summary") else {
        return;
    };
    match earlier.get_mut("summary") {
        Some(Value::Array(have)) => {
            for (at, entry) in extra.iter().enumerate() {
                match (have.get_mut(at), entry) {
                    (Some(Value::Object(have)), Value::Object(entry)) => {
                        merge_fields(have, entry, &[]);
                    }
                    (Some(_), _) => {}
                    (None, _) => have.push(entry.clone()),
                }
            }
        }
        _ => {
            earlier.insert("summary".to_owned(), Value::Array(extra.clone()));
        }
    }
}

fn merge_content(content: &mut Vec<Value>) {
    let mut merged: Vec<Value> = Vec::with_capacity(content.len());
    for part in content.drain(..) {
        match merged.last_mut() {
            Some(last) if is_reasoning(last) && is_reasoning(&part) && !signed(last) => {
                merge_part(last, &part);
            }
            _ => merged.push(part),
        }
    }
    merged.retain(|part| !is_reasoning(part) || signed(part) || has_text(part));
    *content = merged;
}

/// For every `role: "assistant"` message whose `content` is an array: merge each run of
/// fragments of one reasoning part, then drop reasoning parts that carry nothing.
/// Returns whether the body changed.
///
/// A part continues the previous reasoning part until that one is signed; a reasoning part after
/// a signed one starts a new part, so interleaved thinking blocks stay separate. A message left
/// without content loses the `content` key when it has `tool_calls`, and is removed otherwise,
/// as rig never sends a message with neither.
pub(super) fn merge_reasoning_parts(body: &mut Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    messages.retain_mut(|message| {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return true;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            return true;
        };
        if !content.iter().any(is_reasoning) {
            return true;
        }
        let before = content.clone();
        merge_content(content);
        if *content == before {
            return true;
        }
        changed = true;
        if !content.is_empty() {
            return true;
        }
        if filled(message.get("tool_calls")) {
            if let Some(object) = message.as_object_mut() {
                object.remove("content");
            }
            true
        } else {
            false
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::providers::openai::wire::{Chat, OPENROUTER};
    use rig_core::wire::Operation;
    use serde_json::json;

    fn placeholder() -> Value {
        json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": ""}]})
    }

    fn signed_part(text: &str, signature: &str) -> Value {
        json!({"type": "reasoning", "summary": [
            {"type": "summary_text", "text": text, "signature": signature}]})
    }

    fn assistant(content: Vec<Value>) -> Value {
        json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": content,
             "tool_calls": [{"id": "c1", "type": "function",
                             "function": {"name": "read", "arguments": "{}"}}]},
        ]})
    }

    #[test]
    fn the_issues_pair_becomes_one_signed_part() {
        let mut body = assistant(vec![
            placeholder(),
            signed_part("", "CAQ"),
            json!({"type": "text", "text": "ok"}),
        ]);
        let before_calls = body["messages"][1]["tool_calls"].clone();
        assert!(merge_reasoning_parts(&mut body));
        assert_eq!(
            body["messages"][1]["content"],
            json!([signed_part("", "CAQ"), {"type": "text", "text": "ok"}])
        );
        assert_eq!(body["messages"][1]["tool_calls"], before_calls);
        assert_eq!(body["messages"][0]["content"], json!("hi"));
    }

    #[test]
    fn text_fragments_join_under_the_signature() {
        let mut body = assistant(vec![
            placeholder(),
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "think"}]}),
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "ing"}]}),
            signed_part("", "SIG"),
        ]);
        assert!(merge_reasoning_parts(&mut body));
        assert_eq!(
            body["messages"][1]["content"],
            json!([signed_part("thinking", "SIG")])
        );
    }

    #[test]
    fn two_signed_parts_stay_two() {
        let mut body = assistant(vec![signed_part("a", "S1"), signed_part("b", "S2")]);
        assert!(!merge_reasoning_parts(&mut body));
        assert_eq!(
            body["messages"][1]["content"],
            json!([signed_part("a", "S1"), signed_part("b", "S2")])
        );
    }

    #[test]
    fn a_lone_empty_part_is_dropped_and_one_with_text_kept() {
        let mut body = assistant(vec![placeholder(), json!({"type": "text", "text": "x"})]);
        assert!(merge_reasoning_parts(&mut body));
        assert_eq!(
            body["messages"][1]["content"],
            json!([{"type": "text", "text": "x"}])
        );

        let texted =
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "t"}]});
        let mut body = assistant(vec![texted.clone()]);
        assert!(!merge_reasoning_parts(&mut body));
        assert_eq!(body["messages"][1]["content"], json!([texted]));
    }

    #[test]
    fn a_placeholder_apart_from_the_signed_part_is_dropped() {
        let mut body = assistant(vec![
            placeholder(),
            json!({"type": "text", "text": "x"}),
            signed_part("", "SIG"),
        ]);
        assert!(merge_reasoning_parts(&mut body));
        assert_eq!(
            body["messages"][1]["content"],
            json!([{"type": "text", "text": "x"}, signed_part("", "SIG")])
        );
    }

    #[test]
    fn an_emptied_message_with_tool_calls_loses_its_content() {
        let mut body = assistant(vec![placeholder()]);
        assert!(merge_reasoning_parts(&mut body));
        assert!(body["messages"][1].get("content").is_none(), "{body}");
        assert!(body["messages"][1].get("tool_calls").is_some());
    }

    #[test]
    fn an_emptied_message_without_tool_calls_is_removed() {
        let mut body = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [placeholder()]},
        ]});
        assert!(merge_reasoning_parts(&mut body));
        assert_eq!(body["messages"], json!([{"role": "user", "content": "hi"}]));
    }

    #[test]
    fn other_bodies_are_left_alone() {
        for mut body in [
            json!({"messages": [{"role": "user", "content": [placeholder()]}]}),
            json!({"messages": [{"role": "assistant", "content": "text"}]}),
            json!({"messages": [{"role": "assistant", "content": [{"type": "text", "text": "a"}]}]}),
            json!({"model": "m"}),
        ] {
            let before = body.clone();
            assert!(!merge_reasoning_parts(&mut body));
            assert_eq!(body, before);
        }
    }

    #[test]
    fn a_request_without_reasoning_encodes_byte_identically() {
        let chat = Chat::new(OpenAIConfig::with_key(&OPENROUTER, "sk-test"), "m");
        let wrapped = ReasoningPartsWire::new(chat.clone());
        let encode = |wire: &dyn Fn(CompletionRequest) -> Encoded| {
            let request = CompletionRequest::new("hello").max_tokens(16);
            let request = Completion::prepare(request, &chat.describe()).expect("valid");
            wire(request)
        };
        let plain = encode(&|r| chat.encode(r, Mode::Unary).expect("encodable"));
        let rewritten = encode(&|r| wrapped.encode(r, Mode::Unary).expect("encodable"));
        let (Body::Bytes(a), Body::Bytes(b)) = (plain.request.body(), rewritten.request.body())
        else {
            panic!("a chat body is JSON");
        };
        assert_eq!(a, b);
    }
}
