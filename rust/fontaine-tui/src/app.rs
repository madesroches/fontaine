//! The interface state and what each input does to it. Nothing here touches the terminal or the
//! session: key handling returns an [`Action`] for the driver to carry out, so tests can feed
//! keys directly.

use fontaine_core::config::PermissionMode;
use fontaine_core::permissions::{Answer, ApprovalRequest, UnavailableChoice};
use fontaine_core::turn::SessionEvent;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use crate::approval::Approval;
use crate::denials::{DeniedKey, DeniedList, UnavailableAnswer, UnavailablePrompt};
use crate::input::InputBuffer;
use crate::transcript::Transcript;

/// One thing the terminal told us.
#[derive(Clone, Debug)]
pub enum Input {
    Key(KeyEvent),
    Paste(String),
    Resize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Submit(String),
    CycleMode,
    Interrupt,
    Quit,
    /// The human approved denied call `n` (as `/denied` lists it) for one retry.
    RetryDenied(usize),
    /// The human answered the unavailable-guardian prompt.
    Guardian(UnavailableAnswer),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Exit,
    AddDir(String),
    Denied,
    Model(String),
    GuardianModel(String),
    Unknown(String),
}

/// Reads a slash command from a submitted message; `None` when it is an ordinary message.
pub fn parse_command(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (name, argument) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some(match name {
        "exit" | "quit" => Command::Exit,
        "add-dir" => Command::AddDir(argument.trim().to_string()),
        "denied" => Command::Denied,
        "model" => Command::Model(argument.trim().to_string()),
        "guardian-model" => Command::GuardianModel(argument.trim().to_string()),
        _ => Command::Unknown(name.to_string()),
    })
}

pub struct App {
    pub transcript: Transcript,
    pub input: InputBuffer,
    pub mode: PermissionMode,
    pub model: String,
    pub running: bool,
    pub approval: Option<Approval>,
    /// `/denied` while it is open.
    pub denied: Option<DeniedList>,
    /// The pinned guardian cannot be used, and the human has to say what to do.
    pub unavailable: Option<UnavailablePrompt>,
    /// Harness notes the next message to the agent carries, such as a retry the human approved.
    /// They are not the user's words and never enter the ledger as such.
    pub pending_notices: Vec<String>,
    /// Lines the transcript is scrolled up from its end.
    pub scroll: usize,
}

const SCROLL_STEP: usize = 10;
const SCROLL_CAP: usize = 100_000;

fn is_cycle_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

impl App {
    pub fn new(mode: PermissionMode, model: String) -> App {
        App {
            transcript: Transcript::default(),
            input: InputBuffer::default(),
            mode,
            model,
            running: false,
            approval: None,
            denied: None,
            unavailable: None,
            pending_notices: Vec::new(),
            scroll: 0,
        }
    }

    pub fn on_event(&mut self, event: SessionEvent) {
        self.transcript.on_event(event);
    }

    pub fn begin_approval(&mut self, request: ApprovalRequest, reply: oneshot::Sender<Answer>) {
        self.approval = Some(Approval::new(request, reply));
    }

    pub fn begin_unavailable(&mut self, prompt: UnavailablePrompt) {
        self.unavailable = Some(prompt);
    }

    /// Answers the open prompts with a denial, as when their turn is interrupted.
    pub fn deny_pending(&mut self) {
        if let Some(mut approval) = self.approval.take() {
            approval.reply(Answer::Deny { feedback: None });
        }
        if let Some(mut prompt) = self.unavailable.take() {
            prompt.reply(UnavailableChoice::Deny);
        }
    }

    /// Tells the waiting pipeline what the human chose about the unavailable guardian.
    pub fn finish_unavailable(&mut self, choice: UnavailableChoice) {
        if let Some(mut prompt) = self.unavailable.take() {
            prompt.reply(choice);
        }
    }

    pub fn handle_input(&mut self, input: Input) -> Action {
        match input {
            Input::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Input::Paste(text) => {
                if self.approval.is_none() && self.denied.is_none() && self.unavailable.is_none() {
                    self.input.insert_str(&text);
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if is_cycle_key(&key) {
            return Action::CycleMode;
        }
        if control && key.code == KeyCode::Char('c') {
            return if self.running {
                Action::Interrupt
            } else {
                Action::Quit
            };
        }
        let typing_reason = self
            .approval
            .as_ref()
            .is_some_and(Approval::is_typing_feedback);
        if let Some(list) = &mut self.denied {
            return match list.handle_key(key) {
                DeniedKey::None => Action::None,
                DeniedKey::Close => {
                    self.denied = None;
                    Action::None
                }
                DeniedKey::Retry(index) => Action::RetryDenied(index),
            };
        }
        if let Some(prompt) = &mut self.unavailable {
            return match prompt.handle_key(key) {
                Some(answer) => Action::Guardian(answer),
                None => Action::None,
            };
        }
        if key.code == KeyCode::Esc && self.running && !typing_reason {
            return Action::Interrupt;
        }
        if self.approval.is_some() {
            self.approval_key(key);
            return Action::None;
        }
        if control && key.code == KeyCode::Char('d') {
            return if !self.running && self.input.is_empty() {
                Action::Quit
            } else {
                Action::None
            };
        }
        self.message_key(key)
    }

    fn approval_key(&mut self, key: KeyEvent) {
        let Some(approval) = &mut self.approval else {
            return;
        };
        let Some(answer) = approval.handle_key(key) else {
            return;
        };
        let note = match &answer {
            Answer::AllowOnce => "allowed once".to_string(),
            Answer::AllowSession => "allowed for this session".to_string(),
            Answer::AllowAlways => "always allowed".to_string(),
            Answer::Deny { feedback: None } => "denied".to_string(),
            Answer::Deny {
                feedback: Some(why),
            } => format!("denied: {why}"),
        };
        let tool = approval.request.tool.clone();
        approval.reply(answer);
        self.approval = None;
        self.transcript.notice(format!("{tool} {note}"));
    }

    fn message_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::PageUp => self.scroll = (self.scroll + SCROLL_STEP).min(SCROLL_CAP),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(SCROLL_STEP),
            KeyCode::Enter => {
                let newline = key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
                return self.enter(newline);
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.insert('\n');
            }
            _ => {
                self.input.handle_key(key);
            }
        }
        Action::None
    }

    fn enter(&mut self, newline: bool) -> Action {
        let text = self.input.text();
        // A trailing backslash continues the message on a new line, for terminals that cannot
        // tell Shift+Enter from Enter.
        if newline || text.ends_with('\\') {
            if !newline {
                self.input.backspace();
            }
            self.input.insert('\n');
            return Action::None;
        }
        if text.trim().is_empty() {
            return Action::None;
        }
        if self.running {
            self.transcript
                .notice("a turn is running; wait for it or press Esc to interrupt");
            return Action::None;
        }
        self.scroll = 0;
        let text = self.input.take();
        Action::Submit(text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Input {
        Input::Key(KeyEvent::new(code, modifiers))
    }

    fn app() -> App {
        App::new(PermissionMode::Manual, "m".into())
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("/exit"), Some(Command::Exit));
        assert_eq!(
            parse_command("/add-dir  /tmp/x "),
            Some(Command::AddDir("/tmp/x".into()))
        );
        assert_eq!(
            parse_command("/nope"),
            Some(Command::Unknown("nope".into()))
        );
        assert_eq!(parse_command("/denied"), Some(Command::Denied));
        assert_eq!(
            parse_command("/model  work/claude-sonnet-4-6 "),
            Some(Command::Model("work/claude-sonnet-4-6".into()))
        );
        assert_eq!(parse_command("/model"), Some(Command::Model(String::new())));
        assert_eq!(
            parse_command("/guardian-model a/b"),
            Some(Command::GuardianModel("a/b".into()))
        );
    }

    #[test]
    fn enter_submits_and_alt_enter_adds_a_line() {
        let mut app = app();
        for c in "hi".chars() {
            app.handle_input(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_input(key(KeyCode::Enter, KeyModifiers::ALT));
        app.handle_input(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(
            app.handle_input(key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Submit("hi\nx".into())
        );
    }

    #[test]
    fn ctrl_c_interrupts_a_turn_and_quits_when_idle() {
        let mut app = app();
        let ctrl_c = || key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_input(ctrl_c()), Action::Quit);
        app.running = true;
        assert_eq!(app.handle_input(ctrl_c()), Action::Interrupt);
        assert_eq!(
            app.handle_input(key(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Interrupt
        );
    }

    #[test]
    fn a_message_is_not_submitted_during_a_turn() {
        let mut app = app();
        app.running = true;
        app.handle_input(key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(
            app.handle_input(key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
        assert!(!app.input.is_empty());
    }
}
