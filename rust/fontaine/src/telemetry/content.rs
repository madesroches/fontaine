//! What may leave the process. Fontaine's own fields are content-free by construction; the
//! only content comes from rig's span fields, and only behind the opt-in keys. Opted-in text
//! is redacted first and truncated second, so a cut can never leave a recognisable prefix of a
//! secret.

use fontaine_core::config::Telemetry;
use rig_core::redact;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Open,
    Prompt,
    Response,
    ToolDetails,
    ToolContent,
    /// A `gen_ai.*` or `rig.*` field this build does not know: treated as content.
    Unknown,
}

fn classify(key: &str) -> Class {
    match key {
        "gen_ai.prompt" | "gen_ai.input.messages" | "gen_ai.system_instructions" => Class::Prompt,
        "gen_ai.completion" | "gen_ai.output.messages" => Class::Response,
        "gen_ai.tool.call.arguments" => Class::ToolDetails,
        "gen_ai.tool.call.result" => Class::ToolContent,
        "gen_ai.operation.name"
        | "gen_ai.provider.name"
        | "gen_ai.request.model"
        | "gen_ai.request.stream"
        | "gen_ai.response.model"
        | "gen_ai.response.id"
        | "gen_ai.agent.name"
        | "gen_ai.tool.name"
        | "gen_ai.tool.type"
        | "gen_ai.tool.call.id"
        | "gen_ai.tool.call.outcome"
        | "gen_ai.tool.error.type"
        | "rig.provider_request_id"
        | "rig.completion_parent" => Class::Open,
        _ if key.starts_with("gen_ai.usage.") => Class::Open,
        _ if key.starts_with("gen_ai.") || key.starts_with("rig.") => Class::Unknown,
        _ => Class::Open,
    }
}

#[derive(Clone, Debug)]
pub struct ContentPolicy {
    prompts: bool,
    responses: bool,
    tool_details: bool,
    tool_content: bool,
    max_bytes: usize,
}

impl ContentPolicy {
    pub fn from_config(telemetry: &Telemetry) -> Self {
        ContentPolicy {
            prompts: telemetry.log_user_prompts,
            responses: telemetry.log_assistant_responses,
            tool_details: telemetry.log_tool_details,
            tool_content: telemetry.log_tool_content,
            max_bytes: usize::try_from(telemetry.content_max_bytes).unwrap_or(usize::MAX),
        }
    }

    /// Whether `key` is a field that carries no content, so its value needs no scrubbing.
    pub fn is_open(&self, key: &str) -> bool {
        classify(key) == Class::Open
    }

    /// The value to export for `key`, or `None` when it must not leave the process.
    pub fn keep(&self, key: &str, value: &str) -> Option<String> {
        let allowed = match classify(key) {
            Class::Open => return Some(value.to_string()),
            Class::Prompt => self.prompts,
            Class::Response => self.responses,
            Class::ToolDetails => self.tool_details,
            Class::ToolContent => self.tool_content,
            Class::Unknown => false,
        };
        allowed.then(|| self.scrub(value))
    }

    fn scrub(&self, value: &str) -> String {
        let redacted = redact::text(&redact::json_text(value)).into_owned();
        truncate(redacted, self.max_bytes)
    }

    pub fn export_fields(&self, fields: &[(String, String)]) -> Vec<(String, String)> {
        fields
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), self.keep(key, value)?)))
            .collect()
    }
}

fn truncate(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(prompts: bool, max_bytes: usize) -> ContentPolicy {
        ContentPolicy {
            prompts,
            responses: false,
            tool_details: false,
            tool_content: false,
            max_bytes,
        }
    }

    #[test]
    fn content_is_dropped_without_its_opt_in() {
        let p = policy(false, 100);
        assert_eq!(p.keep("gen_ai.prompt", "hello"), None);
        assert_eq!(p.keep("gen_ai.completion", "hello"), None);
        assert_eq!(p.keep("gen_ai.future.field", "hello"), None);
    }

    #[test]
    fn opted_in_content_is_redacted_then_truncated() {
        let p = policy(true, 12);
        let kept = p
            .keep(
                "gen_ai.prompt",
                "key sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            )
            .unwrap();
        assert_eq!(kept, "key [REDACTE");
        assert!(!kept.contains("sk-ant"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate("héllo".to_string(), 2), "h");
    }

    #[test]
    fn non_content_fields_pass_through() {
        let p = policy(false, 100);
        assert_eq!(
            p.keep("gen_ai.usage.input_tokens", "12"),
            Some("12".to_string())
        );
        assert_eq!(p.keep("prompt.id", "abc"), Some("abc".to_string()));
    }
}
