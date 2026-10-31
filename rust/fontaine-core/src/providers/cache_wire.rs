//! Prompt-cache markers for a Chat wire whose own request mapping cannot place them. rig sends
//! one top-level marker, which suits the main agent's growing conversation; the guardian's last
//! message is per-call and never reused, so its marker has to sit on the system prompt instead.

use rig_core::completion::{CacheRetention, CompletionRequest};
use rig_core::error::EncodeError;
use rig_core::operation::Completion;
use rig_core::wire::{Body, Descriptor, Encoded, Mode, Wire};
use serde_json::{Value, json};

use super::{CacheScope, PromptCaching};
use crate::config::CacheTtl;

/// The OpenRouter upstreams that cache by themselves, by `vendor/` prefix. A copy of rig's
/// private list: rig refuses a one-hour retention on these, so it must track rig.
const AUTOMATIC_UPSTREAMS: [&str; 4] = ["openai/", "deepseek/", "x-ai/", "groq/"];

fn automatic(model: &str) -> bool {
    AUTOMATIC_UPSTREAMS
        .iter()
        .any(|prefix| model.starts_with(prefix))
}

#[derive(Clone)]
pub(super) struct CacheWire<W> {
    inner: W,
    scope: CacheScope,
    /// `None` when the account does not cache.
    retention: Option<CacheRetention>,
    automatic_upstream: bool,
}

impl<W> CacheWire<W> {
    pub(super) fn new(inner: W, model: &str, caching: PromptCaching, scope: CacheScope) -> Self {
        let automatic_upstream = automatic(model);
        let retention = caching.enabled.then_some(match caching.ttl {
            CacheTtl::OneHour if !automatic_upstream => CacheRetention::Long,
            CacheTtl::OneHour | CacheTtl::FiveMinutes => CacheRetention::Short,
        });
        CacheWire {
            inner,
            scope,
            retention,
            automatic_upstream,
        }
    }
}

/// The marker as the upstream reads it.
fn marker(retention: CacheRetention) -> Value {
    match retention {
        CacheRetention::Long => json!({"type": "ephemeral", "ttl": "1h"}),
        _ => json!({"type": "ephemeral"}),
    }
}

/// Puts `marker` on the last content block of the first system message, turning a string content
/// into a one-block array. Returns whether a system message was found.
fn mark_system(body: &mut Value, marker: Value) -> bool {
    let Some(message) = body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|messages| {
            messages
                .iter_mut()
                .find(|m| matches!(m["role"].as_str(), Some("system" | "developer")))
        })
    else {
        return false;
    };
    let content = message["content"].take();
    let mut blocks = match content {
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Array(blocks) => blocks,
        other => {
            message["content"] = other;
            return false;
        }
    };
    let Some(last) = blocks.last_mut().and_then(Value::as_object_mut) else {
        message["content"] = Value::Array(blocks);
        return false;
    };
    last.insert("cache_control".to_owned(), marker);
    message["content"] = Value::Array(blocks);
    true
}

impl<W> Wire for CacheWire<W>
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

    fn encode(&self, mut request: CompletionRequest, mode: Mode) -> Result<Encoded, EncodeError> {
        if self.scope == CacheScope::Conversation
            && request.options.cache.is_none()
            && let Some(retention) = self.retention
        {
            request.options.cache = Some(retention);
        }
        let mut encoded = self.inner.encode(request, mode)?;
        if self.scope == CacheScope::StaticPrefix
            && !self.automatic_upstream
            && let Some(retention) = self.retention
            && let Body::Bytes(bytes) = encoded.request.body_mut()
        {
            match serde_json::from_slice::<Value>(bytes) {
                Ok(mut body) => {
                    if mark_system(&mut body, marker(retention)) {
                        *bytes = serde_json::to_vec(&body).expect("a parsed JSON value serializes");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "the guardian request body is not JSON; no cache marker");
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
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::providers::openai::wire::{Chat, OPENROUTER};
    use rig_core::wire::Operation;

    fn chat(model: &str) -> Chat {
        Chat::new(OpenAIConfig::with_key(&OPENROUTER, "sk-test"), model)
    }

    fn caching(enabled: bool, ttl: CacheTtl) -> PromptCaching {
        PromptCaching { enabled, ttl }
    }

    fn sent(model: &str, caching: PromptCaching, scope: CacheScope) -> Value {
        let wire = CacheWire::new(chat(model), model, caching, scope);
        let request = CompletionRequest::new("the per-call tail")
            .preamble("the static prefix")
            .max_tokens(16);
        let request = Completion::prepare(request, &wire.describe()).expect("a valid request");
        let encoded = wire
            .encode(request, Mode::Unary)
            .expect("an encodable request");
        let Body::Bytes(bytes) = encoded.request.body() else {
            panic!("a chat body is JSON");
        };
        serde_json::from_slice(bytes).expect("JSON")
    }

    const CLAUDE: &str = "anthropic/claude-haiku-4.5";

    #[test]
    fn the_conversation_carries_a_top_level_marker() {
        let body = sent(
            CLAUDE,
            caching(true, CacheTtl::FiveMinutes),
            CacheScope::Conversation,
        );
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        let body = sent(
            CLAUDE,
            caching(true, CacheTtl::OneHour),
            CacheScope::Conversation,
        );
        assert_eq!(
            body["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    #[test]
    fn no_marker_when_caching_is_off() {
        for scope in [CacheScope::Conversation, CacheScope::StaticPrefix] {
            let body = sent(CLAUDE, caching(false, CacheTtl::OneHour), scope);
            assert!(!body.to_string().contains("cache_control"), "{body}");
        }
    }

    #[test]
    fn an_automatic_upstream_never_gets_a_long_retention() {
        for model in [
            "openai/gpt-5.2",
            "deepseek/deepseek-chat-v3.1",
            "x-ai/grok-4",
            "groq/llama",
        ] {
            for scope in [CacheScope::Conversation, CacheScope::StaticPrefix] {
                let body = sent(model, caching(true, CacheTtl::OneHour), scope);
                assert!(
                    !body.to_string().contains("cache_control"),
                    "{model}: {body}"
                );
            }
        }
    }

    #[test]
    fn the_guardian_marks_the_system_prompt_not_the_tail() {
        let body = sent(
            CLAUDE,
            caching(true, CacheTtl::FiveMinutes),
            CacheScope::StaticPrefix,
        );
        assert!(body.get("cache_control").is_none(), "{body}");
        assert_eq!(
            body["messages"][0]["content"],
            json!([{
                "type": "text",
                "text": "the static prefix",
                "cache_control": {"type": "ephemeral"}
            }])
        );
        assert_eq!(body["messages"][1]["content"], json!("the per-call tail"));
        let body = sent(
            CLAUDE,
            caching(true, CacheTtl::OneHour),
            CacheScope::StaticPrefix,
        );
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    #[test]
    fn a_marker_the_request_already_carries_stands() {
        let wire = CacheWire::new(
            chat(CLAUDE),
            CLAUDE,
            caching(true, CacheTtl::OneHour),
            CacheScope::Conversation,
        );
        let mut request = CompletionRequest::new("hi").max_tokens(16);
        request.options.cache = Some(CacheRetention::Short);
        let request = Completion::prepare(request, &wire.describe()).expect("a valid request");
        let encoded = wire.encode(request, Mode::Unary).expect("encodable");
        let Body::Bytes(bytes) = encoded.request.body() else {
            panic!("a chat body is JSON");
        };
        let body: Value = serde_json::from_slice(bytes).expect("JSON");
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
    }

    #[test]
    fn a_string_system_content_becomes_a_marked_block() {
        let mut body = json!({"messages": [
            {"role": "system", "content": "rules"},
            {"role": "user", "content": "hi"},
        ]});
        assert!(mark_system(&mut body, json!({"type": "ephemeral"})));
        assert_eq!(
            body["messages"][0]["content"],
            json!([{"type": "text", "text": "rules", "cache_control": {"type": "ephemeral"}}])
        );
    }

    #[test]
    fn a_body_without_a_system_message_is_left_alone() {
        let mut body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let before = body.clone();
        assert!(!mark_system(&mut body, json!({"type": "ephemeral"})));
        assert_eq!(body, before);
    }
}
