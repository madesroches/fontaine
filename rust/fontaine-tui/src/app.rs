//! The interface state and what each input does to it. Nothing here touches the terminal or the
//! session: key handling returns an [`Action`] for the driver to carry out, so tests can feed
//! keys directly.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use fontaine_core::config::PermissionMode;
use fontaine_core::permissions::{Answer, ApprovalRequest, UnavailableChoice};
use fontaine_core::turn::SessionEvent;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use crate::agents_panel::{AgentsPanel, PanelKey};
use crate::approval::Approval;
use crate::commands::{MenuEntry, builtin_menu, match_menu, parse_command};
use crate::complete::{self, Mention};
use crate::denials::{DeniedKey, DeniedList, UnavailableAnswer, UnavailablePrompt};
use crate::history::History;
use crate::input::InputBuffer;
use crate::model_picker::{ModelPicker, PickerKey, PickerPurpose};
use crate::provider_dialog::{DialogKey, ProviderDialog};
use crate::server_prompt::ServerPrompt;
use crate::theme::Theme;
use crate::transcript::Transcript;

pub use crate::commands::Command;

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
    /// A message typed while the agent works: it waits for the agent's next tool result.
    Queue(String),
    /// Take the queued messages back into the input.
    EditQueued,
    /// A `!` command, without the `!`.
    Shell(String),
    CycleMode,
    Interrupt,
    Quit,
    /// The human approved denied call `n` (as `/denied` lists it) for one retry.
    RetryDenied(usize),
    /// The human answered the unavailable-guardian prompt.
    Guardian(UnavailableAnswer),
    /// The human chose `<account>/<model>` in the model picker.
    PickModel(PickerPurpose, String),
    /// The human asked to stop a subagent.
    KillAgent(String),
    /// The provider dialog asks for something done.
    Providers(DialogKey),
    /// The human answered the question about starting a repository's MCP server.
    Server(bool),
}

/// Ctrl-R: the query typed so far and the history entry it found.
#[derive(Clone, Debug, Default)]
pub struct HistorySearch {
    pub query: String,
    /// Index into the history of the match shown.
    pub found: Option<usize>,
}

/// The `@` candidates for the token under the cursor.
#[derive(Clone, Debug)]
pub struct CompletionList {
    pub mention: Mention,
    pub matches: Vec<String>,
    pub selected: usize,
}

pub struct App {
    pub transcript: Transcript,
    pub input: InputBuffer,
    pub mode: PermissionMode,
    pub model: String,
    pub running: bool,
    pub approval: Option<Approval>,
    /// Approvals waiting behind the open one: several agents can ask at once.
    pub queued_approvals: VecDeque<Approval>,
    /// The `/agents` panel while it is open.
    pub agents: Option<AgentsPanel>,
    /// A repository's MCP server waiting for the human's say-so.
    pub server: Option<ServerPrompt>,
    /// The `/model` or `/guardian-model` picker while it is open.
    pub models: Option<ModelPicker>,
    /// The `/login` dialog while it is open.
    pub providers: Option<ProviderDialog>,
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
    /// A `!` command is running; nothing can be queued or sent until it ends.
    pub shell_running: bool,
    /// Messages waiting for the agent's next tool result, copied from the session.
    pub queued: Vec<String>,
    pub history: History,
    pub search: Option<HistorySearch>,
    pub completion: Option<CompletionList>,
    /// The mention whose list the human closed, which stays closed until the token changes.
    dismissed: Option<(usize, String)>,
    /// The name typed after the `/` that the menu selection applies to, and the selected row.
    /// A different name reads as row 0, so typing needs no reset.
    slash_selected: (String, usize),
    /// The input text the human closed the `/` menu on, which keeps it closed until it changes.
    slash_dismissed: Option<String>,
    /// The workspace files `@` completes, as paths under the roots.
    files: Arc<Vec<String>>,
    pub primary_root: PathBuf,
    pub theme: Theme,
    approvals_opened: u64,
    servers_opened: u64,
}

/// The `/` menu while it is open.
pub struct SlashMenu<'a> {
    pub typed: String,
    pub matches: Vec<&'a MenuEntry>,
    pub selected: usize,
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
            queued_approvals: VecDeque::new(),
            agents: None,
            server: None,
            models: None,
            providers: None,
            menu: builtin_menu(),
            running_agents: 0,
            denied: None,
            unavailable: None,
            pending_notices: Vec::new(),
            shell_running: false,
            queued: Vec::new(),
            history: History::in_memory(""),
            search: None,
            completion: None,
            dismissed: None,
            slash_selected: (String::new(), 0),
            slash_dismissed: None,
            files: Arc::default(),
            primary_root: PathBuf::new(),
            theme: Theme::default(),
            approvals_opened: 0,
            servers_opened: 0,
        }
    }

    /// Puts `text` in front of what is already in the input, as when queued messages come back.
    pub fn restore_to_input(&mut self, text: &str) {
        let current = self.input.text();
        if current.is_empty() {
            self.input.set_text(text);
        } else {
            self.input.set_text(&format!("{text}\n\n{current}"));
        }
        self.history.reset();
        self.refresh_slash();
    }

    /// Whether the input is a `!` shell command.
    pub fn shell_mode(&self) -> bool {
        self.input.first_char() == Some('!')
    }

    pub fn set_files(&mut self, files: Arc<Vec<String>>) {
        if !Arc::ptr_eq(&self.files, &files) {
            self.files = files;
            self.refresh_completion();
        }
    }

    /// Search and completion yield to anything that needs an answer.
    fn refresh_completion(&mut self) {
        let previous = self.completion.take();
        if self.search.is_some() || self.covered() {
            return;
        }
        let Some(mention) = complete::mention_at(&self.input.text(), self.input.cursor()) else {
            self.dismissed = None;
            return;
        };
        if self
            .dismissed
            .as_ref()
            .is_some_and(|(start, query)| *start == mention.start && *query == mention.query)
        {
            return;
        }
        self.dismissed = None;
        let matches = complete::rank(&mention.query, &self.files, &self.primary_root);
        if matches.is_empty() {
            return;
        }
        let selected = previous
            .filter(|list| list.mention.query == mention.query)
            .map_or(0, |list| list.selected.min(matches.len() - 1));
        self.completion = Some(CompletionList {
            mention,
            matches,
            selected,
        });
    }

    fn accept_completion(&mut self) {
        let Some(list) = self.completion.take() else {
            return;
        };
        let path = &list.matches[list.selected];
        self.input
            .replace_range(list.mention.start, list.mention.end, &format!("@{path} "));
        self.history.reset();
        self.refresh_completion();
    }

    fn dismiss_completion(&mut self) {
        if let Some(list) = self.completion.take() {
            self.dismissed = Some((list.mention.start, list.mention.query));
        }
    }

    fn search_step(&mut self, older_than_found: bool) {
        let Some(search) = &mut self.search else {
            return;
        };
        let before = match (older_than_found, search.found) {
            (true, Some(found)) => found,
            _ => self.history.len(),
        };
        match self.history.search(&search.query, before) {
            Some((at, _)) => search.found = Some(at),
            // Stepping past the oldest match keeps it; a changed query with none shows none.
            None if older_than_found && search.found.is_some() => {}
            None => search.found = None,
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('r') if control => self.search_step(true),
            KeyCode::Enter | KeyCode::Tab => {
                let found = self.search.take().and_then(|search| search.found);
                if let Some(at) = found
                    && let Some(text) = self.history.get(at)
                {
                    let text = text.to_string();
                    self.input.set_text(&text);
                    self.history.reset();
                }
                self.refresh_completion();
            }
            KeyCode::Backspace => {
                if let Some(search) = &mut self.search {
                    search.query.pop();
                }
                self.search_step(false);
            }
            KeyCode::Char(c) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(search) = &mut self.search {
                    search.query.push(c);
                }
                self.search_step(false);
            }
            _ => {}
        }
    }

    /// The history entry the search currently shows.
    pub fn search_match(&self) -> Option<&str> {
        self.history.get(self.search.as_ref()?.found?)
    }

    pub fn on_event(&mut self, event: SessionEvent) {
        self.transcript.on_event(event);
    }

    /// Shows the approval, or queues it behind the one the human is answering.
    pub fn begin_approval(&mut self, request: ApprovalRequest, reply: oneshot::Sender<Answer>) {
        let mut approval = Approval::new(request, reply);
        self.approvals_opened += 1;
        approval.id = self.approvals_opened;
        if self.approval.is_none() {
            self.approval = Some(approval);
        } else {
            self.queued_approvals.push_back(approval);
        }
    }

    /// Drops the approvals whose call is gone, such as those of an agent that was killed, and
    /// opens the next one when the open one went away.
    pub fn prune_closed(&mut self) {
        self.queued_approvals
            .retain(|approval| !approval.is_closed());
        if self.approval.as_ref().is_some_and(Approval::is_closed) {
            self.approval = None;
        }
        if self.approval.is_none() {
            self.approval = self.queued_approvals.pop_front();
        }
    }

    /// The calls `agent` is waiting for the human to decide, as the call text.
    pub fn pending_for(&self, agent: &str) -> Vec<String> {
        self.approval
            .iter()
            .chain(self.queued_approvals.iter())
            .filter(|approval| approval.request.agent == agent)
            .map(|approval| format!("{} {}", approval.request.tool, approval.request.summary))
            .collect()
    }

    pub fn begin_server(&mut self, mut prompt: ServerPrompt) {
        self.servers_opened += 1;
        prompt.id = self.servers_opened;
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
            .chain(self.queued_approvals.drain(..))
        {
            if root(&approval) {
                approval.reply(Answer::Deny { feedback: None });
            } else {
                kept.push_back(approval);
            }
        }
        self.approval = kept.pop_front();
        self.queued_approvals = kept;
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
        let action = self.dispatch_input(input);
        // Paste and history-search accept change the text outside handle_key's tails.
        self.refresh_slash();
        action
    }

    fn dispatch_input(&mut self, input: Input) -> Action {
        match input {
            Input::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Input::Paste(text) => {
                // A pasted key lands in the dialog's masked field, never in the message box.
                // Overlays that take keys before the dialog also own the paste.
                let above_dialog = self.approval.is_some()
                    || self.denied.is_some()
                    || self.unavailable.is_some()
                    || self.server.is_some()
                    || self.models.is_some();
                if above_dialog {
                } else if let Some(dialog) = &mut self.providers {
                    dialog.handle_paste(&text);
                } else if !self.covered() {
                    if let Some(search) = &mut self.search {
                        search.query.push_str(&text.replace(['\r', '\n'], " "));
                        self.search_step(false);
                    } else {
                        self.input.insert_str(&text);
                        self.history.reset();
                        self.refresh_completion();
                    }
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
            || self.models.is_some()
            || self.providers.is_some()
            || self.agents.is_some()
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if is_cycle_key(&key) {
            return Action::CycleMode;
        }
        if control && key.code == KeyCode::Char('c') {
            if !self.covered() && self.close_palette() {
                return Action::None;
            }
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
        if let Some(picker) = &mut self.models {
            let purpose = picker.purpose();
            return match picker.handle_key(key) {
                PickerKey::None => Action::None,
                PickerKey::Close => {
                    self.models = None;
                    Action::None
                }
                PickerKey::Pick(_, reference) => {
                    self.models = None;
                    Action::PickModel(purpose, reference)
                }
            };
        }
        if let Some(dialog) = &mut self.providers {
            return match dialog.handle_key(key) {
                DialogKey::None => Action::None,
                DialogKey::Close => {
                    self.providers = None;
                    Action::None
                }
                // The model picker takes the screen the dialog had.
                request @ DialogKey::PickModel(_) => {
                    self.providers = None;
                    Action::Providers(request)
                }
                request => Action::Providers(request),
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
        if key.code == KeyCode::Esc && self.approval.is_none() && self.close_palette() {
            return Action::None;
        }
        if key.code == KeyCode::Esc && self.running && !typing_reason {
            return Action::Interrupt;
        }
        if self.approval.is_some() {
            self.approval_key(key);
            return Action::None;
        }
        if self.search.is_some() {
            self.search_key(key);
            return Action::None;
        }
        if self.completion.is_some() && self.completion_key(key) {
            return Action::None;
        }
        if let Some(action) = self.slash_key(key) {
            self.refresh_slash();
            return action;
        }
        if control && key.code == KeyCode::Char('d') {
            return if !self.running && self.input.is_empty() {
                Action::Quit
            } else {
                Action::None
            };
        }
        let action = self.message_key(key);
        self.refresh_completion();
        self.refresh_slash();
        action
    }

    /// Forgets the menu's dismissal and selection once the text they were made for is gone,
    /// so neither resurfaces when the same text is typed again.
    fn refresh_slash(&mut self) {
        let text = self.input.text();
        if self.slash_dismissed.as_deref() != Some(&text) {
            self.slash_dismissed = None;
        }
        if text.strip_prefix('/') != Some(self.slash_selected.0.as_str()) {
            self.slash_selected = (String::new(), 0);
        }
    }

    /// Closes an open history search, completion list or slash menu; false when none was open.
    fn close_palette(&mut self) -> bool {
        if self.search.take().is_some() {
            self.refresh_completion();
            return true;
        }
        if self.completion.is_some() {
            self.dismiss_completion();
            return true;
        }
        if self.slash_menu().is_some() {
            self.slash_dismissed = Some(self.input.text());
            return true;
        }
        false
    }

    /// The `/` menu, when the input is a bare `/name` and nothing else wants the keys.
    pub fn slash_menu(&self) -> Option<SlashMenu<'_>> {
        if self.covered()
            || self.search.is_some()
            || self.completion.is_some()
            || self.history.navigating()
        {
            return None;
        }
        let text = self.input.text();
        let typed = text.strip_prefix('/')?;
        if typed.contains(char::is_whitespace) || self.slash_dismissed.as_deref() == Some(&text) {
            return None;
        }
        let matches = match_menu(&self.menu, typed);
        if matches.is_empty() {
            return None;
        }
        let selected = if self.slash_selected.0 == typed {
            self.slash_selected.1.min(matches.len() - 1)
        } else {
            0
        };
        Some(SlashMenu {
            typed: typed.to_string(),
            matches,
            selected,
        })
    }

    /// Keys the `/` menu takes; `None` when the key is for the input.
    fn slash_key(&mut self, key: KeyEvent) -> Option<Action> {
        let menu = self.slash_menu()?;
        let (typed, count, selected) = (menu.typed.clone(), menu.matches.len(), menu.selected);
        let entry = menu.matches[selected];
        let name = entry.name.clone();
        let argument = entry.hint.is_some();
        let required = entry.requires_argument();
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up if plain => {
                self.slash_selected = (typed, selected.saturating_sub(1));
            }
            KeyCode::Down if plain => {
                self.slash_selected = (typed, (selected + 1).min(count - 1));
            }
            KeyCode::Tab if plain => self.complete_slash(&name, argument),
            KeyCode::Enter if plain => {
                if required {
                    self.complete_slash(&name, argument);
                } else {
                    self.complete_slash(&name, false);
                    return Some(self.enter(false));
                }
            }
            _ => return None,
        }
        Some(Action::None)
    }

    fn complete_slash(&mut self, name: &str, space: bool) {
        let space = if space { " " } else { "" };
        self.input.set_text(&format!("/{name}{space}"));
        self.history.reset();
    }

    /// Keys the completion list takes; false when the key is for the input.
    fn completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(list) = &mut self.completion else {
            return false;
        };
        match key.code {
            KeyCode::Up => list.selected = list.selected.saturating_sub(1),
            KeyCode::Down => list.selected = (list.selected + 1).min(list.matches.len() - 1),
            KeyCode::Tab => self.accept_completion(),
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.accept_completion();
            }
            _ => return false,
        }
        true
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
        self.approval = self.queued_approvals.pop_front();
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
                self.history.reset();
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search = Some(HistorySearch::default());
                self.completion = None;
                self.search_step(false);
            }
            KeyCode::Up if self.input.cursor_position().0 == 0 => return self.up(),
            KeyCode::Down if self.on_last_line() && self.history.navigating() => {
                if let Some(text) = self.history.newer() {
                    self.input.set_text(&text);
                }
            }
            _ => {
                let before = self.input.text();
                self.input.handle_key(key);
                if self.input.text() != before {
                    self.history.reset();
                }
            }
        }
        Action::None
    }

    fn on_last_line(&self) -> bool {
        self.input.cursor_position().0 + 1 == self.input.lines().len()
    }

    /// Up on the first line: the queued messages if there are any to take back, else older
    /// history.
    fn up(&mut self) -> Action {
        if !self.queued.is_empty() && !self.shell_running {
            return Action::EditQueued;
        }
        self.recall_older();
        Action::None
    }

    pub(crate) fn recall_older(&mut self) {
        if let Some(text) = self.history.older(&self.input.text()) {
            self.input.set_text(&text);
        }
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
        if self.shell_running {
            self.transcript
                .notice("a shell command is running; wait for it or press Esc to interrupt");
            return Action::None;
        }
        if let Some(rest) = text.strip_prefix('!') {
            if self.running {
                self.transcript
                    .notice("a turn is running; run shell commands between turns");
                return Action::None;
            }
            let command = rest.trim().to_string();
            if command.is_empty() {
                return Action::None;
            }
            self.input.take();
            return Action::Shell(command);
        }
        if self.running {
            if parse_command(&text).is_some() {
                self.transcript
                    .notice("a turn is running; wait for it or press Esc to interrupt");
                return Action::None;
            }
            self.input.take();
            return Action::Queue(text.trim().to_string());
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

    fn dialog_on_key_screen() -> ProviderDialog {
        use fontaine_core::accounts::{AccountRow, SignInState};
        let rows = vec![AccountRow {
            name: "work".into(),
            kind: fontaine_core::config::AccountKind::named("anthropic"),
            endpoint: "default".into(),
            state: SignInState::NotSignedIn,
        }];
        ProviderDialog::new(rows, Some("work")).0
    }

    #[test]
    fn the_provider_dialog_takes_the_keyboard_and_pastes() {
        let mut app = app();
        app.providers = Some(dialog_on_key_screen());
        assert!(app.covered());
        type_text(&mut app, "sk-");
        app.handle_input(Input::Paste("abc\n".into()));
        assert!(app.input.is_empty());
        assert!(app.history.is_empty());
        let action = press(&mut app, KeyCode::Enter);
        assert_eq!(
            action,
            Action::Providers(DialogKey::StoreKey {
                account: "work".into(),
                key: crate::provider_dialog::Secret::new("sk-abc"),
            })
        );
        assert!(app.input.is_empty());
        assert!(app.transcript.entries.is_empty());
    }

    #[test]
    fn the_dialog_closes_on_escape_and_hands_over_to_the_picker() {
        let mut app = app();
        let (dialog, _) = ProviderDialog::new(Vec::new(), None);
        app.providers = Some(dialog);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.providers.is_none());
        assert!(!app.covered());

        let mut app = self::app();
        app.providers = Some(dialog_on_key_screen());
        press(&mut app, KeyCode::Esc);
        assert_eq!(
            press(&mut app, KeyCode::Char('m')),
            Action::Providers(DialogKey::PickModel("work".into()))
        );
        assert!(app.providers.is_none());
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

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.handle_input(key(code, KeyModifiers::NONE))
    }

    fn ctrl(app: &mut App, c: char) -> Action {
        app.handle_input(key(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn last_notice(app: &App) -> Option<&str> {
        match app.transcript.entries.last() {
            Some(crate::transcript::Entry::Notice(text)) => Some(text),
            _ => None,
        }
    }

    fn approval() -> Approval {
        let (reply, _answer) = oneshot::channel();
        let request = ApprovalRequest {
            tool: "shell".into(),
            agent: "main".into(),
            tool_use_id: None,
            args: serde_json::json!({}),
            summary: "ls".into(),
            tier: fontaine_core::tier::Tier::AllowRule,
            reason: "r".into(),
            offers: fontaine_core::permissions::Offers::default(),
            proposed_rule: None,
        };
        Approval::new(request, reply)
    }

    #[test]
    fn a_message_during_a_turn_is_queued_and_a_command_is_refused() {
        let mut app = app();
        app.running = true;
        type_text(&mut app, "also this");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Queue("also this".into())
        );
        assert!(app.input.is_empty());

        type_text(&mut app, "/denied");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(
            last_notice(&app),
            Some("a turn is running; wait for it or press Esc to interrupt")
        );
        assert_eq!(app.input.text(), "/denied");
        app.input.take();

        type_text(&mut app, "!ls");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(
            last_notice(&app),
            Some("a turn is running; run shell commands between turns")
        );
        assert_eq!(app.input.text(), "!ls");
    }

    #[test]
    fn nothing_is_sent_or_queued_during_a_shell_run() {
        let mut app = app();
        app.running = true;
        app.shell_running = true;
        app.queued = vec!["old".into()];
        for text in ["hello", "/denied", "!ls"] {
            app.input.take();
            type_text(&mut app, text);
            assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
            assert_eq!(
                last_notice(&app),
                Some("a shell command is running; wait for it or press Esc to interrupt")
            );
            assert_eq!(app.input.text(), text);
        }
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
    }

    #[test]
    fn up_takes_back_queued_messages() {
        let mut app = app();
        app.running = true;
        app.queued = vec!["one".into()];
        assert_eq!(press(&mut app, KeyCode::Up), Action::EditQueued);
        app.restore_to_input("one\n\ntwo");
        assert_eq!(app.input.text(), "one\n\ntwo");
        type_text(&mut app, "!");
        app.restore_to_input("back");
        assert_eq!(app.input.text(), "back\n\none\n\ntwo!");
    }

    #[test]
    fn a_bang_input_runs_a_shell_command() {
        let mut app = app();
        type_text(&mut app, "!");
        assert!(app.shell_mode());
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.input.text(), "!");
        type_text(&mut app, "  echo hi ");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Shell("echo hi".into())
        );
        assert!(app.input.is_empty());
        assert!(!app.shell_mode());
    }

    #[test]
    fn up_and_down_walk_the_history_on_the_edge_lines_only() {
        let mut app = app();
        app.history.push("first");
        app.history.push("second\nline");
        type_text(&mut app, "draft");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.input.text(), "second\nline");
        // On the entry's last line Up moves the cursor, not the history.
        app.input.set_text("second\nline");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.input.text(), "second\nline");
        assert_eq!(app.input.cursor_position().0, 0);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.input.text(), "first");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.input.text(), "second\nline");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.input.text(), "draft");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.input.text(), "draft");
    }

    #[test]
    fn editing_a_recalled_entry_stops_the_walk() {
        let mut app = app();
        app.history.push("one");
        app.history.push("two");
        press(&mut app, KeyCode::Up);
        type_text(&mut app, "!");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.input.text(), "two!");
    }

    #[test]
    fn ctrl_r_searches_and_enter_fills_without_sending() {
        let mut app = app();
        for entry in ["fix parser", "run tests", "parser docs"] {
            app.history.push(entry);
        }
        type_text(&mut app, "keep");
        ctrl(&mut app, 'r');
        assert!(app.search.is_some());
        type_text(&mut app, "pars");
        assert_eq!(app.search_match(), Some("parser docs"));
        ctrl(&mut app, 'r');
        assert_eq!(app.search_match(), Some("fix parser"));
        ctrl(&mut app, 'r');
        assert_eq!(app.search_match(), Some("fix parser"));
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(app.search.is_none());
        assert_eq!(app.input.text(), "fix parser");

        ctrl(&mut app, 'r');
        type_text(&mut app, "tests");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.search.is_none());
        assert_eq!(app.input.text(), "fix parser");

        ctrl(&mut app, 'r');
        type_text(&mut app, "zzz");
        assert_eq!(app.search_match(), None);
        app.handle_input(Input::Paste("tests".into()));
        assert_eq!(app.search.as_ref().unwrap().query, "zzztests");
        assert_eq!(ctrl(&mut app, 'c'), Action::None);
        assert!(app.search.is_none());
    }

    fn app_with_files() -> App {
        let mut app = app();
        app.primary_root = PathBuf::from("/w");
        app.set_files(Arc::new(vec![
            "/w/src/main.rs".to_string(),
            "/w/src/manual.md".to_string(),
            "/w/README.md".to_string(),
        ]));
        app
    }

    #[test]
    fn a_completion_list_inserts_the_selected_path() {
        let mut app = app_with_files();
        type_text(&mut app, "read @ma");
        let list = app.completion.as_ref().expect("a list");
        assert_eq!(list.matches, vec!["src/main.rs", "src/manual.md"]);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Tab), Action::None);
        assert_eq!(app.input.text(), "read @src/manual.md ");
        assert!(app.completion.is_none());

        app.input.take();
        type_text(&mut app, "@read");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.input.text(), "@README.md ");
    }

    #[test]
    fn esc_closes_the_list_without_interrupting_until_the_token_changes() {
        let mut app = app_with_files();
        app.running = true;
        type_text(&mut app, "@ma");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.completion.is_none());
        assert_eq!(app.input.text(), "@ma");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);
        type_text(&mut app, "i");
        assert!(app.completion.is_some());
    }

    #[test]
    fn a_paste_edits_the_input_and_refreshes_the_list() {
        let mut app = app_with_files();
        app.handle_input(Input::Paste("see @READ".into()));
        assert_eq!(app.input.text(), "see @READ");
        assert!(app.completion.is_some());
    }

    fn unavailable() -> UnavailablePrompt {
        let (reply, _choice) = oneshot::channel();
        let info = fontaine_core::permissions::GuardianUnavailable {
            model: "a/b".into(),
            reason: "no key".into(),
            tool: "shell".into(),
        };
        UnavailablePrompt::new(info, Vec::new(), reply)
    }

    #[test]
    fn an_open_prompt_keeps_its_keys_from_the_completion_list() {
        for open in ["approval", "denied", "unavailable"] {
            let mut app = app_with_files();
            type_text(&mut app, "@ma");
            assert!(app.completion.is_some());
            match open {
                "approval" => app.approval = Some(approval()),
                "denied" => app.denied = Some(DeniedList::new(Vec::new())),
                _ => app.unavailable = Some(unavailable()),
            }
            for code in [
                KeyCode::Down,
                KeyCode::Tab,
                KeyCode::Enter,
                KeyCode::Char('y'),
            ] {
                let action = press(&mut app, code);
                assert!(matches!(action, Action::None), "{open}: {action:?}");
                assert_eq!(app.input.text(), "@ma", "{open}");
                assert_eq!(app.completion.as_ref().unwrap().selected, 0, "{open}");
            }
        }
    }

    #[test]
    fn an_open_approval_keeps_its_keys_from_the_search() {
        let mut app = app();
        app.history.push("earlier");
        ctrl(&mut app, 'r');
        app.approval = Some(approval());
        let before = app.approval.as_ref().unwrap().selected();
        press(&mut app, KeyCode::Down);
        type_text(&mut app, "x");
        assert_ne!(app.approval.as_ref().unwrap().selected(), before);
        assert_eq!(app.search.as_ref().unwrap().query, "");
        assert!(app.input.is_empty());
    }

    #[test]
    fn esc_with_the_denied_list_open_closes_it_and_leaves_the_completion() {
        let mut app = app_with_files();
        app.running = true;
        type_text(&mut app, "@ma");
        app.denied = Some(DeniedList::new(Vec::new()));
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.denied.is_none());
        assert!(app.completion.is_some());
    }

    #[test]
    fn esc_with_an_approval_open_while_running_interrupts() {
        let mut app = app();
        app.running = true;
        app.approval = Some(approval());
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);
    }

    fn menu_names(app: &App) -> Vec<String> {
        app.slash_menu()
            .map(|m| m.matches.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default()
    }

    fn selected_name(app: &App) -> String {
        let menu = app.slash_menu().expect("menu is open");
        menu.matches[menu.selected].name.clone()
    }

    #[test]
    fn the_slash_menu_opens_only_for_a_bare_name() {
        let mut app = app();
        assert!(app.slash_menu().is_none());
        type_text(&mut app, "/");
        assert!(app.slash_menu().is_some());
        type_text(&mut app, "mod");
        assert_eq!(menu_names(&app), ["model", "guardian-model"]);
        type_text(&mut app, " x");
        assert!(app.slash_menu().is_none());
        app.input.set_text("/zzz");
        assert!(app.slash_menu().is_none());
        app.input.set_text("hello /mod");
        assert!(app.slash_menu().is_none());
    }

    #[test]
    fn up_and_down_move_the_selection_and_clamp() {
        let mut app = app();
        type_text(&mut app, "/mod");
        assert_eq!(selected_name(&app), "model");
        press(&mut app, KeyCode::Up);
        assert_eq!(selected_name(&app), "model");
        press(&mut app, KeyCode::Down);
        assert_eq!(selected_name(&app), "guardian-model");
        press(&mut app, KeyCode::Down);
        assert_eq!(selected_name(&app), "guardian-model");
        // Typing another character starts over at the top.
        press(&mut app, KeyCode::Backspace);
        assert_eq!(selected_name(&app), "model");
    }

    #[test]
    fn tab_completes_with_a_space_only_when_there_is_an_argument() {
        let mut app = app();
        type_text(&mut app, "/add");
        assert_eq!(press(&mut app, KeyCode::Tab), Action::None);
        assert_eq!(app.input.text(), "/add-dir ");
        assert!(app.slash_menu().is_none());

        app.input.take();
        type_text(&mut app, "/cle");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/clear");
        assert!(app.slash_menu().is_some());
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/clear");
    }

    #[test]
    fn tab_on_an_alias_match_substitutes_the_canonical_name() {
        let mut app = app();
        type_text(&mut app, "/reset");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/clear");
    }

    #[test]
    fn enter_runs_a_command_without_a_required_argument_and_completes_one_with() {
        let mut app = app();
        type_text(&mut app, "/rese");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Submit("/clear".into())
        );
        assert!(app.input.is_empty());

        type_text(&mut app, "/plan");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Submit("/plan".into())
        );

        type_text(&mut app, "/add");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.input.text(), "/add-dir ");
    }

    #[test]
    fn enter_on_a_command_while_a_turn_runs_is_refused_as_before() {
        let mut app = app();
        app.running = true;
        type_text(&mut app, "/clear");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(last_notice(&app).is_some_and(|n| n.contains("a turn is running")));
    }

    #[test]
    fn esc_dismisses_the_menu_until_the_text_changes_and_does_not_interrupt() {
        let mut app = app();
        app.running = true;
        type_text(&mut app, "/mod");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.slash_menu().is_none());
        type_text(&mut app, "e");
        assert!(app.slash_menu().is_some());
        // With the menu closed, the next Esc interrupts the turn.
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);
    }

    #[test]
    fn a_paste_clears_the_menu_dismissal() {
        let mut app = app();
        type_text(&mut app, "/mod");
        press(&mut app, KeyCode::Esc);
        app.handle_input(Input::Paste("e".into()));
        press(&mut app, KeyCode::Backspace);
        assert!(app.slash_menu().is_some());
    }

    #[test]
    fn a_dismissed_menu_reopens_for_the_same_text_in_a_later_message() {
        let mut app = app();
        type_text(&mut app, "/");
        press(&mut app, KeyCode::Esc);
        assert!(app.slash_menu().is_none());
        press(&mut app, KeyCode::Backspace);
        type_text(&mut app, "/");
        assert!(app.slash_menu().is_some());
    }

    #[test]
    fn the_menu_selection_does_not_return_with_the_same_typed_name() {
        let mut app = app();
        type_text(&mut app, "/mod");
        press(&mut app, KeyCode::Down);
        let moved = selected_name(&app);
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Backspace);
        assert_ne!(selected_name(&app), moved);
    }

    #[test]
    fn ctrl_c_closes_the_menu_before_it_quits() {
        let mut app = app();
        type_text(&mut app, "/mod");
        assert_eq!(ctrl(&mut app, 'c'), Action::None);
        assert!(app.slash_menu().is_none());
        assert_eq!(ctrl(&mut app, 'c'), Action::Quit);
    }

    #[test]
    fn the_menu_stays_shut_and_keyless_under_every_panel() {
        let panels: Vec<fn(&mut App)> = vec![
            |a| a.approval = Some(approval()),
            |a| a.agents = Some(AgentsPanel::new()),
            |a| a.providers = Some(dialog_on_key_screen()),
            |a| a.server = Some(ServerPrompt::new("s".into(), "d".into())),
            |a| a.denied = Some(DeniedList::new(Vec::new())),
        ];
        for open in panels {
            let mut app = app();
            type_text(&mut app, "/add");
            open(&mut app);
            assert!(app.covered());
            assert!(app.slash_menu().is_none());
            press(&mut app, KeyCode::Tab);
            assert_eq!(app.input.text(), "/add");
        }
    }

    #[test]
    fn the_menu_yields_to_history_search_and_the_file_list() {
        let mut app = app_with_files();
        type_text(&mut app, "/mod");
        ctrl(&mut app, 'r');
        assert!(app.slash_menu().is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.slash_menu().is_some());

        app.input.set_text("/mod @ma");
        app.refresh_completion();
        assert!(app.completion.is_some());
        assert!(app.slash_menu().is_none());
    }

    #[test]
    fn with_the_menu_closed_the_keys_behave_as_before() {
        let mut app = app();
        app.history.push("older");
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.input.text(), "older");
        app.input.take();
        type_text(&mut app, "hello");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Submit("hello".into())
        );
    }

    #[test]
    fn recalling_a_slash_entry_does_not_hand_the_next_up_to_the_menu() {
        let mut app = app();
        app.history.push("older");
        app.history.push("/mod");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.input.text(), "/mod");
        assert!(app.slash_menu().is_none());
        press(&mut app, KeyCode::Up);
        assert_eq!(app.input.text(), "older");
    }
}
