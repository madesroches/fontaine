//! What a device renders from: a serde mirror of what the host's interface loop shows. The host
//! never ships rendered frames; it ships the transcript's entries, the status-bar state and the
//! one dialog that wants an answer, and the device paints them with the same renderers.
//!
//! The mirror is computed by comparing the host's `App` with what was last published, so every
//! change the loop makes reaches devices without a second render path.

use fontaine_sdk::core::preview::DiffPreview;
use fontaine_sdk::tools::ask_user::Question;
use fontaine_sdk::tools::todo::TodoItem;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::transcript::{Entry, ToolOutput};

/// Longest text field of a mirrored entry. A longer one is cut, and says so, so one frame can
/// never outgrow the host's frame cap.
pub(crate) const FIELD_CAP: usize = 256 * 1024;

const CUT_MARK: &str = "\n… [cut for the remote device]";

fn cap(text: &mut String) {
    if text.len() > FIELD_CAP {
        let mut end = FIELD_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(CUT_MARK);
    }
}

fn cap_preview(preview: &mut DiffPreview) {
    cap(&mut preview.path);
    for line in &mut preview.lines {
        cap(&mut line.text);
    }
}

/// `dialog` with every text field within `FIELD_CAP`.
fn capped_dialog(mut dialog: Dialog) -> Dialog {
    match &mut dialog {
        Dialog::Approval {
            tool,
            agent,
            agent_name,
            summary,
            reason,
            choices,
            preview,
            proposed_rule,
            ..
        } => {
            for text in [tool, agent, agent_name, summary, reason] {
                cap(text);
            }
            choices.iter_mut().for_each(cap);
            if let Some(preview) = preview {
                cap_preview(preview);
            }
            if let Some(rule) = proposed_rule {
                cap(rule);
            }
        }
        Dialog::Question {
            agent, questions, ..
        } => {
            cap(agent);
            for question in questions {
                cap(&mut question.question);
                cap(&mut question.header);
                for option in &mut question.options {
                    cap(&mut option.label);
                    cap(&mut option.description);
                }
            }
        }
        Dialog::HostOnly { what } => cap(what),
    }
    dialog
}

/// `entry` with every text field within `FIELD_CAP`.
pub(crate) fn capped(entry: &Entry) -> Entry {
    let mut entry = entry.clone();
    match &mut entry {
        Entry::User(text) | Entry::Attachment(text) | Entry::Error(text) => cap(text),
        Entry::Assistant { text, .. } => cap(text),
        Entry::Tool {
            summary,
            call,
            output,
            diff,
            ..
        } => {
            cap(summary);
            cap(call);
            if let Some(diff) = diff {
                cap_preview(diff);
            }
            if let Some(ToolOutput { text, .. }) = output {
                cap(text);
            }
        }
        Entry::Reasoning { text, live, .. } => {
            cap(text);
            cap(live);
        }
        Entry::Expanded(inner) => **inner = capped(inner),
        Entry::Notice { headline, hint, .. } => {
            cap(headline);
            if let Some(hint) = hint {
                cap(hint);
            }
        }
        Entry::Shell {
            command, output, ..
        } => {
            cap(command);
            cap(output);
        }
    }
    entry
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Status {
    pub mode: String,
    pub model: String,
    pub effort: Option<String>,
    pub running: bool,
    pub afk: bool,
    pub afk_stopped: bool,
    pub guardian_off: bool,
    pub queued: Vec<String>,
    pub todos: Vec<TodoItem>,
    pub cwd: String,
    pub branch: Option<String>,
    pub running_agents: usize,
    #[serde(default)]
    pub background_shells: usize,
    #[serde(default)]
    pub background_monitors: usize,
    pub scheduled: usize,
    /// The active goal's age in seconds and whether it waits for the user.
    pub goal: Option<(u64, bool)>,
}

impl Status {
    pub(crate) fn of(app: &App) -> Status {
        Status {
            mode: app.mode.as_str().to_string(),
            model: app.model.clone(),
            effort: app.effort.map(|effort| effort.to_string()),
            running: app.running,
            afk: app.afk,
            afk_stopped: app.afk_stopped,
            guardian_off: app.guardian_off,
            queued: app.queued.clone(),
            todos: app.todos.items().to_vec(),
            cwd: app.cwd.display().to_string(),
            branch: app.branch.clone(),
            running_agents: app.running_agents,
            background_shells: app.background_shells,
            background_monitors: app.background_monitors,
            scheduled: app.scheduled,
            goal: app.goal.map(|(age, waiting)| (age.as_secs(), waiting)),
        }
    }
}

/// The one dialog a device is shown. An approval and a question can be answered from the device;
/// everything else is the host's alone and reaches the device as a "waiting on the host" line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dialog", rename_all = "snake_case")]
pub(crate) enum Dialog {
    Approval {
        id: u64,
        tool: String,
        agent: String,
        agent_name: String,
        summary: String,
        reason: String,
        /// The labels the host's prompt offers; a device may pick only once, session or deny.
        choices: Vec<String>,
        preview: Option<DiffPreview>,
        proposed_rule: Option<String>,
    },
    Question {
        id: u64,
        agent: String,
        /// The sub-question showing now, from 0.
        index: usize,
        questions: Vec<Question>,
    },
    HostOnly {
        what: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RemoteEvent {
    /// Entry `at` is now `entry`; `at` may be one past the end.
    Set {
        at: usize,
        entry: Entry,
    },
    /// Streamed text added to the assistant entry at `at`.
    Append {
        at: usize,
        text: String,
        streaming: bool,
    },
    Truncate {
        len: usize,
    },
    Status {
        status: Box<Status>,
    },
    Dialog {
        dialog: Option<Dialog>,
    },
}

/// Everything a device needs to start from, as of the latest published event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub entries: Vec<Entry>,
    pub status: Option<Status>,
    pub dialog: Option<Dialog>,
}

/// What was last published, and the comparison that finds what to publish next.
pub(crate) struct MirrorTracker {
    entries: Vec<Entry>,
    status: Option<Status>,
    dialog: Option<Dialog>,
    /// The transcript's change counter at the last comparison, to skip an unchanged transcript.
    changes: Option<u64>,
    /// Whether AFK was on when devices were last told.
    pub afk_told: bool,
}

impl MirrorTracker {
    pub(crate) fn new(afk: bool) -> MirrorTracker {
        MirrorTracker {
            entries: Vec::new(),
            status: None,
            dialog: None,
            changes: None,
            afk_told: afk,
        }
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self.entries.clone(),
            status: self.status.clone(),
            dialog: self.dialog.clone(),
        }
    }

    /// The events that bring a device up to date with `app`, in order.
    pub(crate) fn diff(&mut self, app: &App) -> Vec<RemoteEvent> {
        let mut events = Vec::new();
        let changes = app.transcript.changes();
        if self.changes != Some(changes) {
            self.changes = Some(changes);
            self.diff_entries(app, &mut events);
        }
        let status = Status::of(app);
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            events.push(RemoteEvent::Status {
                status: Box::new(status),
            });
        }
        let dialog = app.device_dialog().map(capped_dialog);
        if self.dialog != dialog {
            self.dialog = dialog.clone();
            events.push(RemoteEvent::Dialog { dialog });
        }
        events
    }

    fn diff_entries(&mut self, app: &App, events: &mut Vec<RemoteEvent>) {
        let current: Vec<Entry> = app
            .transcript
            .mirror_entries()
            .into_iter()
            .map(capped)
            .collect();
        if current.len() < self.entries.len() {
            events.push(RemoteEvent::Truncate { len: current.len() });
            self.entries.truncate(current.len());
        }
        for (at, entry) in current.into_iter().enumerate() {
            match self.entries.get(at) {
                Some(old) if *old == entry => continue,
                Some(Entry::Assistant { text: old, .. }) => {
                    if let Entry::Assistant { text, streaming } = &entry
                        && text.len() > old.len()
                        && text.starts_with(old.as_str())
                    {
                        events.push(RemoteEvent::Append {
                            at,
                            text: text[old.len()..].to_string(),
                            streaming: *streaming,
                        });
                        self.entries[at] = entry;
                        continue;
                    }
                }
                _ => {}
            }
            events.push(RemoteEvent::Set {
                at,
                entry: entry.clone(),
            });
            if at < self.entries.len() {
                self.entries[at] = entry;
            } else {
                self.entries.push(entry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fontaine_sdk::core::config::PermissionMode;
    use fontaine_sdk::core::turn::SessionEvent;
    use serde_json::json;

    use super::*;
    use crate::theme::Theme;

    fn host() -> App {
        App::new(PermissionMode::Manual, "m".into())
    }

    fn lines(app: &App) -> Vec<String> {
        app.transcript
            .lines(60, &Theme::preset(Default::default()))
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    /// Applies what the host published to a device's app, through the wire encoding.
    fn deliver(client: &mut App, events: Vec<RemoteEvent>) {
        for event in events {
            let wire = serde_json::to_value(&event).expect("an event serializes");
            let back: RemoteEvent = serde_json::from_value(wire).expect("an event parses");
            assert_eq!(back, event);
            crate::remote::client_ui::apply_event(client, back);
        }
    }

    #[test]
    fn a_device_built_only_from_events_renders_the_host_transcript() {
        let mut host = host();
        let mut client = host_like();
        let mut tracker = MirrorTracker::new(false);
        let step = |host: &mut App, client: &mut App, tracker: &mut MirrorTracker| {
            deliver(client, tracker.diff(host));
            assert_eq!(lines(host), lines(client));
        };
        host.transcript.user("make a note".into());
        step(&mut host, &mut client, &mut tracker);
        for delta in ["Sure, ", "writing ", "it now."] {
            host.on_event(SessionEvent::AssistantDelta(delta.into()));
            step(&mut host, &mut client, &mut tracker);
        }
        host.on_event(SessionEvent::ToolCall {
            id: "c1".into(),
            tool: "write".into(),
            args: json!({ "path": "notes.txt", "content": "hello" }),
        });
        step(&mut host, &mut client, &mut tracker);
        host.on_event(SessionEvent::ToolResult {
            id: "c1".into(),
            tool: "write".into(),
            success: true,
            text: "wrote 5 bytes".into(),
        });
        step(&mut host, &mut client, &mut tracker);
        host.on_event(SessionEvent::ReasoningDelta("thinking about it".into()));
        step(&mut host, &mut client, &mut tracker);
        host.on_event(SessionEvent::Reasoning("thought about it".into()));
        host.transcript.error("a failure");
        host.transcript.notice("a note");
        host.transcript
            .shell("ls".into(), "a\nb\n[exit 0]".into(), Some(0));
        step(&mut host, &mut client, &mut tracker);
        host.on_event(SessionEvent::Assistant("done".into()));
        step(&mut host, &mut client, &mut tracker);
        // A shorter transcript (a rewind, a clear) reaches the device as a truncation.
        host.transcript.entries.truncate(2);
        host.transcript.touch();
        step(&mut host, &mut client, &mut tracker);
        assert_eq!(client.transcript.entries.len(), 2);
    }

    fn host_like() -> App {
        host()
    }

    #[test]
    fn a_snapshot_reproduces_the_transcript_for_a_device_that_joins_late() {
        let mut host = host();
        let mut tracker = MirrorTracker::new(false);
        host.transcript.user("first".into());
        host.on_event(SessionEvent::AssistantDelta("answer".into()));
        host.on_event(SessionEvent::ToolCall {
            id: "c".into(),
            tool: "shell".into(),
            args: json!({ "command": "ls" }),
        });
        tracker.diff(&host);
        let wire = serde_json::to_string(&tracker.snapshot()).unwrap();
        let mut late = host_like();
        crate::remote::client_ui::apply_snapshot(&mut late, serde_json::from_str(&wire).unwrap());
        assert_eq!(lines(&host), lines(&late));
        // And events after it keep it in step.
        host.on_event(SessionEvent::ToolResult {
            id: "c".into(),
            tool: "shell".into(),
            success: true,
            text: "x".into(),
        });
        deliver(&mut late, tracker.diff(&host));
        assert_eq!(lines(&host), lines(&late));
    }

    #[test]
    fn status_and_dialog_changes_are_events_and_an_unchanged_app_is_silent() {
        let mut host = host();
        let mut tracker = MirrorTracker::new(false);
        let first = tracker.diff(&host);
        assert!(
            first
                .iter()
                .any(|e| matches!(e, RemoteEvent::Status { .. }))
        );
        assert!(tracker.diff(&host).is_empty());
        host.running = true;
        host.model = "other".into();
        let events = tracker.diff(&host);
        assert_eq!(events.len(), 1);
        let mut client = host_like();
        deliver(&mut client, first);
        deliver(&mut client, events);
        assert!(client.running && client.model == "other");
    }

    #[test]
    fn the_background_job_counts_round_trip_and_default_when_absent() {
        let mut app = host();
        app.background_shells = 2;
        app.background_monitors = 1;
        let status = Status::of(&app);
        let wire = serde_json::to_value(&status).expect("a status serializes");
        let back: Status = serde_json::from_value(wire.clone()).expect("a status parses");
        assert_eq!((back.background_shells, back.background_monitors), (2, 1));

        let mut old = wire;
        let fields = old.as_object_mut().expect("a status is an object");
        fields.remove("background_shells");
        fields.remove("background_monitors");
        let back: Status = serde_json::from_value(old).expect("an older status parses");
        assert_eq!((back.background_shells, back.background_monitors), (0, 0));
    }

    #[test]
    fn an_oversized_text_field_is_cut_and_marked() {
        let mut host = host();
        host.transcript.user("x".repeat(FIELD_CAP + 1000));
        let mut tracker = MirrorTracker::new(false);
        let events = tracker.diff(&host);
        let Some(RemoteEvent::Set {
            entry: Entry::User(text),
            ..
        }) = events.first()
        else {
            panic!("a user entry");
        };
        assert!(text.len() < FIELD_CAP + CUT_MARK.len() + 8);
        assert!(text.ends_with(CUT_MARK));
        let frame = serde_json::to_vec(&events[0]).unwrap();
        assert!(frame.len() < fontaine_remote::HOST_FRAME_CAP);
    }

    #[test]
    fn events_round_trip_through_json() {
        let event = RemoteEvent::Set {
            at: 3,
            entry: Entry::Tool {
                id: "i".into(),
                tool: "shell".into(),
                summary: "ls".into(),
                status: crate::transcript::ToolStatus::Denied("no".into()),
                ordinal: 2,
                call: "ls".into(),
                output: Some(ToolOutput {
                    text: "t".into(),
                    clipped: true,
                }),
                diff: None,
            },
        };
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<RemoteEvent>(&text).unwrap(), event);
        let dialog = RemoteEvent::Dialog {
            dialog: Some(Dialog::HostOnly { what: "x".into() }),
        };
        let text = serde_json::to_string(&dialog).unwrap();
        assert_eq!(serde_json::from_str::<RemoteEvent>(&text).unwrap(), dialog);
    }
}
