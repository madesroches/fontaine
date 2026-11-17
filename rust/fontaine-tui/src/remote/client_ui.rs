//! `fontaine attach`: the terminal UI of a device. It runs no agent, loads no credentials and no
//! project config. An `App` is built purely from the host's events and painted with the same
//! renderers as the host's; what the person types becomes a client message.
//!
//! Nothing local may enter the mirrored transcript: the host addresses entries by position, so an
//! entry the client added would shift every later `Set`. Local words (a refusal, a reconnect)
//! therefore go to the status bar, and the few paths of `App` that write a notice are undone.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fontaine_remote::{
    AttachError, AttachOptions, Client, ClientEvent, ClientMsg, ClientStore, Level, Target,
    WireAnswer,
};
use fontaine_sdk::core::config::PermissionMode;
use fontaine_sdk::core::permissions::{Answer, ApprovalRequest, Offers};
use fontaine_sdk::core::tier::Tier;
use fontaine_sdk::tools::ask_user::QuestionReply;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::EnableBracketedPaste;
use ratatui::crossterm::terminal::enable_raw_mode;
use tokio::sync::{mpsc, oneshot};

use super::event::{Dialog, RemoteEvent, Snapshot, Status};
use crate::app::{Action, App, Input};
use crate::commands::{Command, parse_command};
use crate::screen::{Anchored, FrameWriter, Screen, Tty};
use crate::{
    LiveScreen, ReaderPause, TerminalGuard, hyperlink, linear, read_terminal, restore_terminal,
};

const HOST_SIDE: &str = "that runs at the host";

fn mode_of(name: &str) -> PermissionMode {
    [
        PermissionMode::Manual,
        PermissionMode::AcceptEdits,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
        PermissionMode::Afk,
    ]
    .into_iter()
    .find(|mode| mode.as_str() == name)
    .unwrap_or(PermissionMode::Manual)
}

fn apply_status(app: &mut App, status: Status) {
    app.mode = mode_of(&status.mode);
    app.model = status.model;
    app.effort = status.effort.and_then(|effort| effort.parse().ok());
    if app.running != status.running {
        app.set_running(status.running);
    }
    app.afk = status.afk;
    app.afk_stopped = status.afk_stopped;
    app.guardian_off = status.guardian_off;
    app.queued = status.queued;
    app.todos.set_items(status.todos);
    app.cwd = PathBuf::from(status.cwd);
    app.branch = status.branch;
    app.running_agents = status.running_agents;
    app.background_shells = status.background_shells;
    app.background_monitors = status.background_monitors;
    app.scheduled = status.scheduled;
    app.goal = status
        .goal
        .map(|(secs, waiting)| (Duration::from_secs(secs), waiting));
}

/// Applies one host event to the device's `App`. A dialog is returned for the caller to install,
/// since it needs the device's own bookkeeping.
pub(crate) fn apply_event(app: &mut App, event: RemoteEvent) -> Option<Option<Dialog>> {
    match event {
        RemoteEvent::Set { at, entry } => app.transcript.mirror_set(at, entry),
        RemoteEvent::Append {
            at,
            text,
            streaming,
        } => app.transcript.mirror_append(at, &text, streaming),
        RemoteEvent::Truncate { len } => {
            app.transcript.mirror_truncate(len);
        }
        RemoteEvent::Status { status } => apply_status(app, *status),
        RemoteEvent::Dialog { dialog } => return Some(dialog),
    }
    None
}

/// Replaces what the device shows with the host's whole state.
pub(crate) fn apply_snapshot(app: &mut App, snapshot: Snapshot) -> Option<Dialog> {
    app.transcript.mirror_truncate(0);
    for (at, entry) in snapshot.entries.into_iter().enumerate() {
        app.transcript.mirror_set(at, entry);
    }
    // What scrollback holds of the old state is stale: the screen is written again from the top.
    app.reprint = true;
    if let Some(status) = snapshot.status {
        apply_status(app, status);
    }
    snapshot.dialog
}

/// The dialog the host showed, as the device has it open.
enum Waiting {
    Approval {
        id: u64,
        answer: oneshot::Receiver<Answer>,
    },
    Question {
        id: u64,
        index: usize,
        options: Vec<String>,
        answer: oneshot::Receiver<QuestionReply>,
    },
}

/// What a person's input at a device came to.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Send(ClientMsg),
    Detach,
    Nothing,
}

pub(crate) struct RemoteClientUi {
    pub(crate) app: App,
    waiting: Option<Waiting>,
    /// The host's dialog as last installed: the host publishes a dialog only when it changes, so
    /// a device that closed its copy without a taken answer could not otherwise reopen it.
    shown: Option<Dialog>,
    /// Set while an answer sent to the host has not yet closed the dialog.
    answer_sent: bool,
    next_id: u64,
    session: String,
}

impl RemoteClientUi {
    pub(crate) fn new(mut app: App) -> RemoteClientUi {
        app.remote_indicator = Some("attached".to_string());
        app.attached_device = true;
        RemoteClientUi {
            app,
            waiting: None,
            shown: None,
            answer_sent: false,
            next_id: 0,
            session: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn message_id(&mut self) -> String {
        self.next_id += 1;
        format!("c-{}-{}", &self.session[..8], self.next_id)
    }

    pub(crate) fn event(&mut self, event: RemoteEvent) {
        if let Some(dialog) = apply_event(&mut self.app, event) {
            self.install(dialog);
        }
    }

    pub(crate) fn snapshot(&mut self, snapshot: Snapshot) {
        let dialog = apply_snapshot(&mut self.app, snapshot);
        self.install(dialog);
    }

    /// Opens the host's dialog on the device, or closes the one that went away.
    fn install(&mut self, dialog: Option<Dialog>) {
        self.answer_sent = false;
        self.shown = dialog.clone();
        self.waiting = None;
        self.app.approval = None;
        self.app.queued_approvals.clear();
        self.app.questions.clear();
        match dialog {
            None => {}
            Some(Dialog::Approval {
                id,
                tool,
                agent,
                agent_name,
                summary,
                reason,
                choices,
                preview,
                proposed_rule,
            }) => {
                let request = ApprovalRequest {
                    tool,
                    agent,
                    agent_name,
                    tool_use_id: None,
                    args: serde_json::Value::Null,
                    summary,
                    tier: Tier::Human,
                    reason,
                    offers: Offers {
                        session: choices.iter().any(|c| c == "allow session"),
                        always: false,
                    },
                    proposed_rule,
                    preview,
                };
                let (reply, answer) = oneshot::channel();
                self.app.begin_approval(request, reply);
                self.waiting = Some(Waiting::Approval { id, answer });
            }
            Some(Dialog::Question {
                id,
                agent,
                index,
                questions,
            }) => {
                let Some(question) = questions.get(index).cloned() else {
                    return;
                };
                let options = question.options.iter().map(|o| o.label.clone()).collect();
                let (reply, answer) = oneshot::channel();
                // Only the sub-question the host is on: each is answered on its own.
                self.app.begin_question(agent, vec![question], reply);
                self.waiting = Some(Waiting::Question {
                    id,
                    index,
                    options,
                    answer,
                });
            }
            Some(Dialog::HostOnly { what }) => {
                self.app.attach_notice = Some(format!("the host is waiting on {what}"));
            }
        }
    }

    /// A host warning after an answer was sent means the host may have refused it: the dialog is
    /// still pending there, so it is opened again here.
    pub(crate) fn host_warned(&mut self) {
        if self.answer_sent {
            self.reopen();
        }
    }

    fn reopen(&mut self) {
        let dialog = self.shown.clone();
        self.install(dialog);
    }

    /// What a dialog answered on the device means for the host, once the person has picked.
    pub(crate) fn answered(&mut self) -> Option<ClientMsg> {
        let had_dialog = self.waiting.is_some();
        let message = self.take_answer();
        if message.is_some() {
            self.answer_sent = true;
        } else if had_dialog && self.waiting.is_none() {
            // A pick that sent nothing (a decline or a typed reply) closed the device's copy.
            self.reopen();
        }
        message
    }

    fn take_answer(&mut self) -> Option<ClientMsg> {
        match self.waiting.as_mut()? {
            Waiting::Approval { id, answer } => {
                let picked = answer.try_recv().ok()?;
                let id = *id;
                self.waiting = None;
                let (choice, feedback) = match picked {
                    Answer::AllowOnce => ("allow once", None),
                    Answer::AllowSession => ("allow session", None),
                    Answer::Deny { feedback } => ("deny", feedback),
                    // The device never offers these; deny rather than guess.
                    Answer::AllowAlways | Answer::AllowUpdated { .. } => ("deny", None),
                };
                Some(ClientMsg::ApprovalAnswer {
                    id,
                    answer: WireAnswer {
                        choice: choice.to_string(),
                        feedback,
                    },
                })
            }
            Waiting::Question {
                id,
                index,
                options,
                answer,
            } => {
                let reply = answer.try_recv().ok()?;
                let (id, index, options) = (*id, *index, std::mem::take(options));
                self.waiting = None;
                match reply {
                    QuestionReply::Answered(answers) => {
                        if answers.first().is_some_and(|a| a.selected.len() > 1) {
                            self.app.attach_notice = Some(
                                "a device can only give one option per question; answer a multi-select question at the host"
                                    .to_string(),
                            );
                            return None;
                        }
                        let Some(label) = answers.first().and_then(|a| a.selected.first()) else {
                            self.app.attach_notice = Some(
                                "a typed answer can only be given at the host; pick an option"
                                    .to_string(),
                            );
                            return None;
                        };
                        Some(ClientMsg::QuestionReply {
                            id,
                            index,
                            options,
                            reply: label.clone(),
                        })
                    }
                    // Declining a question is the host's call.
                    _ => {
                        self.app.attach_notice = Some(format!("{HOST_SIDE}: declining a question"));
                        None
                    }
                }
            }
        }
    }

    /// Feeds one terminal input to the app and says what the device does with it.
    pub(crate) fn input(&mut self, input: Input) -> Outcome {
        let kept = self.app.transcript.entries.len();
        let action = self.app.handle_input(input);
        // Anything the app wrote to the transcript itself would shift the host's positions.
        self.app.transcript.mirror_cut(kept);
        self.action(action)
    }

    fn local(&mut self, text: &str) -> Outcome {
        self.app.attach_notice = Some(text.to_string());
        Outcome::Nothing
    }

    fn line(&mut self, text: String) -> Outcome {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Outcome::Nothing;
        }
        let message_id = self.message_id();
        match parse_command(&text) {
            Some(Command::Other { name, .. }) if name == "detach" => Outcome::Detach,
            Some(Command::Exit) => Outcome::Detach,
            Some(_) => Outcome::Send(ClientMsg::Slash {
                message_id,
                line: text,
            }),
            None => Outcome::Send(ClientMsg::Prompt { message_id, text }),
        }
    }

    fn action(&mut self, action: Action) -> Outcome {
        match action {
            Action::None => Outcome::Nothing,
            Action::Submit(text, attachments) => {
                if !attachments.is_empty() {
                    return self.local("attachments are host-side; they were not sent");
                }
                self.line(text)
            }
            Action::Queue(queued) => {
                use fontaine_sdk::core::steering::Queued;
                match queued {
                    Queued::Message(text) => self.line(text),
                    Queued::Shell(_) => self.local("shell lines run at the host terminal"),
                    Queued::Command(raw) | Queued::Skill { raw, .. } => self.line(raw),
                }
            }
            Action::Command { command, raw } => match command {
                Command::Exit => Outcome::Detach,
                Command::Todos => {
                    self.app.toggle_todos();
                    Outcome::Nothing
                }
                Command::Transcript => {
                    self.app.toggle_transcript();
                    Outcome::Nothing
                }
                _ => self.line(raw),
            },
            Action::Interrupt => Outcome::Send(ClientMsg::Interrupt),
            Action::Quit => Outcome::Detach,
            Action::CycleMode => self.local("the mode is changed at the host"),
            Action::Shell(_) => self.local("shell lines run at the host terminal"),
            Action::AttachClipboard => self.local("attachments are host-side"),
            Action::EditQueued => self.local("queued messages are edited at the host"),
            _ => self.local(HOST_SIDE),
        }
    }
}

/// How an attach ended.
#[derive(Debug)]
pub enum AttachEnd {
    /// The person left with Ctrl+D or `/detach`; the host session continues.
    Detached,
    /// The host said goodbye: remote control was turned off, or the session ended.
    HostClosed,
    Failed(AttachError),
}

pub struct AttachParams {
    pub target: Target,
    pub options: AttachOptions,
    pub store: Arc<dyn ClientStore>,
    /// Draw for a screen reader instead of the live area.
    pub linear: bool,
}

/// Takes over the terminal and drives a host from it until the person leaves or the attach ends.
pub async fn run_attached(params: AttachParams) -> Result<AttachEnd, String> {
    enable_raw_mode().map_err(|e| {
        format!(
            "could not start the terminal UI: {}",
            fontaine_sdk::tools::error_chain::chain(&e)
        )
    })?;
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    std::panic::set_hook(crate::terminal_panic_hook(std::panic::take_hook()));
    let screen: Option<Arc<Mutex<LiveScreen>>> = if params.linear {
        None
    } else {
        match Screen::new(
            Anchored::new(CrosstermBackend::new(FrameWriter::stdout())),
            || Anchored::new(CrosstermBackend::new(FrameWriter::discarding(Tty::std()))),
        ) {
            Ok(screen) => Some(Arc::new(Mutex::new(screen))),
            Err(e) => {
                restore_terminal();
                return Err(format!(
                    "could not start the terminal UI: {}",
                    fontaine_sdk::tools::error_chain::chain(&e)
                ));
            }
        }
    };
    let _guard = TerminalGuard {
        screen: screen.clone(),
    };
    let (tx, mut input) = mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(ReaderPause::default());
    let reader = {
        let (stop, pause) = (stop.clone(), pause.clone());
        tokio::task::spawn_blocking(move || read_terminal(&tx, &stop, &pause))
    };
    let hyperlinks = hyperlink::supported(|k| std::env::var(k).ok(), cfg!(windows));
    let mut linear = linear::Linear::new(std::io::stdout());
    let on_error = stop.clone();
    let mut render = move |app: &mut App| {
        let rendered = match &screen {
            Some(screen) => {
                app.links.enabled = hyperlinks;
                match screen.lock() {
                    Ok(mut screen) => if app.flush {
                        screen.commit_all(app)
                    } else {
                        screen.render(app)
                    }
                    .is_ok(),
                    Err(_) => false,
                }
            }
            None => {
                app.linear = true;
                linear.render(app).is_ok()
            }
        };
        if !rendered {
            on_error.store(true, Ordering::Relaxed);
        }
    };

    let mut client = Client::start(params.target, params.options, params.store);
    let mut ui = RemoteClientUi::new(App::new(PermissionMode::Manual, String::new()));
    ui.app.transcript.notice("connecting…");
    let epoch = Instant::now();
    let end = loop {
        ui.app.spinner_frame =
            (epoch.elapsed().as_millis() / u128::from(crate::spinner::FRAME_MS)) as u64;
        ui.app.turn_elapsed = ui.app.turn_started.map(|started| started.elapsed());
        // The device loads no config, so its bar is always the default program's.
        crate::statusbar::render_default(&mut ui.app);
        render(&mut ui.app);
        if stop.load(Ordering::Relaxed) {
            break AttachEnd::Detached;
        }
        let tick = if ui.app.running {
            Duration::from_millis(crate::spinner::FRAME_MS)
        } else {
            Duration::from_secs(1)
        };
        tokio::select! {
            input = input.recv() => {
                let Some(input) = input else {
                    break AttachEnd::Detached;
                };
                match ui.input(input) {
                    Outcome::Send(message) => client.send(message),
                    Outcome::Detach => break AttachEnd::Detached,
                    Outcome::Nothing => {}
                }
                if let Some(message) = ui.answered() {
                    client.send(message);
                }
            }
            event = client.events.recv() => {
                let Some(event) = event else {
                    break AttachEnd::HostClosed;
                };
                match event {
                    ClientEvent::Paired => {
                        ui.app.attach_notice = Some("paired with the host".to_string());
                    }
                    ClientEvent::Connected => {
                        ui.app.remote_indicator = Some("attached".to_string());
                    }
                    ClientEvent::Reconnecting { .. } => {
                        ui.app.remote_indicator = Some("reconnecting…".to_string());
                    }
                    ClientEvent::Snapshot { state, .. } => {
                        match serde_json::from_str::<Snapshot>(&state) {
                            Ok(snapshot) => ui.snapshot(snapshot),
                            Err(e) => break AttachEnd::Failed(AttachError::protocol_from("undecodable snapshot", e)),
                        }
                    }
                    ClientEvent::Event { event, .. } => {
                        match serde_json::from_value::<RemoteEvent>(event) {
                            Ok(event) => ui.event(event),
                            Err(e) => break AttachEnd::Failed(AttachError::protocol_from("undecodable event", e)),
                        }
                    }
                    ClientEvent::Notice { severity, text } => {
                        if severity == Level::Warning {
                            ui.host_warned();
                        }
                        ui.app.attach_notice = Some(match severity {
                            Level::Info => text,
                            Level::Warning => format!("warning: {text}"),
                        });
                    }
                    ClientEvent::Accepted { .. } => {}
                    ClientEvent::PromptRejected { reason, .. } => {
                        ui.app.attach_notice = Some(reason.describe().to_string());
                    }
                    ClientEvent::Bye => break AttachEnd::HostClosed,
                    ClientEvent::Failed(error) => break AttachEnd::Failed(error),
                }
            }
            () = tokio::time::sleep(tick) => {}
        }
    };
    stop.store(true, Ordering::Relaxed);
    client.stop();
    // What the screen held back is written out so the conversation stays in the terminal.
    ui.app.flush = true;
    crate::statusbar::render_default(&mut ui.app);
    render(&mut ui.app);
    let _ = tokio::time::timeout(Duration::from_millis(500), reader).await;
    Ok(end)
}

#[cfg(test)]
mod tests {
    use fontaine_sdk::core::turn::SessionEvent;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use super::*;
    use crate::remote::MirrorTracker;
    use crate::theme::Theme;
    use crate::transcript::Entry;

    fn app() -> App {
        App::new(PermissionMode::Manual, "m".into())
    }

    fn key(code: KeyCode) -> Input {
        Input::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_line(ui: &mut RemoteClientUi, text: &str) -> Outcome {
        for c in text.chars() {
            ui.input(key(KeyCode::Char(c)));
        }
        ui.input(key(KeyCode::Enter))
    }

    fn approval_dialog(id: u64, choices: &[&str]) -> Dialog {
        Dialog::Approval {
            id,
            tool: "write".into(),
            agent: String::new(),
            agent_name: String::new(),
            summary: "notes.txt".into(),
            reason: "it writes".into(),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            preview: None,
            proposed_rule: Some("write(notes.txt)".into()),
        }
    }

    fn screen_text(app: &mut App) -> Vec<String> {
        let mut screen =
            Screen::new(TestBackend::new(60, 24), || TestBackend::new(60, 24)).unwrap();
        screen.render(app).unwrap();
        let rows = |buffer: &ratatui::buffer::Buffer| -> Vec<String> {
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .filter(|row| !row.is_empty())
                .collect()
        };
        let mut all = rows(screen.backend().scrollback());
        all.extend(rows(screen.backend().buffer()));
        all
    }

    #[test]
    fn the_devices_bar_shows_the_mode_and_the_attached_indicator() {
        let mut device = app();
        device.remote_indicator = Some("attached".to_string());
        crate::statusbar::render_default(&mut device);
        let text = screen_text(&mut device).join("\n");
        assert!(text.contains("manual · m"), "{text}");
        assert!(text.contains("attached"), "{text}");
    }

    #[test]
    fn a_device_renders_the_same_screen_as_the_host_from_events_alone() {
        let mut host = app();
        host.model = "m".into();
        let mut tracker = MirrorTracker::new(false);
        let mut device = RemoteClientUi::new(app());
        host.transcript.user("make a note".into());
        host.on_event(SessionEvent::AssistantDelta("Writing it.".into()));
        host.on_event(SessionEvent::ToolCall {
            id: "c".into(),
            tool: "write".into(),
            args: json!({ "path": "notes.txt", "content": "hello" }),
        });
        host.on_event(SessionEvent::ToolResult {
            id: "c".into(),
            tool: "write".into(),
            success: true,
            text: "wrote 5 bytes".into(),
        });
        host.on_event(SessionEvent::Assistant("Done.".into()));
        for event in tracker.diff(&host) {
            let wire = serde_json::to_value(&event).unwrap();
            device.event(serde_json::from_value(wire).unwrap());
        }
        device.app.remote_indicator = None;
        let theme = Theme::preset(Default::default());
        assert_eq!(
            host.transcript.lines(60, &theme),
            device.app.transcript.lines(60, &theme)
        );
        // Both screens show the same transcript rows; the device's status line is its own.
        let on_host = screen_text(&mut host);
        let on_device = screen_text(&mut device.app);
        let body = |rows: &[String]| {
            rows.iter()
                .filter(|row| {
                    ["make a note", "Writing it.", "write", "Done."]
                        .iter()
                        .any(|needle| row.contains(needle))
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(body(&on_host), body(&on_device));
        assert!(!body(&on_host).is_empty());
    }

    #[test]
    fn what_a_person_types_becomes_a_prompt_or_a_slash_message() {
        let mut ui = RemoteClientUi::new(app());
        assert!(matches!(
            type_line(&mut ui, "hello there"),
            Outcome::Send(ClientMsg::Prompt { text, .. }) if text == "hello there"
        ));
        assert!(matches!(
            type_line(&mut ui, "/compact"),
            Outcome::Send(ClientMsg::Slash { line, .. }) if line == "/compact"
        ));
        assert_eq!(type_line(&mut ui, "/detach"), Outcome::Detach);
        // A shell line is the host's: it is neither sent nor run here.
        assert_eq!(type_line(&mut ui, "!ls"), Outcome::Nothing);
        assert!(
            ui.app
                .attach_notice
                .as_deref()
                .is_some_and(|n| n.contains("host"))
        );
    }

    #[test]
    fn nothing_the_app_writes_locally_lands_in_the_mirrored_transcript() {
        let mut ui = RemoteClientUi::new(app());
        ui.event(RemoteEvent::Set {
            at: 0,
            entry: Entry::User("hi".into()),
        });
        ui.install(Some(approval_dialog(
            3,
            &["allow once", "allow session", "deny"],
        )));
        // Past the answer grace, as a real person's key would be.
        std::thread::sleep(crate::confirm_prompt::ANSWER_GRACE + Duration::from_millis(50));
        ui.input(key(KeyCode::Char('y')));
        assert_eq!(ui.app.transcript.entries.len(), 1);
        let message = ui.answered().expect("the answer goes to the host");
        assert_eq!(
            message,
            ClientMsg::ApprovalAnswer {
                id: 3,
                answer: WireAnswer {
                    choice: "allow once".into(),
                    feedback: None
                }
            }
        );
    }

    #[test]
    fn an_approval_offers_no_always_and_a_denial_carries_its_reason() {
        let mut ui = RemoteClientUi::new(app());
        ui.install(Some(approval_dialog(9, &["allow once", "deny"])));
        let footer = ui
            .app
            .approval
            .as_ref()
            .expect("an approval")
            .footer(60)
            .iter()
            .map(|l| l.to_string())
            .collect::<String>();
        assert!(!footer.contains("always allow") && !footer.contains("allow session"));
        std::thread::sleep(crate::confirm_prompt::ANSWER_GRACE + Duration::from_millis(50));
        ui.input(key(KeyCode::Down));
        ui.input(key(KeyCode::Enter));
        for c in "too risky".chars() {
            ui.input(key(KeyCode::Char(c)));
        }
        ui.input(key(KeyCode::Enter));
        let Some(ClientMsg::ApprovalAnswer { id, answer }) = ui.answered() else {
            panic!("an approval answer");
        };
        assert_eq!(id, 9);
        assert_eq!(answer.choice, "deny");
        assert_eq!(answer.feedback.as_deref(), Some("too risky"));
    }

    #[test]
    fn a_question_is_answered_one_sub_question_at_a_time_with_its_options() {
        use fontaine_sdk::tools::ask_user::{Question, QuestionOption};
        let option = |label: &str| QuestionOption {
            label: label.into(),
            description: "d".into(),
        };
        let question = |text: &str| Question {
            question: text.into(),
            header: "h".into(),
            options: vec![option("a"), option("b")],
            multi_select: false,
        };
        let mut ui = RemoteClientUi::new(app());
        ui.install(Some(Dialog::Question {
            id: 5,
            agent: "main".into(),
            index: 1,
            questions: vec![question("one?"), question("two?")],
        }));
        std::thread::sleep(crate::confirm_prompt::ANSWER_GRACE + Duration::from_millis(50));
        ui.input(key(KeyCode::Char('2')));
        ui.input(key(KeyCode::Enter));
        assert_eq!(
            ui.answered(),
            Some(ClientMsg::QuestionReply {
                id: 5,
                index: 1,
                options: vec!["a".into(), "b".into()],
                reply: "b".into(),
            })
        );
    }

    #[test]
    fn a_dialog_the_host_still_has_open_can_be_answered_again() {
        use fontaine_sdk::tools::ask_user::{Question, QuestionOption};
        let grace = crate::confirm_prompt::ANSWER_GRACE + Duration::from_millis(50);
        let mut ui = RemoteClientUi::new(app());
        ui.install(Some(approval_dialog(4, &["allow once", "deny"])));
        std::thread::sleep(grace);
        ui.input(key(KeyCode::Char('y')));
        assert!(ui.answered().is_some());
        assert!(ui.app.approval.is_none());
        // The host refused the answer and warned; its dialog is still pending.
        ui.host_warned();
        assert!(ui.app.approval.is_some());
        std::thread::sleep(grace);
        ui.input(key(KeyCode::Char('y')));
        assert!(matches!(
            ui.answered(),
            Some(ClientMsg::ApprovalAnswer { id: 4, .. })
        ));

        // A typed reply sends nothing, so the question opens again.
        let option = |label: &str| QuestionOption {
            label: label.into(),
            description: "d".into(),
        };
        ui.install(Some(Dialog::Question {
            id: 5,
            agent: String::new(),
            index: 0,
            questions: vec![Question {
                question: "q?".into(),
                header: "h".into(),
                options: vec![option("a"), option("b")],
                multi_select: false,
            }],
        }));
        ui.waiting = match ui.waiting.take() {
            Some(Waiting::Question {
                id, index, options, ..
            }) => {
                let (reply, answer) = oneshot::channel();
                reply
                    .send(QuestionReply::Answered(Vec::new()))
                    .expect("receiver is alive");
                Some(Waiting::Question {
                    id,
                    index,
                    options,
                    answer,
                })
            }
            other => other,
        };
        assert!(ui.answered().is_none());
        assert!(matches!(ui.waiting, Some(Waiting::Question { id: 5, .. })));
        assert!(!ui.app.questions.is_empty());
    }

    #[test]
    fn a_multi_select_answer_with_several_options_is_refused_with_a_notice() {
        use fontaine_sdk::tools::ask_user::{Question, QuestionAnswer, QuestionOption};
        let option = |label: &str| QuestionOption {
            label: label.into(),
            description: "d".into(),
        };
        let mut ui = RemoteClientUi::new(app());
        ui.install(Some(Dialog::Question {
            id: 6,
            agent: String::new(),
            index: 0,
            questions: vec![Question {
                question: "q?".into(),
                header: "h".into(),
                options: vec![option("a"), option("b")],
                multi_select: true,
            }],
        }));
        ui.waiting = match ui.waiting.take() {
            Some(Waiting::Question {
                id, index, options, ..
            }) => {
                let (reply, answer) = oneshot::channel();
                reply
                    .send(QuestionReply::Answered(vec![QuestionAnswer {
                        question: "q?".into(),
                        selected: vec!["a".into(), "b".into()],
                        other: None,
                    }]))
                    .expect("receiver is alive");
                Some(Waiting::Question {
                    id,
                    index,
                    options,
                    answer,
                })
            }
            other => other,
        };
        assert!(ui.answered().is_none());
        assert!(
            ui.app
                .attach_notice
                .as_deref()
                .is_some_and(|n| n.contains("multi-select"))
        );
    }

    #[test]
    fn a_host_only_dialog_is_a_status_line_and_nothing_to_answer() {
        let mut ui = RemoteClientUi::new(app());
        ui.install(Some(Dialog::HostOnly {
            what: "a device pairing".into(),
        }));
        assert!(ui.app.approval.is_none() && ui.app.questions.is_empty());
        assert!(
            ui.app
                .attach_notice
                .as_deref()
                .is_some_and(|n| n.contains("waiting"))
        );
        assert!(ui.answered().is_none());
    }

    #[test]
    fn interrupt_and_local_only_keys_are_sorted_out() {
        let mut ui = RemoteClientUi::new(app());
        ui.event(RemoteEvent::Status {
            status: Box::new(Status {
                mode: "auto".into(),
                model: "x".into(),
                effort: Some("high".into()),
                running: true,
                afk: false,
                afk_stopped: false,
                guardian_off: false,
                queued: Vec::new(),
                todos: Vec::new(),
                cwd: "/w".into(),
                branch: None,
                running_agents: 0,
                background_shells: 0,
                background_monitors: 0,
                scheduled: 0,
                goal: None,
            }),
        });
        assert!(ui.app.running && ui.app.mode == PermissionMode::Auto);
        assert_eq!(
            ui.input(key(KeyCode::Esc)),
            Outcome::Send(ClientMsg::Interrupt)
        );
        assert_eq!(
            ui.input(Input::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL
            ))),
            Outcome::Detach
        );
    }

    #[test]
    fn a_call_written_to_a_small_device_scrollback_still_shows_its_outcome() {
        let theme = Theme::preset(Default::default());
        let mut host = app();
        host.on_event(SessionEvent::ToolCall {
            id: "c".into(),
            tool: "write".into(),
            args: json!({ "path": "notes.txt", "content": "hello" }),
        });
        let mut device = RemoteClientUi::new(app());
        device.event(RemoteEvent::Set {
            at: 0,
            entry: host.transcript.entries[0].clone(),
        });
        // A live area over budget writes the pending call into the device's scrollback.
        assert!(
            !device
                .app
                .transcript
                .commit_within(60, &theme, 1)
                .is_empty()
        );
        host.on_event(SessionEvent::ToolResult {
            id: "c".into(),
            tool: "write".into(),
            success: false,
            text: "disk full".into(),
        });
        device.event(RemoteEvent::Set {
            at: 0,
            entry: host.transcript.entries[0].clone(),
        });
        device.event(RemoteEvent::Set {
            at: 1,
            entry: Entry::User("next".into()),
        });
        let live = device
            .app
            .transcript
            .live_lines(60, &theme)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(live.contains("disk full"), "{live}");
        assert!(live.contains("next"), "{live}");
        assert_eq!(device.app.transcript.mirror_entries().len(), 2);
    }

    #[test]
    fn a_snapshot_replaces_the_state_and_asks_for_a_full_redraw() {
        let mut ui = RemoteClientUi::new(app());
        ui.event(RemoteEvent::Set {
            at: 0,
            entry: Entry::User("stale".into()),
        });
        ui.app.reprint = false;
        ui.snapshot(Snapshot {
            entries: vec![Entry::User("fresh".into())],
            status: None,
            dialog: None,
        });
        assert_eq!(ui.app.transcript.entries, vec![Entry::User("fresh".into())]);
        assert!(ui.app.reprint);
    }
}
