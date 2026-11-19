//! The session state machine behind the screens. Input events and `ClientEvent`s go in; `Action`s
//! for the network client and a borrowed `View` for the renderer come out. Nothing here touches
//! the network, the camera or the screen, so all of it runs under host tests.

use fontaine_remote::{
    AttachError, ClientEvent, ClientMsg, Level, MAX_MESSAGE_BYTES, Reason, StoredHost, Target,
    Ticket, WireAnswer, fresh_key, short_node,
};

use crate::link;
use crate::model::{Dialog, DiffPreview, Entry, Question, RemoteEvent, Snapshot, Status};

/// The choice the host refuses from a device, so it is never offered.
const ALWAYS: &str = "always allow";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Scan,
    /// A link was read; nothing connects until the person agrees.
    Confirm {
        host: String,
    },
    Pairing {
        host: String,
        /// Whether the host's own confirmation is what is awaited, as opposed to a reattach.
        first_time: bool,
    },
    Session,
    Error(String),
}

/// What the activity must do for the app.
pub enum Action {
    Start(Target),
    Send(ClientMsg),
    /// Drop the client and its connection.
    Stop,
}

pub struct App {
    screen: Screen,
    pending: Option<Box<Ticket>>,
    hosts: Vec<StoredHost>,
    entries: Vec<Entry>,
    status: Option<Status>,
    dialog: Option<Dialog>,
    /// Set once an answer went out, so a second tap cannot answer the same dialog twice; the
    /// host's next dialog event clears it.
    answer_sent: bool,
    reconnecting: Option<u64>,
    notice: Option<String>,
    session_tag: String,
    next_id: u64,
}

impl Default for App {
    fn default() -> App {
        App::new()
    }
}

/// A line of the transcript for the renderer to draw.
pub enum DialogView<'a> {
    Approval {
        title: String,
        summary: &'a str,
        reason: &'a str,
        preview: Option<&'a DiffPreview>,
        choices: Vec<&'a str>,
        /// False while reconnecting or once an answer is out.
        answerable: bool,
    },
    Question {
        question: &'a Question,
        answerable: bool,
        /// Why a multi-select or malformed question must be answered at the host.
        host_only_note: Option<&'static str>,
    },
    HostWaiting(String),
}

pub struct View<'a> {
    pub screen: &'a Screen,
    pub entries: &'a [Entry],
    pub status_line: String,
    pub running: bool,
    pub dialog: Option<DialogView<'a>>,
    /// Milliseconds until the next reconnect attempt, while the link is down.
    pub reconnecting_in_ms: Option<u64>,
    pub notice: Option<&'a str>,
    pub hosts: &'a [StoredHost],
}

impl App {
    pub fn new() -> App {
        let key = fresh_key();
        App {
            screen: Screen::Scan,
            pending: None,
            hosts: Vec::new(),
            entries: Vec::new(),
            status: None,
            dialog: None,
            answer_sent: false,
            reconnecting: None,
            notice: None,
            session_tag: key[..4].iter().map(|b| format!("{b:02x}")).collect(),
            next_id: 0,
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn set_hosts(&mut self, hosts: Vec<StoredHost>) {
        self.hosts = hosts;
    }

    /// For a failure outside the app's own state, such as the paired-host list not reading.
    pub fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    /// Reads scanned or pasted text. A link only asks to pair, so this connects to nothing: it
    /// moves to the confirmation, and the text itself is not kept.
    pub fn submit_link(&mut self, text: &str) {
        if !matches!(
            self.screen,
            Screen::Scan | Screen::Confirm { .. } | Screen::Error { .. }
        ) {
            return;
        }
        match link::parse(text) {
            Ok(parsed) => {
                self.notice = None;
                self.screen = Screen::Confirm { host: parsed.host };
                self.pending = Some(parsed.ticket);
            }
            Err(e) => {
                self.notice = Some(format!("that is not a Fontaine pairing code: {e}"));
                self.screen = Screen::Scan;
                self.pending = None;
            }
        }
    }

    pub fn confirm_pairing(&mut self) -> Option<Action> {
        let ticket = self.pending.take()?;
        let host = short_node(&ticket.node.id.to_string());
        self.reset_session();
        self.screen = Screen::Pairing {
            host,
            first_time: true,
        };
        Some(Action::Start(Target::Ticket(ticket)))
    }

    pub fn decline_pairing(&mut self) {
        self.pending = None;
        self.screen = Screen::Scan;
    }

    /// Reattaches to a paired host without scanning again.
    pub fn connect_host(&mut self, node: &str) -> Option<Action> {
        let host = self.hosts.iter().find(|h| h.node == node)?.clone();
        self.reset_session();
        self.screen = Screen::Pairing {
            host: short_node(&host.node),
            first_time: false,
        };
        Some(Action::Start(Target::Host(Box::new(host))))
    }

    /// Leaves the session or an error and returns to the scan screen.
    pub fn back_to_scan(&mut self) -> Option<Action> {
        let was_live = !matches!(self.screen, Screen::Scan | Screen::Error(_));
        self.pending = None;
        self.reset_session();
        self.screen = Screen::Scan;
        was_live.then_some(Action::Stop)
    }

    fn reset_session(&mut self) {
        self.entries.clear();
        self.status = None;
        self.dialog = None;
        self.answer_sent = false;
        self.reconnecting = None;
        self.notice = None;
    }

    fn message_id(&mut self) -> String {
        self.next_id += 1;
        format!("a-{}-{}", self.session_tag, self.next_id)
    }

    /// A prompt, or a slash line when it starts with `/` (the host's device list decides whether
    /// it runs).
    pub fn send_prompt(&mut self, text: &str) -> Option<Action> {
        if self.screen != Screen::Session {
            return None;
        }
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if text.len() > MAX_MESSAGE_BYTES {
            self.notice = Some(format!(
                "that message is over the {} KiB limit",
                MAX_MESSAGE_BYTES / 1024
            ));
            return None;
        }
        self.notice = None;
        let message_id = self.message_id();
        Some(Action::Send(if text.starts_with('/') {
            ClientMsg::Slash {
                message_id,
                line: text.to_string(),
            }
        } else {
            ClientMsg::Prompt {
                message_id,
                text: text.to_string(),
            }
        }))
    }

    pub fn interrupt(&mut self) -> Option<Action> {
        (self.screen == Screen::Session).then_some(Action::Send(ClientMsg::Interrupt))
    }

    fn can_answer(&self) -> bool {
        self.screen == Screen::Session && self.reconnecting.is_none() && !self.answer_sent
    }

    /// Answers the approval with one of the offered choices. "Always allow" is never sent, and a
    /// choice the host did not offer is ignored.
    pub fn answer_approval(&mut self, choice: &str, feedback: Option<String>) -> Option<Action> {
        if !self.can_answer() {
            return None;
        }
        let Some(Dialog::Approval { id, choices, .. }) = &self.dialog else {
            return None;
        };
        if choice.eq_ignore_ascii_case(ALWAYS) || !choices.iter().any(|c| c == choice) {
            return None;
        }
        let message = ClientMsg::ApprovalAnswer {
            id: *id,
            answer: WireAnswer {
                choice: choice.to_string(),
                feedback: feedback.filter(|f| !f.trim().is_empty()),
            },
        };
        self.answer_sent = true;
        Some(Action::Send(message))
    }

    /// Answers the question showing now with its option number `option`, one option per question.
    pub fn answer_question(&mut self, option: usize) -> Option<Action> {
        if !self.can_answer() {
            return None;
        }
        let Some(Dialog::Question {
            id,
            index,
            questions,
        }) = &self.dialog
        else {
            return None;
        };
        let question = questions.get(*index)?;
        if question.multi_select {
            return None;
        }
        let reply = question.options.get(option)?.label.clone();
        let message = ClientMsg::QuestionReply {
            id: *id,
            index: *index,
            options: question.options.iter().map(|o| o.label.clone()).collect(),
            reply,
        };
        self.answer_sent = true;
        Some(Action::Send(message))
    }

    /// Feeds one client event to the app and says what, if anything, the activity must do.
    pub fn on_client_event(&mut self, event: ClientEvent) -> Option<Action> {
        match event {
            ClientEvent::Paired => {
                if matches!(self.screen, Screen::Pairing { .. }) {
                    self.screen = Screen::Session;
                }
            }
            ClientEvent::Connected => {
                self.reconnecting = None;
                if matches!(
                    self.screen,
                    Screen::Pairing {
                        first_time: false,
                        ..
                    }
                ) {
                    self.screen = Screen::Session;
                }
            }
            ClientEvent::Snapshot { state, .. } => {
                if self.screen == Screen::Session {
                    match Snapshot::parse(&state) {
                        Ok(snapshot) => {
                            self.entries = snapshot.entries;
                            self.status = snapshot.status;
                            self.set_dialog(snapshot.dialog);
                        }
                        Err(_) => {
                            self.notice = Some("the session state could not be read".to_string());
                        }
                    }
                }
            }
            ClientEvent::Event { event, .. } => {
                if self.screen == Screen::Session {
                    match RemoteEvent::parse(event) {
                        Ok(event) => self.apply(event),
                        Err(_) => {
                            self.notice = Some("an update from the host was skipped".to_string());
                        }
                    }
                }
            }
            ClientEvent::Notice { severity, text } => {
                // The host sends a warning when it refuses an answer, and its dialog stays up.
                if severity == Level::Warning {
                    self.answer_sent = false;
                }
                self.notice = Some(match severity {
                    Level::Info => text,
                    Level::Warning => format!("host: {text}"),
                });
            }
            ClientEvent::Accepted { .. } => {}
            ClientEvent::PromptRejected { reason, .. } => {
                self.notice = Some(reason.describe().to_string());
            }
            ClientEvent::Reconnecting { after_ms } => {
                if self.screen == Screen::Session {
                    self.reconnecting = Some(after_ms);
                }
            }
            ClientEvent::Bye => {
                self.fail(Reason::Closed.describe().to_string());
                return Some(Action::Stop);
            }
            ClientEvent::Failed(error) => {
                self.fail(describe(&error));
                return Some(Action::Stop);
            }
        }
        None
    }

    fn fail(&mut self, message: String) {
        self.pending = None;
        self.reset_session();
        self.screen = Screen::Error(message);
    }

    fn set_dialog(&mut self, dialog: Option<Dialog>) {
        self.answer_sent = false;
        self.dialog = dialog;
    }

    fn apply(&mut self, event: RemoteEvent) {
        match event {
            RemoteEvent::Set { at, entry } => {
                if at < self.entries.len() {
                    self.entries[at] = entry;
                } else if at == self.entries.len() {
                    self.entries.push(entry);
                }
            }
            RemoteEvent::Append {
                at,
                text,
                streaming,
            } => {
                if let Some(Entry::Assistant {
                    text: held,
                    streaming: live,
                }) = self.entries.get_mut(at)
                {
                    held.push_str(&text);
                    *live = streaming;
                }
            }
            RemoteEvent::Truncate { len } => self.entries.truncate(len),
            RemoteEvent::Status { status } => self.status = Some(*status),
            RemoteEvent::Dialog { dialog } => self.set_dialog(dialog),
            RemoteEvent::Unknown => {}
        }
    }

    pub fn view(&self) -> View<'_> {
        let status_line = match &self.status {
            Some(s) => {
                let mut line = format!("{} · {}", s.model, s.mode);
                if let Some(branch) = &s.branch {
                    line.push_str(&format!(" · {branch}"));
                }
                if s.afk {
                    line.push_str(" · afk");
                }
                line
            }
            None => String::new(),
        };
        let answerable = self.can_answer();
        let dialog = self.dialog.as_ref().map(|dialog| match dialog {
            Dialog::Approval {
                tool,
                agent_name,
                summary,
                reason,
                choices,
                preview,
                ..
            } => DialogView::Approval {
                title: if agent_name.is_empty() {
                    format!("Allow {tool}?")
                } else {
                    format!("{agent_name}: allow {tool}?")
                },
                summary,
                reason,
                preview: preview.as_ref(),
                choices: choices
                    .iter()
                    .map(String::as_str)
                    .filter(|c| !c.eq_ignore_ascii_case(ALWAYS))
                    .collect(),
                answerable,
            },
            Dialog::Question {
                index, questions, ..
            } => match questions.get(*index) {
                Some(question) => DialogView::Question {
                    question,
                    answerable: answerable && !question.multi_select,
                    host_only_note: question
                        .multi_select
                        .then_some("answer this multi-select question at the host"),
                },
                None => DialogView::HostWaiting("a question".to_string()),
            },
            Dialog::HostOnly { what } => DialogView::HostWaiting(what.clone()),
            Dialog::Unknown => DialogView::HostWaiting("a prompt".to_string()),
        });
        View {
            screen: &self.screen,
            entries: &self.entries,
            status_line,
            running: self.status.as_ref().is_some_and(|s| s.running),
            dialog,
            reconnecting_in_ms: self.reconnecting,
            notice: self.notice.as_deref(),
            hosts: &self.hosts,
        }
    }
}

/// The failure as one line: the host's reason in the words `fontaine attach` uses, or the error
/// with its causes.
fn describe(error: &AttachError) -> String {
    match error {
        AttachError::Rejected(reason) => reason.describe().to_string(),
        other => {
            let mut text = other.to_string();
            let mut source = std::error::Error::source(other);
            while let Some(cause) = source {
                text.push_str(": ");
                text.push_str(&cause.to_string());
                source = cause.source();
            }
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use fontaine_remote::{EndpointAddr, SecretKey};
    use serde_json::json;

    use super::*;

    fn ticket() -> Ticket {
        let key = SecretKey::from_bytes(&[5; 32]);
        Ticket::new(EndpointAddr::new(key.public()), [3; 32], 1_700_000_000)
    }

    fn event(value: serde_json::Value) -> ClientEvent {
        ClientEvent::Event {
            epoch: 1,
            seq: 1,
            event: value,
        }
    }

    fn snapshot(value: serde_json::Value) -> ClientEvent {
        ClientEvent::Snapshot {
            epoch: 1,
            seq: 1,
            state: value.to_string(),
        }
    }

    fn paired() -> App {
        let mut app = App::new();
        app.submit_link(&ticket().qr_payload());
        assert!(matches!(
            app.confirm_pairing(),
            Some(Action::Start(Target::Ticket(_)))
        ));
        app.on_client_event(ClientEvent::Paired);
        app.on_client_event(ClientEvent::Connected);
        assert_eq!(*app.screen(), Screen::Session);
        app
    }

    fn approval(choices: &[&str]) -> serde_json::Value {
        json!({ "dialog": "approval", "id": 7, "tool": "bash", "agent": "", "agent_name": "",
            "summary": "rm x", "reason": "r", "choices": choices, "preview": null,
            "proposed_rule": null })
    }

    #[test]
    fn a_link_asks_first_and_connects_only_after_the_person_agrees() {
        let mut app = App::new();
        app.submit_link(&ticket().link());
        let node = ticket().node.id.to_string();
        assert_eq!(
            *app.screen(),
            Screen::Confirm {
                host: node.chars().take(10).collect()
            }
        );
        app.decline_pairing();
        assert_eq!(*app.screen(), Screen::Scan);
        assert!(app.confirm_pairing().is_none(), "a declined link is gone");
    }

    #[test]
    fn the_session_starts_on_the_paired_event_and_not_before() {
        let mut app = App::new();
        app.submit_link(&ticket().link());
        app.confirm_pairing();
        assert!(matches!(
            app.screen(),
            Screen::Pairing {
                first_time: true,
                ..
            }
        ));
        app.on_client_event(ClientEvent::Connected);
        assert!(matches!(app.screen(), Screen::Pairing { .. }));
        app.on_client_event(ClientEvent::Paired);
        assert_eq!(*app.screen(), Screen::Session);
    }

    #[test]
    fn a_link_submitted_while_pairing_is_ignored() {
        let mut app = App::new();
        app.submit_link(&ticket().link());
        app.confirm_pairing();
        app.submit_link(&ticket().link());
        assert!(matches!(app.screen(), Screen::Pairing { .. }));
    }

    #[test]
    fn a_set_notice_reaches_the_view() {
        let mut app = App::new();
        app.set_notice("cannot read the paired hosts".to_string());
        assert_eq!(app.view().notice, Some("cannot read the paired hosts"));
    }

    #[test]
    fn text_that_is_no_link_stays_on_scan_with_a_notice() {
        let mut app = App::new();
        app.submit_link("hello");
        assert_eq!(*app.screen(), Screen::Scan);
        assert!(
            app.view()
                .notice
                .expect("a notice")
                .contains("not a Fontaine pairing code")
        );
    }

    #[test]
    fn a_rejected_pairing_shows_the_reason_text() {
        for reason in [Reason::PairingDeclined, Reason::Reused, Reason::Expired] {
            let mut app = App::new();
            app.submit_link(&ticket().link());
            app.confirm_pairing();
            let action = app.on_client_event(ClientEvent::Failed(AttachError::Rejected(reason)));
            assert!(matches!(action, Some(Action::Stop)));
            assert_eq!(*app.screen(), Screen::Error(reason.describe().to_string()));
            assert!(app.back_to_scan().is_none());
            assert_eq!(*app.screen(), Screen::Scan);
        }
    }

    #[test]
    fn revoked_and_not_paired_after_pairing_show_the_reason_text() {
        for reason in [Reason::Revoked, Reason::NotPaired] {
            let mut app = paired();
            app.on_client_event(ClientEvent::Failed(AttachError::Rejected(reason)));
            assert_eq!(*app.screen(), Screen::Error(reason.describe().to_string()));
            assert!(app.view().entries.is_empty());
        }
    }

    #[test]
    fn the_host_closing_ends_the_session_with_its_message() {
        let mut app = paired();
        assert!(matches!(
            app.on_client_event(ClientEvent::Bye),
            Some(Action::Stop)
        ));
        assert_eq!(
            *app.screen(),
            Screen::Error(Reason::Closed.describe().to_string())
        );
    }

    #[test]
    fn a_failure_with_causes_lists_them() {
        let mut app = paired();
        let error = AttachError::Store(std::io::Error::other("disk full"));
        app.on_client_event(ClientEvent::Failed(error));
        let Screen::Error(text) = app.screen() else {
            panic!("an error")
        };
        assert!(text.contains("disk full"), "{text}");
    }

    #[test]
    fn the_transcript_follows_the_snapshot_and_its_events() {
        let mut app = paired();
        app.on_client_event(snapshot(json!({
            "entries": [{ "User": "hi" }, { "Assistant": { "text": "he", "streaming": true } }],
            "status": { "mode": "manual", "model": "m", "running": true, "branch": "main" },
            "dialog": null,
        })));
        app.on_client_event(event(
            json!({ "kind": "append", "at": 1, "text": "llo", "streaming": false }),
        ));
        app.on_client_event(event(
            json!({ "kind": "set", "at": 2, "entry": { "Error": "bad" } }),
        ));
        app.on_client_event(event(
            json!({ "kind": "set", "at": 9, "entry": { "Error": "gap" } }),
        ));
        let view = app.view();
        assert_eq!(view.entries.len(), 3);
        assert!(
            matches!(&view.entries[1], Entry::Assistant { text, streaming: false } if text == "hello")
        );
        assert_eq!(view.status_line, "m · manual · main");
        assert!(view.running);
        app.on_client_event(event(json!({ "kind": "truncate", "len": 1 })));
        assert_eq!(app.view().entries.len(), 1);
    }

    #[test]
    fn an_approval_never_offers_always_allow_and_refuses_it() {
        let mut app = paired();
        app.on_client_event(event(json!({
            "kind": "dialog",
            "dialog": approval(&["allow once", "allow session", "always allow", "deny"]),
        })));
        let Some(DialogView::Approval { choices, .. }) = app.view().dialog else {
            panic!("an approval");
        };
        assert_eq!(choices, ["allow once", "allow session", "deny"]);
        assert!(app.answer_approval("always allow", None).is_none());
        assert!(app.answer_approval("ALWAYS ALLOW", None).is_none());
        assert!(app.answer_approval("allow forever", None).is_none());
        let Some(Action::Send(ClientMsg::ApprovalAnswer { id, answer })) =
            app.answer_approval("deny", Some("no".into()))
        else {
            panic!("an answer");
        };
        assert_eq!(
            (id, answer.choice.as_str(), answer.feedback.as_deref()),
            (7, "deny", Some("no"))
        );
        assert!(
            app.answer_approval("allow once", None).is_none(),
            "answers once"
        );
    }

    #[test]
    fn a_disconnect_mid_approval_sends_no_answer_and_the_snapshot_reopens_it() {
        let mut app = paired();
        app.on_client_event(event(json!({
            "kind": "dialog", "dialog": approval(&["allow once", "deny"]),
        })));
        app.on_client_event(ClientEvent::Reconnecting { after_ms: 500 });
        assert_eq!(app.view().reconnecting_in_ms, Some(500));
        assert!(app.answer_approval("allow once", None).is_none());
        let Some(DialogView::Approval { answerable, .. }) = app.view().dialog else {
            panic!("the dialog stays visible");
        };
        assert!(!answerable);
        app.on_client_event(ClientEvent::Connected);
        app.on_client_event(snapshot(json!({
            "entries": [], "status": null, "dialog": approval(&["allow once", "deny"]),
        })));
        assert!(app.view().reconnecting_in_ms.is_none());
        assert!(matches!(
            app.answer_approval("allow once", None),
            Some(Action::Send(ClientMsg::ApprovalAnswer { .. }))
        ));
    }

    #[test]
    fn a_question_is_answered_with_one_option_and_a_multi_select_is_left_to_the_host() {
        let mut app = paired();
        let question = |multi: bool| {
            json!({ "dialog": "question", "id": 3, "agent": "", "index": 0, "questions": [{
                "question": "which?", "header": "h", "multiSelect": multi,
                "options": [{ "label": "a", "description": "" }, { "label": "b", "description": "" }],
            }]})
        };
        app.on_client_event(event(json!({ "kind": "dialog", "dialog": question(true) })));
        assert!(app.answer_question(0).is_none());
        let Some(DialogView::Question { host_only_note, .. }) = app.view().dialog else {
            panic!("a question");
        };
        assert!(host_only_note.is_some());
        app.on_client_event(event(
            json!({ "kind": "dialog", "dialog": question(false) }),
        ));
        assert!(app.answer_question(5).is_none());
        let Some(Action::Send(ClientMsg::QuestionReply {
            id,
            index,
            options,
            reply,
        })) = app.answer_question(1)
        else {
            panic!("a reply");
        };
        assert_eq!(
            (id, index, options, reply.as_str()),
            (3, 0, vec!["a".to_string(), "b".to_string()], "b")
        );
    }

    #[test]
    fn a_host_only_dialog_says_the_host_is_waiting() {
        let mut app = paired();
        app.on_client_event(event(json!({
            "kind": "dialog", "dialog": { "dialog": "host_only", "what": "a device pairing" },
        })));
        assert!(
            matches!(app.view().dialog, Some(DialogView::HostWaiting(w)) if w == "a device pairing")
        );
        assert!(app.answer_approval("deny", None).is_none());
    }

    #[test]
    fn a_slash_line_is_a_slash_and_other_text_a_prompt() {
        let mut app = paired();
        assert!(matches!(
            app.send_prompt("  hello "),
            Some(Action::Send(ClientMsg::Prompt { text, .. })) if text == "hello"
        ));
        assert!(matches!(
            app.send_prompt("/cost"),
            Some(Action::Send(ClientMsg::Slash { line, .. })) if line == "/cost"
        ));
        assert!(app.send_prompt("   ").is_none());
        assert!(
            app.send_prompt(&"x".repeat(MAX_MESSAGE_BYTES + 1))
                .is_none()
        );
        assert!(app.view().notice.expect("a notice").contains("limit"));
        assert!(matches!(
            app.interrupt(),
            Some(Action::Send(ClientMsg::Interrupt))
        ));
    }

    #[test]
    fn message_ids_differ() {
        let mut app = paired();
        let id = |a: Option<Action>| match a {
            Some(Action::Send(ClientMsg::Prompt { message_id, .. })) => message_id,
            _ => panic!("a prompt"),
        };
        let first = id(app.send_prompt("a"));
        assert_ne!(first, id(app.send_prompt("b")));
    }

    #[test]
    fn a_rejected_prompt_and_host_notices_reach_the_view() {
        let mut app = paired();
        app.on_client_event(ClientEvent::PromptRejected {
            message_id: "x".into(),
            reason: Reason::InboxFull,
        });
        assert_eq!(app.view().notice, Some(Reason::InboxFull.describe()));
        app.on_client_event(ClientEvent::Notice {
            severity: Level::Warning,
            text: "careful".into(),
        });
        assert_eq!(app.view().notice, Some("host: careful"));
    }

    #[test]
    fn a_host_warning_after_an_answer_reopens_the_dialog() {
        let mut app = paired();
        app.on_client_event(event(json!({
            "kind": "dialog", "dialog": approval(&["allow once", "deny"]),
        })));
        assert!(app.answer_approval("allow once", None).is_some());
        assert!(app.answer_approval("allow once", None).is_none());
        app.on_client_event(ClientEvent::Notice {
            severity: Level::Warning,
            text: "busy".into(),
        });
        assert!(app.answer_approval("allow once", None).is_some());
    }

    #[test]
    fn reconnecting_clears_when_the_link_is_back() {
        let mut app = paired();
        app.on_client_event(ClientEvent::Reconnecting { after_ms: 1000 });
        assert_eq!(app.view().reconnecting_in_ms, Some(1000));
        app.on_client_event(ClientEvent::Connected);
        assert!(app.view().reconnecting_in_ms.is_none());
        assert_eq!(*app.screen(), Screen::Session);
    }

    #[test]
    fn a_stored_host_reattaches_without_a_scan_and_starts_on_connect() {
        let mut app = App::new();
        let node = SecretKey::from_bytes(&[8; 32]).public();
        let host = StoredHost {
            node: node.to_string(),
            label: "work".into(),
            addr: EndpointAddr::new(node),
        };
        app.set_hosts(vec![host.clone()]);
        assert!(app.connect_host("nope").is_none());
        assert!(matches!(
            app.connect_host(&host.node),
            Some(Action::Start(Target::Host(_)))
        ));
        assert!(matches!(
            app.screen(),
            Screen::Pairing {
                first_time: false,
                ..
            }
        ));
        app.on_client_event(ClientEvent::Connected);
        assert_eq!(*app.screen(), Screen::Session);
        assert!(matches!(app.back_to_scan(), Some(Action::Stop)));
    }

    #[test]
    fn events_before_the_session_starts_are_ignored() {
        let mut app = App::new();
        app.on_client_event(snapshot(json!({ "entries": [{ "User": "x" }] })));
        assert!(app.view().entries.is_empty());
        assert!(app.send_prompt("hi").is_none());
        assert!(app.interrupt().is_none());
    }
}
