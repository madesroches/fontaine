//! The messages of Claude Code's SDK stream-json protocol, as typed structs. Inbound types ignore
//! unknown fields, so an SDK that adds one keeps working; outbound types carry every field the
//! SDKs read.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A line the host sent.
#[derive(Debug)]
pub enum Inbound {
    User(UserMessage),
    ControlRequest { request_id: String, request: Value },
    ControlResponse(ControlResponse),
    ControlCancel { request_id: String },
    KeepAlive,
}

/// A user message, reduced to what a turn needs.
#[derive(Debug, PartialEq, Eq)]
pub struct UserMessage {
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub struct ControlResponse {
    pub request_id: String,
    pub outcome: Result<Value, String>,
}

#[derive(Deserialize)]
struct RawControlResponse {
    response: RawResponseBody,
}

#[derive(Deserialize)]
struct RawResponseBody {
    subtype: String,
    request_id: String,
    #[serde(default)]
    response: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// Why a line could not be accepted. It ends the run.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed(pub String);

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The text of a user message: a string, or the text blocks of an array.
fn user_text(value: &Value) -> Result<String, Malformed> {
    let content = value
        .pointer("/message/content")
        .ok_or_else(|| Malformed("a `user` line has no `message.content`".into()))?;
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
        _ => Err(Malformed(
            "`message.content` must be a string or an array of content blocks".into(),
        )),
    }
}

/// Parses one stdin line.
pub fn parse_line(line: &[u8]) -> Result<Inbound, Malformed> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|e| Malformed(format!("an input line is not JSON: {e}")))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Malformed("an input line has no `type`".into()))?;
    match kind {
        "user" => {
            let text = user_text(&value)?;
            if text.trim().is_empty() {
                return Err(Malformed("a `user` line holds an empty prompt".into()));
            }
            Ok(Inbound::User(UserMessage { text }))
        }
        "control_request" => {
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Malformed("a `control_request` has no `request_id`".into()))?
                .to_string();
            let request = value
                .get("request")
                .cloned()
                .ok_or_else(|| Malformed("a `control_request` has no `request`".into()))?;
            Ok(Inbound::ControlRequest {
                request_id,
                request,
            })
        }
        "control_response" => {
            let raw: RawControlResponse = serde_json::from_value(value)
                .map_err(|e| Malformed(format!("a `control_response` is malformed: {e}")))?;
            let body = raw.response;
            let outcome = match body.subtype.as_str() {
                "success" => Ok(body.response.unwrap_or(Value::Null)),
                _ => Err(body
                    .error
                    .unwrap_or_else(|| "the host reported an error".into())),
            };
            Ok(Inbound::ControlResponse(ControlResponse {
                request_id: body.request_id,
                outcome,
            }))
        }
        "control_cancel_request" => Ok(Inbound::ControlCancel {
            request_id: value
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Malformed("a `control_cancel_request` has no `request_id`".into()))?
                .to_string(),
        }),
        "keep_alive" => Ok(Inbound::KeepAlive),
        "update_environment_variables" => Err(Malformed(
            "`update_environment_variables` is not supported: the environment is fixed at start"
                .into(),
        )),
        other => Err(Malformed(format!(
            "an input line has the unknown type `{other}`"
        ))),
    }
}

/// `system/init`.
#[derive(Debug, Serialize)]
pub struct SystemInit {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub subtype: &'static str,
    pub cwd: String,
    pub session_id: String,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<Value>,
    pub model: String,
    #[serde(rename = "permissionMode")]
    pub permission_mode: String,
    pub slash_commands: Vec<String>,
    #[serde(rename = "apiKeySource")]
    pub api_key_source: &'static str,
    pub claude_code_version: &'static str,
    pub output_style: &'static str,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub plugins: Vec<Value>,
    pub uuid: String,
    pub fontaine_version: &'static str,
    pub compat_protocol: &'static str,
}

/// The `message` of an `assistant` message.
#[derive(Debug, Serialize)]
pub struct ApiMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub role: &'static str,
    pub model: String,
    pub content: Vec<Value>,
    pub stop_reason: Option<&'static str>,
    pub stop_sequence: Option<String>,
    pub usage: Value,
}

#[derive(Debug, Serialize)]
pub struct Assistant {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: ApiMessage,
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    pub uuid: String,
}

#[derive(Debug, Serialize)]
pub struct UserToolResults {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: Value,
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    pub uuid: String,
}

#[derive(Debug, Serialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub event: Value,
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    pub uuid: String,
}

#[derive(Debug, Serialize)]
pub struct PermissionDenied {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub subtype: &'static str,
    pub tool_name: String,
    pub tool_use_id: String,
    pub tool_input: Value,
    pub message: String,
    pub session_id: String,
    pub uuid: String,
}

#[derive(Debug, Serialize)]
pub struct ResultMessage {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub subtype: &'static str,
    pub is_error: bool,
    pub duration_ms: u64,
    pub duration_api_ms: u64,
    pub num_turns: u64,
    pub result: String,
    pub stop_reason: Option<&'static str>,
    pub session_id: String,
    pub total_cost_usd: Option<f64>,
    pub usage: Value,
    #[serde(rename = "modelUsage")]
    pub model_usage: Value,
    pub permission_denials: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    pub uuid: String,
    pub fontaine_usage: Value,
}

/// An outbound `control_request`.
pub fn control_request(request_id: &str, request: Value) -> Value {
    json!({ "type": "control_request", "request_id": request_id, "request": request })
}

pub fn control_cancel(request_id: &str) -> Value {
    json!({ "type": "control_cancel_request", "request_id": request_id })
}

pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response }
    })
}

pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_line_reads_text_from_a_string_or_blocks_and_ignores_unknown_fields() {
        let line = br#"{"type":"user","session_id":"","parent_tool_use_id":null,"client_composed":true,"message":{"role":"user","content":"hi"}}"#;
        assert!(
            matches!(parse_line(line), Ok(Inbound::User(UserMessage { text })) if text == "hi")
        );
        let line = br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"a"},{"type":"image"},{"type":"text","text":"b"}]}}"#;
        assert!(
            matches!(parse_line(line), Ok(Inbound::User(UserMessage { text })) if text == "ab")
        );
    }

    #[test]
    fn control_frames_parse_with_their_ids() {
        let request =
            br#"{"type":"control_request","request_id":"r1","request":{"subtype":"interrupt"}}"#;
        assert!(
            matches!(parse_line(request), Ok(Inbound::ControlRequest { request_id, .. }) if request_id == "r1")
        );
        let ok = br#"{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{"behavior":"allow"}}}"#;
        assert!(matches!(
            parse_line(ok),
            Ok(Inbound::ControlResponse(ControlResponse {
                outcome: Ok(_),
                ..
            }))
        ));
        let bad = br#"{"type":"control_response","response":{"subtype":"error","request_id":"x","error":"no"}}"#;
        assert!(
            matches!(parse_line(bad), Ok(Inbound::ControlResponse(ControlResponse { outcome: Err(e), .. })) if e == "no")
        );
        assert!(matches!(
            parse_line(br#"{"type":"keep_alive"}"#),
            Ok(Inbound::KeepAlive)
        ));
        assert!(matches!(
            parse_line(br#"{"type":"control_cancel_request","request_id":"q"}"#),
            Ok(Inbound::ControlCancel { .. })
        ));
    }

    #[test]
    fn malformed_and_unsupported_lines_are_refused() {
        assert!(parse_line(b"not json").is_err());
        assert!(parse_line(br#"{"no":"type"}"#).is_err());
        assert!(parse_line(br#"{"type":"user","message":{"content":""}}"#).is_err());
        assert!(parse_line(br#"{"type":"update_environment_variables","variables":{}}"#).is_err());
        assert!(parse_line(br#"{"type":"mystery"}"#).is_err());
    }

    #[test]
    fn the_result_message_carries_the_keys_the_sdks_index() {
        let message = ResultMessage {
            kind: "result",
            subtype: "success",
            is_error: false,
            duration_ms: 1,
            duration_api_ms: 1,
            num_turns: 1,
            result: "ok".into(),
            stop_reason: Some("end_turn"),
            session_id: "s".into(),
            total_cost_usd: None,
            usage: json!({}),
            model_usage: json!({}),
            permission_denials: Vec::new(),
            structured_output: None,
            terminal_reason: None,
            errors: Vec::new(),
            uuid: "u".into(),
            fontaine_usage: json!({}),
        };
        let value = serde_json::to_value(&message).unwrap();
        for key in [
            "subtype",
            "duration_ms",
            "duration_api_ms",
            "is_error",
            "num_turns",
            "session_id",
            "total_cost_usd",
            "modelUsage",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
        assert!(value["total_cost_usd"].is_null());
    }

    /// The lines of a fixture the real `claude` and the SDK wrote.
    fn fixture_lines(suffix: &str) -> Vec<(String, Value)> {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_sdk");
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).expect("the fixture directory") {
            let path = entry.unwrap().path();
            if !path.to_string_lossy().ends_with(suffix) {
                continue;
            }
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                out.push((line.to_string(), serde_json::from_str(line).unwrap()));
            }
        }
        out
    }

    /// The JSON type of a value, the way the SDKs' parsers see it.
    fn kind(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    #[test]
    fn every_line_the_sdks_sent_to_the_real_claude_is_accepted() {
        let lines = fixture_lines(".stdin.jsonl");
        assert!(!lines.is_empty());
        for (line, _) in lines {
            let parsed = parse_line(line.as_bytes());
            // `get_context_usage` and the rest parse as control requests; the router answers
            // them with an error naming the subtype.
            assert!(parsed.is_ok(), "{line}: {parsed:?}");
        }
    }

    #[test]
    fn the_messages_serialize_with_the_keys_and_types_the_sdks_read_in_the_real_ones() {
        // The fields each SDK parser indexes directly, per message type.
        let required: [(&str, Option<&str>, &[&str]); 3] = [
            (
                "result",
                Some("success"),
                &[
                    "subtype",
                    "duration_ms",
                    "duration_api_ms",
                    "is_error",
                    "num_turns",
                    "session_id",
                    "total_cost_usd",
                    "usage",
                    "modelUsage",
                    "permission_denials",
                    "uuid",
                    "result",
                    "stop_reason",
                ],
            ),
            (
                "system",
                Some("init"),
                &[
                    "cwd",
                    "session_id",
                    "tools",
                    "mcp_servers",
                    "model",
                    "permissionMode",
                    "slash_commands",
                    "apiKeySource",
                    "claude_code_version",
                    "output_style",
                    "uuid",
                    "agents",
                    "skills",
                    "plugins",
                ],
            ),
            (
                "assistant",
                None,
                &["message", "parent_tool_use_id", "session_id", "uuid"],
            ),
        ];
        let real = fixture_lines(".stdout.jsonl");
        let ours = [
            serde_json::to_value(ResultMessage {
                kind: "result",
                subtype: "success",
                is_error: false,
                duration_ms: 1,
                duration_api_ms: 1,
                num_turns: 1,
                result: "ok".into(),
                stop_reason: Some("end_turn"),
                session_id: "s".into(),
                total_cost_usd: Some(0.5),
                usage: json!({}),
                model_usage: json!({}),
                permission_denials: Vec::new(),
                structured_output: None,
                terminal_reason: Some("completed"),
                errors: Vec::new(),
                uuid: "u".into(),
                fontaine_usage: json!({}),
            })
            .unwrap(),
            serde_json::to_value(SystemInit {
                kind: "system",
                subtype: "init",
                cwd: "/w".into(),
                session_id: "s".into(),
                tools: Vec::new(),
                mcp_servers: Vec::new(),
                model: "m".into(),
                permission_mode: "manual".into(),
                slash_commands: Vec::new(),
                api_key_source: "none",
                claude_code_version: "2.1.325",
                output_style: "default",
                agents: Vec::new(),
                skills: Vec::new(),
                plugins: Vec::new(),
                uuid: "u".into(),
                fontaine_version: "0",
                compat_protocol: "claude-sdk-1",
            })
            .unwrap(),
            serde_json::to_value(Assistant {
                kind: "assistant",
                message: ApiMessage {
                    id: "m".into(),
                    kind: "message",
                    role: "assistant",
                    model: "m".into(),
                    content: vec![json!({"type": "text", "text": "x"})],
                    stop_reason: None,
                    stop_sequence: None,
                    usage: json!({}),
                },
                parent_tool_use_id: None,
                session_id: "s".into(),
                uuid: "u".into(),
            })
            .unwrap(),
        ];
        for ((kind_name, subtype, keys), ours) in required.iter().zip(&ours) {
            let sample = real
                .iter()
                .map(|(_, value)| value)
                .find(|v| v["type"] == *kind_name && subtype.is_none_or(|s| v["subtype"] == s))
                .unwrap_or_else(|| panic!("no real {kind_name} in the fixtures"));
            for key in *keys {
                assert!(ours.get(key).is_some(), "{kind_name}: ours lacks `{key}`");
                // `total_cost_usd` and `stop_reason` are nullable in ours by design.
                if let Some(theirs) = sample.get(key)
                    && !theirs.is_null()
                    && !ours[key].is_null()
                {
                    assert_eq!(kind(&ours[key]), kind(theirs), "{kind_name}.{key}");
                }
            }
        }
        // The nested message the Python parser indexes.
        let message = &ours[2]["message"];
        assert!(message.get("content").is_some() && message.get("model").is_some());
    }
}
