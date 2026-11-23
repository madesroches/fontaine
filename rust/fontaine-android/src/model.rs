//! The app's own read-only mirror of what the host publishes. It is lenient on purpose: unknown
//! fields are ignored and unknown kinds decode to a placeholder, so a newer host never breaks the
//! app. A test in `fontaine-tui` serializes the host's real values and decodes them here.

use std::collections::BTreeMap;

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
    /// The model's reasoning: its settled text, or the live form while it streams.
    Reasoning {
        text: String,
        streaming: bool,
    },
    /// A `!` command the host's person ran, with the line saying how it ended in `output`.
    Shell {
        command: String,
        output: String,
    },
    /// A one-line marker naming a file a user message carried.
    Attachment(String),
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
            "Reasoning" => {
                let streaming = field("streaming").and_then(Value::as_bool).unwrap_or(false);
                // While it streams the settled text is the raw buffer and `live` its safe form.
                let text = if streaming {
                    text_of(field("live"))
                } else {
                    text_of(field("text"))
                };
                Entry::Reasoning { text, streaming }
            }
            "Shell" => Entry::Shell {
                command: text_of(field("command")),
                output: text_of(field("output")),
            },
            "Attachment" => Entry::Attachment(text_of(Some(body))),
            // A card printed in full by `/expand` is the call or reasoning it wraps.
            "Expanded" => Entry::from_value(body),
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

/// One agent of the session, as the host lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentSummary {
    /// `None` is the main conversation.
    pub id: Option<String>,
    pub name: String,
    pub parent: Option<String>,
    pub agent_type: String,
    pub model: String,
    pub status: String,
    pub depth: usize,
    pub spend: String,
    pub idle: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TodoItem {
    pub content: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
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
    pub queued: Vec<String>,
    pub todos: Vec<TodoItem>,
    pub background_shells: usize,
    pub background_monitors: usize,
    pub scheduled: usize,
    /// The active goal's age in seconds and whether it waits for the user.
    pub goal: Option<(u64, bool)>,
    /// Main first, then every subagent; empty from a host that does not publish agents.
    pub agents: Vec<AgentSummary>,
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
        /// The id of the agent that asked; empty is the main conversation.
        #[serde(default)]
        agent: String,
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
        agent: String,
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
        /// The subagent whose transcript this is; absent is the main conversation.
        #[serde(default)]
        agent: Option<String>,
    },
    Append {
        at: usize,
        text: String,
        #[serde(default)]
        streaming: bool,
        #[serde(default)]
        agent: Option<String>,
    },
    Truncate {
        len: usize,
        #[serde(default)]
        agent: Option<String>,
    },
    AgentGone {
        agent: String,
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
    pub agent_entries: BTreeMap<String, Vec<Entry>>,
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
        assert_eq!(
            snapshot.entries[3],
            Entry::Reasoning {
                text: "t".into(),
                streaming: false
            }
        );
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
                entry: Entry::Other("Hologram".into()),
                agent: None,
            }
        );
    }

    #[test]
    fn the_entry_kinds_the_host_added_decode_to_their_own_rows() {
        let entries = [
            json!({ "Reasoning": { "text": "raw", "live": "safe", "streaming": true,
                "clipped": false, "ended": false } }),
            json!({ "Shell": { "command": "ls", "output": "a\n[exit 0]", "exit_code": 0 } }),
            json!({ "Attachment": "notes.txt" }),
            json!({ "Expanded": { "Reasoning": { "text": "full", "live": "", "streaming": false,
                "clipped": false, "ended": true } } }),
        ]
        .map(|value| serde_json::from_value::<Entry>(value).unwrap());
        assert_eq!(
            entries[0],
            Entry::Reasoning {
                text: "safe".into(),
                streaming: true
            }
        );
        assert_eq!(
            entries[1],
            Entry::Shell {
                command: "ls".into(),
                output: "a\n[exit 0]".into()
            }
        );
        assert_eq!(entries[2], Entry::Attachment("notes.txt".into()));
        assert_eq!(
            entries[3],
            Entry::Reasoning {
                text: "full".into(),
                streaming: false
            }
        );
    }

    #[test]
    fn agent_fields_decode_and_unknown_ones_are_ignored() {
        let state = json!({
            "entries": [],
            "agent_entries": { "a1": [{ "User": "hi" }] },
            "status": { "agents": [
                { "id": null, "name": "main", "status": "running", "future": 1 },
                { "id": "a1", "name": "worker", "parent": null, "agent_type": "general",
                  "model": "m", "status": "idle", "depth": 1, "spend": "3 tok", "idle": true },
            ], "queued": ["next"], "goal": [90, false],
            "todos": [{ "content": "x", "status": "in_progress" }] },
            "dialog": null,
        });
        let snapshot = Snapshot::parse(&state.to_string()).unwrap();
        assert_eq!(snapshot.agent_entries["a1"], vec![Entry::User("hi".into())]);
        let status = snapshot.status.unwrap();
        assert_eq!(status.agents.len(), 2);
        assert_eq!(status.agents[0].id, None);
        assert_eq!(status.agents[1].id.as_deref(), Some("a1"));
        assert!(status.agents[1].idle);
        assert_eq!(status.queued, ["next"]);
        assert_eq!(status.goal, Some((90, false)));
        assert_eq!(status.todos[0].status, "in_progress");
    }

    #[test]
    fn events_without_an_agent_are_the_main_conversation_and_agent_gone_decodes() {
        let event = RemoteEvent::parse(json!({ "kind": "truncate", "len": 1 })).unwrap();
        assert_eq!(
            event,
            RemoteEvent::Truncate {
                len: 1,
                agent: None
            }
        );
        let event =
            RemoteEvent::parse(json!({ "kind": "append", "at": 0, "text": "x", "agent": "a1" }))
                .unwrap();
        assert!(matches!(event, RemoteEvent::Append { agent: Some(a), .. } if a == "a1"));
        let event = RemoteEvent::parse(json!({ "kind": "agent_gone", "agent": "a1" })).unwrap();
        assert_eq!(event, RemoteEvent::AgentGone { agent: "a1".into() });
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
