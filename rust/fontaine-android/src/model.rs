//! The app's own read-only mirror of what the host publishes. It is lenient on purpose: unknown
//! fields are ignored and unknown kinds decode to a placeholder, so a newer host never breaks the
//! app. A test in `fontaine-tui` serializes the host's real values and decodes them here.

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Done,
    Failed(String),
    Denied(String),
    Interrupted,
    Unknown,
}

impl ToolStatus {
    fn from_value(value: &Value) -> ToolStatus {
        let text = |inner: &Value| inner.as_str().unwrap_or_default().to_string();
        match value {
            Value::String(name) => match name.as_str() {
                "Pending" => ToolStatus::Pending,
                "Done" => ToolStatus::Done,
                "Interrupted" => ToolStatus::Interrupted,
                _ => ToolStatus::Unknown,
            },
            Value::Object(map) if map.len() == 1 => match map.iter().next() {
                Some((name, inner)) if name == "Failed" => ToolStatus::Failed(text(inner)),
                Some((name, inner)) if name == "Denied" => ToolStatus::Denied(text(inner)),
                _ => ToolStatus::Unknown,
            },
            _ => ToolStatus::Unknown,
        }
    }
}

/// One transcript row, reduced to what the app renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    User(String),
    Assistant {
        text: String,
        streaming: bool,
    },
    Tool {
        tool: String,
        summary: String,
        status: ToolStatus,
        output: Option<String>,
    },
    Notice {
        warning: bool,
        headline: String,
        hint: Option<String>,
    },
    Error(String),
    /// A kind this build does not render, by the name the host gave it.
    Other(String),
}

fn text_of(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Entry {
    // The host's `Entry` is an externally tagged enum, so an unknown variant cannot be absorbed
    // by `#[serde(other)]`; reading it as a value first lets one unknown kind stand in for itself.
    fn from_value(value: &Value) -> Entry {
        let Value::Object(map) = value else {
            return Entry::Other(value.as_str().unwrap_or("unknown").to_string());
        };
        let Some((kind, body)) = map.iter().next().filter(|_| map.len() == 1) else {
            return Entry::Other("unknown".to_string());
        };
        let field = |name: &str| body.get(name);
        match kind.as_str() {
            "User" => Entry::User(text_of(Some(body))),
            "Error" => Entry::Error(text_of(Some(body))),
            "Assistant" => Entry::Assistant {
                text: text_of(field("text")),
                streaming: field("streaming").and_then(Value::as_bool).unwrap_or(false),
            },
            "Tool" => Entry::Tool {
                tool: text_of(field("tool")),
                summary: text_of(field("summary")),
                status: field("status").map_or(ToolStatus::Unknown, ToolStatus::from_value),
                output: field("output")
                    .and_then(|o| o.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
            "Notice" => Entry::Notice {
                warning: field("severity").and_then(Value::as_str) == Some("warning"),
                headline: text_of(field("headline")),
                hint: field("hint").and_then(Value::as_str).map(str::to_string),
            },
            other => Entry::Other(other.to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Entry, D::Error> {
        let value = Value::deserialize(d).map_err(de::Error::custom)?;
        Ok(Entry::from_value(&value))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Status {
    pub mode: String,
    pub model: String,
    pub running: bool,
    pub afk: bool,
    pub cwd: String,
    pub branch: Option<String>,
    pub running_agents: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Question {
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default, rename = "multiSelect", alias = "multi_select")]
    pub multi_select: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
    Gap,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DiffLine {
    pub kind: DiffKind,
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DiffPreview {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
    pub hidden: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "dialog", rename_all = "snake_case")]
pub enum Dialog {
    Approval {
        id: u64,
        #[serde(default)]
        tool: String,
        #[serde(default)]
        agent_name: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        choices: Vec<String>,
        #[serde(default)]
        preview: Option<DiffPreview>,
    },
    Question {
        id: u64,
        #[serde(default)]
        index: usize,
        #[serde(default)]
        questions: Vec<Question>,
    },
    HostOnly {
        #[serde(default)]
        what: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteEvent {
    Set {
        at: usize,
        entry: Entry,
    },
    Append {
        at: usize,
        text: String,
        #[serde(default)]
        streaming: bool,
    },
    Truncate {
        len: usize,
    },
    Status {
        status: Box<Status>,
    },
    Dialog {
        #[serde(default)]
        dialog: Option<Dialog>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub status: Option<Status>,
    pub dialog: Option<Dialog>,
}

impl Snapshot {
    pub fn parse(state: &str) -> Result<Snapshot, serde_json::Error> {
        serde_json::from_str(state)
    }
}

impl RemoteEvent {
    pub fn parse(event: Value) -> Result<RemoteEvent, serde_json::Error> {
        serde_json::from_value(event)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_unknown_entry_kind_decodes_to_a_placeholder_and_the_rest_still_decode() {
        let state = json!({
            "entries": [
                { "User": "hi" },
                { "Hologram": { "frames": 3 } },
                { "Assistant": { "text": "yo", "streaming": false, "extra": 1 } },
                { "Reasoning": { "text": "t", "live": "", "streaming": false, "clipped": false, "ended": true } },
                { "Tool": { "id": "c1", "tool": "write", "summary": "write a",
                    "status": { "Failed": "boom" }, "ordinal": 1, "call": "x",
                    "output": { "text": "out", "clipped": false }, "diff": null } },
                { "Notice": { "severity": "warning", "headline": "h", "hint": null } },
                { "Error": "bad" },
            ],
            "status": { "mode": "manual", "model": "m", "running": true, "new_field": [] },
            "dialog": null,
            "future": true,
        });
        let snapshot = Snapshot::parse(&state.to_string()).expect("a lenient decode");
        assert_eq!(snapshot.entries.len(), 7);
        assert_eq!(snapshot.entries[0], Entry::User("hi".into()));
        assert_eq!(snapshot.entries[1], Entry::Other("Hologram".into()));
        assert!(matches!(&snapshot.entries[2], Entry::Assistant { text, .. } if text == "yo"));
        assert_eq!(snapshot.entries[3], Entry::Other("Reasoning".into()));
        assert!(matches!(
            &snapshot.entries[4],
            Entry::Tool { status: ToolStatus::Failed(m), output: Some(o), .. } if m == "boom" && o == "out"
        ));
        assert!(matches!(
            &snapshot.entries[5],
            Entry::Notice { warning: true, .. }
        ));
        assert_eq!(snapshot.entries[6], Entry::Error("bad".into()));
        assert!(snapshot.status.expect("status").running);
    }

    #[test]
    fn unknown_events_and_dialogs_are_placeholders() {
        let event = RemoteEvent::parse(json!({ "kind": "warp", "x": 1 })).unwrap();
        assert_eq!(event, RemoteEvent::Unknown);
        let event =
            RemoteEvent::parse(json!({ "kind": "dialog", "dialog": { "dialog": "hologram" } }))
                .unwrap();
        assert_eq!(
            event,
            RemoteEvent::Dialog {
                dialog: Some(Dialog::Unknown)
            }
        );
        let event = RemoteEvent::parse(json!({
            "kind": "set", "at": 2, "entry": { "Hologram": 1 }
        }))
        .unwrap();
        assert_eq!(
            event,
            RemoteEvent::Set {
                at: 2,
                entry: Entry::Other("Hologram".into())
            }
        );
    }

    #[test]
    fn an_approval_dialog_keeps_the_offered_choices() {
        let dialog: Dialog = serde_json::from_value(json!({
            "dialog": "approval", "id": 4, "tool": "bash", "agent": "", "agent_name": "",
            "summary": "ls", "reason": "r", "choices": ["allow once", "deny"],
            "preview": { "path": "a", "added": 1, "removed": 0, "hidden": 0,
                "lines": [{ "kind": "Added", "old_no": null, "new_no": 1, "text": "x" }] },
            "proposed_rule": null,
        }))
        .unwrap();
        let Dialog::Approval {
            id,
            choices,
            preview,
            ..
        } = dialog
        else {
            panic!("an approval");
        };
        assert_eq!(
            (id, choices),
            (4, vec!["allow once".to_string(), "deny".to_string()])
        );
        assert_eq!(preview.unwrap().lines[0].kind, DiffKind::Added);
    }
}
