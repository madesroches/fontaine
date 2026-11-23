//! What a device renders from: a serde mirror of what the host's interface loop shows. The host
//! never ships rendered frames; it ships the transcript's entries, the status-bar state and the
//! one dialog that wants an answer, and the device paints them with the same renderers.
//!
//! The mirror is computed by comparing the host's `App` with what was last published, so every
//! change the loop makes reaches devices without a second render path.

use std::collections::BTreeMap;

use fontaine_sdk::core::preview::DiffPreview;
use fontaine_sdk::tools::ask_user::Question;
use fontaine_sdk::tools::todo::TodoItem;
use serde::{Deserialize, Serialize};

use crate::agent_list::depth_first;
use crate::agents_panel::{SpendView, row_label, spend_text};
use crate::app::App;
use crate::transcript::{Entry, ToolOutput, Transcript};

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

/// One agent of the session as a device lists it. The host publishes every agent in the tree,
/// whether or not its own terminal lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentSummary {
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

/// Every agent of the tree in tree order, for the host to store where `Status::of` can read it.
pub(crate) fn summarize_agents(rows: &[fontaine_sdk::agents::tree::AgentRow]) -> Vec<AgentSummary> {
    depth_first(rows)
        .into_iter()
        .map(|row| AgentSummary {
            id: Some(row.id.clone()),
            name: row.name.clone(),
            parent: row.parent.clone(),
            agent_type: row.agent_type.clone(),
            model: row.model.clone(),
            status: row_label(row.status, row.idle).to_string(),
            depth: row.depth,
            spend: spend_text(&SpendView {
                tokens: row.spend.tokens(),
                token_budget: row.budget.tokens,
                cost_usd: row.spend.cost_usd,
                cost_budget: row.budget.cost_usd,
                unpriced_tokens: row.spend.unpriced_tokens,
                estimated: row.spend.estimated,
            }),
            idle: row.idle,
        })
        .collect()
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
    /// Main first, then every subagent; empty while the session has no subagent.
    #[serde(default)]
    pub agents: Vec<AgentSummary>,
}

impl Status {
    pub(crate) fn of(app: &App) -> Status {
        let agents = if app.agent_summaries.is_empty() {
            Vec::new()
        } else {
            let main = AgentSummary {
                id: None,
                name: "main".to_string(),
                parent: None,
                agent_type: String::new(),
                model: app.model.clone(),
                status: if app.running { "running" } else { "idle" }.to_string(),
                depth: 0,
                spend: String::new(),
                idle: !app.running,
            };
            std::iter::once(main)
                .chain(app.agent_summaries.iter().cloned())
                .collect()
        };
        Status {
            agents,
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
        /// The subagent whose transcript this is; absent is the main conversation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// Streamed text added to the assistant entry at `at`.
    Append {
        at: usize,
        text: String,
        streaming: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    Truncate {
        len: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// The host dropped this agent's transcript, as `/clear` does.
    AgentGone {
        agent: String,
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
    /// Each subagent's whole transcript, by agent id.
    #[serde(default)]
    pub agent_entries: BTreeMap<String, Vec<Entry>>,
    pub status: Option<Status>,
    pub dialog: Option<Dialog>,
}

/// What was last published of one transcript.
#[derive(Default)]
struct Mirrored {
    entries: Vec<Entry>,
    /// The transcript's change counter at the last comparison, to skip an unchanged transcript.
    changes: Option<u64>,
}

/// What was last published, and the comparison that finds what to publish next.
pub(crate) struct MirrorTracker {
    main: Mirrored,
    agents: BTreeMap<String, Mirrored>,
    status: Option<Status>,
    dialog: Option<Dialog>,
    /// Whether AFK was on when devices were last told.
    pub afk_told: bool,
}

impl MirrorTracker {
    pub(crate) fn new(afk: bool) -> MirrorTracker {
        MirrorTracker {
            main: Mirrored::default(),
            agents: BTreeMap::new(),
            status: None,
            dialog: None,
            afk_told: afk,
        }
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self.main.entries.clone(),
            agent_entries: self
                .agents
                .iter()
                .map(|(id, mirrored)| (id.clone(), mirrored.entries.clone()))
                .collect(),
            status: self.status.clone(),
            dialog: self.dialog.clone(),
        }
    }

    /// The events that bring a device up to date with `app`, in order.
    pub(crate) fn diff(&mut self, app: &App) -> Vec<RemoteEvent> {
        let mut events = Vec::new();
        diff_transcript(&app.transcript, &mut self.main, None, &mut events);
        let gone: Vec<String> = self
            .agents
            .keys()
            .filter(|id| !app.agent_views.contains_key(*id))
            .cloned()
            .collect();
        for agent in gone {
            self.agents.remove(&agent);
            events.push(RemoteEvent::AgentGone { agent });
        }
        let mut views: Vec<_> = app.agent_views.iter().collect();
        views.sort_by_key(|(id, _)| id.as_str());
        for (id, view) in views {
            let mirrored = self.agents.entry(id.clone()).or_default();
            diff_transcript(&view.transcript, mirrored, Some(id), &mut events);
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
}

/// Publishes what changed in `transcript` since `mirrored`, tagged with `agent`.
fn diff_transcript(
    transcript: &Transcript,
    mirrored: &mut Mirrored,
    agent: Option<&str>,
    events: &mut Vec<RemoteEvent>,
) {
    let changes = transcript.changes();
    if mirrored.changes == Some(changes) {
        return;
    }
    mirrored.changes = Some(changes);
    let agent = || agent.map(str::to_string);
    let current: Vec<Entry> = transcript
        .mirror_entries()
        .into_iter()
        .map(capped)
        .collect();
    let entries = &mut mirrored.entries;
    if current.len() < entries.len() {
        events.push(RemoteEvent::Truncate {
            len: current.len(),
            agent: agent(),
        });
        entries.truncate(current.len());
    }
    for (at, entry) in current.into_iter().enumerate() {
        match entries.get(at) {
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
                        agent: agent(),
                    });
                    entries[at] = entry;
                    continue;
                }
            }
            _ => {}
        }
        events.push(RemoteEvent::Set {
            at,
            entry: entry.clone(),
            agent: agent(),
        });
        if at < entries.len() {
            entries[at] = entry;
        } else {
            entries.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use fontaine_sdk::core::config::PermissionMode;
    use fontaine_sdk::core::turn::SessionEvent;
    use serde_json::json;
    use tokio::sync::oneshot;

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

    // The Android app decodes what the host publishes with its own lenient mirror; this fails when
    // a host change would leave the app unable to read what it renders.
    #[test]
    fn the_android_mirror_decodes_what_the_host_publishes() {
        use fontaine_android::model as app;
        use fontaine_sdk::core::permissions::{ApprovalRequest, Offers};
        use fontaine_sdk::core::tier::Tier;
        use fontaine_sdk::tools::ask_user::{Question, QuestionOption};

        let mut host = host();
        let mut tracker = MirrorTracker::new(false);
        host.transcript.user("make a note".into());
        host.on_event(SessionEvent::AssistantDelta("Sure".into()));
        host.on_event(SessionEvent::ToolCall {
            id: "c1".into(),
            tool: "write".into(),
            args: json!({ "path": "notes.txt", "content": "hello" }),
        });
        host.on_event(SessionEvent::ToolResult {
            id: "c1".into(),
            tool: "write".into(),
            success: false,
            text: "denied by the floor".into(),
        });
        host.on_event(SessionEvent::ToolCall {
            id: "c2".into(),
            tool: "shell".into(),
            args: json!({ "command": "ls" }),
        });
        host.on_event(SessionEvent::Reasoning("thought about it".into()));
        host.transcript.error("a failure");
        host.transcript.notice("a note");
        host.transcript
            .shell("ls".into(), "a\n[exit 0]".into(), Some(0));
        host.transcript
            .entries
            .push(Entry::Attachment("notes.txt".into()));
        host.transcript.touch();
        host.view_of("a1".into())
            .transcript
            .user("for the agent".into());
        host.agent_summaries = vec![AgentSummary {
            id: Some("a1".into()),
            name: "worker".into(),
            parent: None,
            agent_type: "general".into(),
            model: "m".into(),
            status: "running".into(),
            depth: 1,
            spend: "1k tok".into(),
            idle: false,
        }];
        host.running = true;
        let (reply, _answer) = oneshot::channel();
        host.begin_approval(
            ApprovalRequest {
                tool: "write".into(),
                agent: String::new(),
                agent_name: "helper".into(),
                tool_use_id: None,
                args: serde_json::Value::Null,
                summary: "notes.txt".into(),
                tier: Tier::Human,
                reason: "it writes".into(),
                offers: Offers {
                    session: true,
                    always: true,
                },
                proposed_rule: None,
                preview: None,
            },
            reply,
        );
        tracker.diff(&host);

        let snapshot = tracker.snapshot();
        let wire = serde_json::to_string(&snapshot).expect("a snapshot serializes");
        let mirrored = app::Snapshot::parse(&wire).expect("the app decodes a host snapshot");
        assert_eq!(mirrored.entries.len(), snapshot.entries.len());
        assert!(
            mirrored
                .entries
                .iter()
                .all(|e| !matches!(e, app::Entry::Other(_))),
            "{:?}",
            mirrored.entries
        );
        assert!(mirrored.entries.iter().any(
            |e| matches!(e, app::Entry::Reasoning { text, .. } if text == "thought about it")
        ));
        assert!(
            mirrored
                .entries
                .iter()
                .any(|e| matches!(e, app::Entry::Shell { command, .. } if command == "ls"))
        );
        assert!(
            mirrored
                .entries
                .iter()
                .any(|e| matches!(e, app::Entry::Attachment(n) if n == "notes.txt"))
        );
        assert_eq!(
            mirrored.agent_entries["a1"],
            vec![app::Entry::User("for the agent".into())]
        );
        assert_eq!(mirrored.entries[0], app::Entry::User("make a note".into()));
        assert!(matches!(
            &mirrored.entries[1],
            app::Entry::Assistant { text, .. } if text == "Sure"
        ));
        let failed = mirrored.entries.iter().find_map(|e| match e {
            app::Entry::Tool {
                tool,
                status,
                output,
                ..
            } if tool == "write" => Some((status.clone(), output.clone())),
            _ => None,
        });
        let (status, output) = failed.expect("the write call is mirrored");
        assert!(matches!(status, app::ToolStatus::Failed(_)), "{status:?}");
        assert_eq!(output.as_deref(), Some("denied by the floor"));
        assert!(mirrored.entries.iter().any(|e| matches!(
            e,
            app::Entry::Tool { tool, status: app::ToolStatus::Pending, .. } if tool == "shell"
        )));
        assert!(
            mirrored
                .entries
                .iter()
                .any(|e| matches!(e, app::Entry::Error(t) if t == "a failure"))
        );
        assert!(
            mirrored
                .entries
                .iter()
                .any(|e| matches!(e, app::Entry::Notice { headline, .. } if headline == "a note"))
        );
        let status = mirrored.status.expect("a status");
        assert!(status.running);
        assert_eq!(status.model, "m");
        assert_eq!(status.agents.len(), 2);
        assert_eq!(status.agents[0].id, None);
        assert_eq!(status.agents[1].id.as_deref(), Some("a1"));
        assert_eq!(status.agents[1].status, "running");
        assert_eq!(status.agents[1].spend, "1k tok");
        let Some(app::Dialog::Approval {
            id,
            choices,
            agent_name,
            ..
        }) = mirrored.dialog
        else {
            panic!("the approval is mirrored");
        };
        assert_ne!(id, 0);
        assert_eq!(agent_name, "helper");
        assert!(
            choices.iter().any(|c| c == "allow once")
                && !choices.iter().any(|c| c == "always allow")
        );

        // Every event kind the host sends, through the same encoding the wire uses.
        let events = [
            RemoteEvent::Set {
                at: 0,
                entry: Entry::User("x".into()),
                agent: None,
            },
            RemoteEvent::Set {
                at: 0,
                entry: Entry::User("x".into()),
                agent: Some("a1".into()),
            },
            RemoteEvent::Append {
                at: 1,
                text: "more".into(),
                streaming: true,
                agent: None,
            },
            RemoteEvent::Truncate {
                len: 2,
                agent: None,
            },
            RemoteEvent::AgentGone { agent: "a1".into() },
            RemoteEvent::Status {
                status: Box::new(Status::of(&host)),
            },
            RemoteEvent::Dialog { dialog: None },
            RemoteEvent::Dialog {
                dialog: Some(Dialog::HostOnly {
                    what: "a device pairing".into(),
                }),
            },
            RemoteEvent::Dialog {
                dialog: Some(Dialog::Question {
                    id: 2,
                    agent: String::new(),
                    index: 0,
                    questions: vec![Question {
                        question: "which?".into(),
                        header: "h".into(),
                        options: vec![QuestionOption {
                            label: "a".into(),
                            description: "d".into(),
                        }],
                        multi_select: false,
                    }],
                }),
            },
        ];
        for event in events {
            let value = serde_json::to_value(&event).expect("an event serializes");
            let mirrored = app::RemoteEvent::parse(value).expect("the app decodes a host event");
            assert_ne!(mirrored, app::RemoteEvent::Unknown, "{event:?}");
        }
        let value = serde_json::to_value(RemoteEvent::Dialog {
            dialog: Some(Dialog::Question {
                id: 2,
                agent: String::new(),
                index: 0,
                questions: vec![Question {
                    question: "which?".into(),
                    header: "h".into(),
                    options: vec![QuestionOption {
                        label: "a".into(),
                        description: "d".into(),
                    }],
                    multi_select: true,
                }],
            }),
        })
        .unwrap();
        let Ok(app::RemoteEvent::Dialog {
            dialog: Some(app::Dialog::Question { questions, .. }),
        }) = app::RemoteEvent::parse(value)
        else {
            panic!("the question is mirrored");
        };
        assert!(questions[0].multi_select);
    }

    fn host_like() -> App {
        host()
    }

    fn lines_of(transcript: &Transcript) -> Vec<String> {
        transcript
            .lines(60, &Theme::preset(Default::default()))
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    fn with_two_agents(host: &mut App) {
        for (id, said) in [("a1", "first agent"), ("a2", "second agent")] {
            let view = host.view_of(id.into());
            view.transcript.user(format!("task for {id}"));
            view.transcript.error(said);
        }
    }

    #[test]
    fn a_device_built_from_events_renders_every_agents_transcript() {
        let mut host = host();
        let mut client = host_like();
        let mut tracker = MirrorTracker::new(false);
        host.transcript.user("main".into());
        with_two_agents(&mut host);
        deliver(&mut client, tracker.diff(&host));
        for id in ["a1", "a2"] {
            assert_eq!(
                lines_of(&host.agent_views[id].transcript),
                lines_of(&client.agent_views[id].transcript)
            );
        }
        assert_eq!(lines(&host), lines(&client));
        // A change to one agent publishes only that agent's events, tagged with it.
        host.view_of("a2".into()).transcript.notice("later");
        let events = tracker.diff(&host);
        assert!(events.iter().all(|e| matches!(
            e,
            RemoteEvent::Set { agent: Some(a), .. } if a == "a2"
        )));
        assert_eq!(events.len(), 1);
        deliver(&mut client, events);
        assert_eq!(
            lines_of(&host.agent_views["a2"].transcript),
            lines_of(&client.agent_views["a2"].transcript)
        );
        // Rewinding one agent truncates only it.
        host.view_of("a1".into()).transcript.entries.truncate(1);
        host.view_of("a1".into()).transcript.touch();
        let events = tracker.diff(&host);
        assert!(matches!(
            events.as_slice(),
            [RemoteEvent::Truncate { len: 1, agent: Some(a) }] if a == "a1"
        ));
        deliver(&mut client, events);
        assert_eq!(client.agent_views["a1"].transcript.entries.len(), 1);
    }

    #[test]
    fn a_main_event_has_no_agent_key_on_the_wire() {
        let event = RemoteEvent::Truncate {
            len: 1,
            agent: None,
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"kind":"truncate","len":1}"#
        );
        let old: RemoteEvent =
            serde_json::from_str(r#"{"kind":"set","at":0,"entry":{"User":"x"}}"#).unwrap();
        assert_eq!(
            old,
            RemoteEvent::Set {
                at: 0,
                entry: Entry::User("x".into()),
                agent: None
            }
        );
    }

    #[test]
    fn a_snapshot_with_agents_round_trips_through_the_assembler_and_renders_the_same() {
        use fontaine_remote::HostMsg;
        use fontaine_remote::protocol::{SnapshotAssembler, split_snapshot};

        let mut host = host();
        let mut tracker = MirrorTracker::new(false);
        host.transcript.user("main".into());
        with_two_agents(&mut host);
        tracker.diff(&host);
        let json = serde_json::to_string(&tracker.snapshot()).unwrap();
        let mut assembler = SnapshotAssembler::default();
        let mut joined = None;
        for part in split_snapshot(1, 1, &json, 256) {
            let HostMsg::Snapshot {
                epoch,
                seq,
                part,
                of,
                state,
            } = part
            else {
                panic!("a snapshot part");
            };
            joined = assembler.push(epoch, seq, part, of, state).or(joined);
        }
        let (_, _, state) = joined.expect("every part arrived");
        let mut late = host_like();
        crate::remote::client_ui::apply_snapshot(&mut late, serde_json::from_str(&state).unwrap());
        for id in ["a1", "a2"] {
            assert_eq!(
                lines_of(&host.agent_views[id].transcript),
                lines_of(&late.agent_views[id].transcript)
            );
        }
        // An old host's snapshot has no agent entries and still parses.
        let old: Snapshot = serde_json::from_str(r#"{"entries":[],"status":null,"dialog":null}"#)
            .expect("an older snapshot parses");
        assert!(old.agent_entries.is_empty());
    }

    #[test]
    fn dropping_the_agent_views_publishes_agent_gone_and_the_device_returns_to_main() {
        let mut host = host();
        let mut client = host_like();
        let mut tracker = MirrorTracker::new(false);
        with_two_agents(&mut host);
        deliver(&mut client, tracker.diff(&host));
        client.view(Some("a1".into()));
        host.agent_views.clear();
        let events = tracker.diff(&host);
        let mut gone: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                RemoteEvent::AgentGone { agent } => Some(agent.as_str()),
                _ => None,
            })
            .collect();
        gone.sort_unstable();
        assert_eq!(gone, ["a1", "a2"]);
        deliver(&mut client, events);
        assert!(client.agent_views.is_empty());
        assert!(client.viewing.is_none());
        assert!(tracker.snapshot().agent_entries.is_empty());
    }

    #[test]
    fn the_status_lists_every_agent_with_main_first_and_none_while_there_is_no_subagent() {
        let mut host = host();
        assert!(Status::of(&host).agents.is_empty());
        host.agent_summaries = vec![AgentSummary {
            id: Some("a1".into()),
            name: "worker".into(),
            parent: None,
            agent_type: "general".into(),
            model: "m".into(),
            status: "running".into(),
            depth: 1,
            spend: "3 tok".into(),
            idle: false,
        }];
        let status = Status::of(&host);
        assert_eq!(status.agents.len(), 2);
        assert_eq!(status.agents[0].id, None);
        assert_eq!(status.agents[1].id.as_deref(), Some("a1"));
        let wire = serde_json::to_value(&status).unwrap();
        let back: Status = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back.agents, status.agents);
        let mut old = wire;
        old.as_object_mut().unwrap().remove("agents");
        assert!(
            serde_json::from_value::<Status>(old)
                .unwrap()
                .agents
                .is_empty()
        );
    }

    #[test]
    fn the_device_command_picker_is_exactly_the_hosts_device_list() {
        use crate::commands::{Command, builtin_menu, parse_command};

        let host_list: Vec<String> = builtin_menu()
            .into_iter()
            .filter(|entry| {
                parse_command(&format!("/{}", entry.name))
                    .is_some_and(|c| !matches!(c, Command::Other { .. }) && c.device_allowed())
            })
            .map(|entry| entry.name)
            .collect();
        let mut picker: Vec<String> = fontaine_android::commands::DEVICE_COMMANDS
            .iter()
            .map(|c| c.name.to_string())
            .collect();
        let mut host_list = host_list;
        picker.sort_unstable();
        host_list.sort_unstable();
        assert_eq!(picker, host_list);
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
            agent: Some("a1".into()),
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
