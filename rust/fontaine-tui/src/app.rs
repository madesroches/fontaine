//! The interface state and what each input does to it. Nothing here touches the terminal or the
//! session: key handling returns an [`Action`] for the driver to carry out, so tests can feed
//! keys directly.

use std::collections::VecDeque;

use fontaine_core::config::PermissionMode;
use fontaine_core::permissions::{Answer, ApprovalRequest, UnavailableChoice};
use fontaine_core::turn::SessionEvent;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use crate::agents_panel::{AgentsPanel, PanelKey};
use crate::approval::Approval;
use crate::denials::{DeniedKey, DeniedList, UnavailableAnswer, UnavailablePrompt};
use crate::input::InputBuffer;
use crate::server_prompt::ServerPrompt;
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
    /// The human asked to stop a subagent.
    KillAgent(String),
    /// The human answered the question about starting a repository's MCP server.
    Server(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Exit,
    AddDir(String),
    Denied,
    Agents,
    OutputStyle(String),
    Model(String),
    GuardianModel(String),
    /// A name no built-in command has; it may be a skill.
    Other {
        name: String,
        argument: String,
    },
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
        "agents" => Command::Agents,
        "output-style" => Command::OutputStyle(argument.trim().to_string()),
        "model" => Command::Model(argument.trim().to_string()),
        "guardian-model" => Command::GuardianModel(argument.trim().to_string()),
        _ => Command::Other {
            name: name.to_string(),
            argument: argument.trim().to_string(),
        },
    })
}

/// One line of the `/` menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub name: String,
    pub hint: Option<String>,
    pub description: String,
}

/// The built-in commands, as the `/` menu lists them.
pub fn builtin_menu() -> Vec<MenuEntry> {
    let entry = |name: &str, hint: Option<&str>, description: &str| MenuEntry {
        name: name.to_string(),
        hint: hint.map(str::to_string),
        description: description.to_string(),
    };
    vec![
        entry("add-dir", Some("<path>"), "add a workspace root"),
        entry("agents", None, "the background agents panel"),
        entry("denied", None, "calls the guardian denied"),
        entry("exit", None, "leave Fontaine"),
        entry(
            "guardian-model",
            Some("[account/model]"),
            "show or change the guardian",
        ),
        entry("model", Some("[account/model]"), "show or change the model"),
        entry(
            "output-style",
            Some("[name|none]"),
            "show or change the output style",
        ),
    ]
}

pub struct App {
    pub transcript: Transcript,
    pub input: InputBuffer,
    pub mode: PermissionMode,
    pub model: String,
    pub running: bool,
    pub approval: Option<Approval>,
    /// Approvals waiting behind the open one: several agents can ask at once.
    pub queued: VecDeque<Approval>,
    /// The `/agents` panel while it is open.
    pub agents: Option<AgentsPanel>,
    /// A repository's MCP server waiting for the human's say-so.
    pub server: Option<ServerPrompt>,
    /// The commands and user-invocable skills the `/` menu lists.
    pub menu: Vec<MenuEntry>,
    /// Subagents running right now, for the status bar.
    pub running_agents: usize,
    /// `/denied` while it is open.
    pub denied: Option<DeniedList>,
    /// The pinned guardian cannot be used, and the human has to say what to do.
    pub unavailable: Option<UnavailablePrompt>,
    /// Harness notes the next message to the agent carries, such as a retry the human approved.
    /// They are not the user's words and never enter the ledger as such.
    pub pending_notices: Vec<String>,
}

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
            queued: VecDeque::new(),
            agents: None,
            server: None,
            menu: builtin_menu(),
            running_agents: 0,
            denied: None,
            unavailable: None,
            pending_notices: Vec::new(),
        }
    }

    pub fn on_event(&mut self, event: SessionEvent) {
        self.transcript.on_event(event);
    }

    /// Shows the approval, or queues it behind the one the human is answering.
    pub fn begin_approval(&mut self, request: ApprovalRequest, reply: oneshot::Sender<Answer>) {
        let approval = Approval::new(request, reply);
        if self.approval.is_none() {
            self.approval = Some(approval);
        } else {
            self.queued.push_back(approval);
        }
    }

    /// Drops the approvals whose call is gone, such as those of an agent that was killed, and
    /// opens the next one when the open one went away.
    pub fn prune_closed(&mut self) {
        self.queued.retain(|approval| !approval.is_closed());
        if self.approval.as_ref().is_some_and(Approval::is_closed) {
            self.approval = None;
        }
        if self.approval.is_none() {
            self.approval = self.queued.pop_front();
        }
    }

    /// The calls `agent` is waiting for the human to decide, as the call text.
    pub fn pending_for(&self, agent: &str) -> Vec<String> {
        self.approval
            .iter()
            .chain(self.queued.iter())
            .filter(|approval| approval.request.agent == agent)
            .map(|approval| format!("{} {}", approval.request.tool, approval.request.summary))
            .collect()
    }

    pub fn begin_server(&mut self, prompt: ServerPrompt) {
        self.server = Some(prompt);
    }

    pub fn begin_unavailable(&mut self, prompt: UnavailablePrompt) {
        self.unavailable = Some(prompt);
    }

    /// Answers the open prompts of the main conversation with a denial, as when its turn is
    /// interrupted. A background agent's prompts stay: its run goes on.
    pub fn deny_pending(&mut self) {
        let root = |approval: &Approval| {
            approval.request.agent.is_empty()
                || approval.request.agent == fontaine_tools::gate::ROOT_AGENT
        };
        let mut kept: VecDeque<Approval> = VecDeque::new();
        for mut approval in self
            .approval
            .take()
            .into_iter()
            .chain(self.queued.drain(..))
        {
            if root(&approval) {
                approval.reply(Answer::Deny { feedback: None });
            } else {
                kept.push_back(approval);
            }
        }
        self.approval = kept.pop_front();
        self.queued = kept;
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
                if !self.covered() {
                    self.input.insert_str(&text);
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Whether an overlay has the keyboard.
    pub fn covered(&self) -> bool {
        self.approval.is_some()
            || self.denied.is_some()
            || self.unavailable.is_some()
            || self.server.is_some()
            || self.agents.is_some()
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
        if let Some(prompt) = &mut self.server {
            return match prompt.handle_key(key) {
                Some(answer) => {
                    self.server = None;
                    Action::Server(answer)
                }
                None => Action::None,
            };
        }
        // The panel can be open over a pending approval, and takes the keys while it is.
        if let Some(panel) = &mut self.agents {
            return match panel.handle_key(key) {
                PanelKey::None => Action::None,
                PanelKey::Close => {
                    self.agents = None;
                    Action::None
                }
                PanelKey::Kill(id) => Action::KillAgent(id),
            };
        }
        if control && key.code == KeyCode::Char('t') && !typing_reason {
            self.agents = Some(AgentsPanel::new());
            return Action::None;
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
        let agent = approval.request.agent.clone();
        approval.reply(answer);
        self.approval = self.queued.pop_front();
        if agent.is_empty() || agent == fontaine_tools::gate::ROOT_AGENT {
            self.transcript.notice(format!("{tool} {note}"));
        } else {
            self.transcript.notice(format!("{agent}: {tool} {note}"));
        }
    }

    fn message_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
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
            parse_command("/nope  with args "),
            Some(Command::Other {
                name: "nope".into(),
                argument: "with args".into()
            })
        );
        assert_eq!(parse_command("/agents"), Some(Command::Agents));
        assert_eq!(
            parse_command("/output-style terse"),
            Some(Command::OutputStyle("terse".into()))
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
