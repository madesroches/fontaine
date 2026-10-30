//! The interface state and what each input does to it. Nothing here touches the terminal or the
//! session: key handling returns an [`Action`] for the driver to carry out, so tests can feed
//! keys directly.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use fontaine_core::config::PermissionMode;
use fontaine_core::permissions::{Answer, ApprovalRequest, UnavailableChoice};
use fontaine_core::steering::Queued;
use fontaine_core::turn::SessionEvent;
use fontaine_tools::ask_user::{Question, QuestionReply};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use crate::agent_list::{AgentList, ListKey, ListRow};
use crate::agents_panel::{AgentsPanel, PanelKey};
use crate::approval::Approval;
use crate::commands::{MenuEntry, builtin_menu, common_prefix, match_menu, parse_command};
use crate::complete::{self, Token};
use crate::config_panel::{ConfigKey, ConfigPanel};
use crate::denials::{DeniedKey, DeniedList, UnavailableAnswer, UnavailablePrompt};
use crate::guardian_prompt::GuardianOffPrompt;
use crate::history::History;
use crate::input::InputBuffer;
use crate::model_picker::{ModelPicker, PickerKey, PickerPurpose};
use crate::provider_dialog::{DialogKey, ProviderDialog};
use crate::question::QuestionPanel;
use crate::resume_picker::{ResumeKey, ResumePicker};
use crate::rewind::{RewindKey, RewindPicker};
use crate::server_prompt::ServerPrompt;
use crate::theme::Theme;
use crate::todo_panel::TodoPanel;
use crate::transcript::Transcript;
use fontaine_core::checkpoint::{RewindPlan, RewindWhat};
use fontaine_core::config_catalog::ModelSlot;

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
    /// Input typed while the agent works: a plain message, a skill invocation, a built-in
    /// command or a `!` line, queued as typed until the turn ends.
    Queue(Queued),
    /// A command that acts on the UI alone, run immediately even mid-turn; `raw` is the
    /// trimmed line for the history.
    Command {
        command: Command,
        raw: String,
    },
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
    /// The human chose a session in the `/resume` picker.
    ResumeSession(String),
    /// The human chose a prompt in the `/rewind` picker, which waits for what a rewind to it
    /// would do.
    SelectRewind(u64),
    /// The human confirmed a rewind.
    Rewind {
        plan: RewindPlan,
        what: RewindWhat,
        overwrite_changed: bool,
    },
    /// Esc twice at an idle, empty prompt.
    OpenRewind,
    /// The human asked to stop a subagent.
    KillAgent(String),
    /// Text typed in a subagent's view, for that agent.
    MessageAgent {
        id: String,
        text: String,
    },
    /// The human asked to kill a background job.
    KillJob(String),
    /// The provider dialog asks for something done.
    Providers(DialogKey),
    /// The human answered the question about starting a repository's MCP server.
    Server(bool),
    /// Esc at the idle prompt while a self-paced loop waits for its next iteration.
    CancelWakeup,
    /// The settings panel asks for a value to be saved or reset.
    Config(ConfigKey),
    /// The settings panel hands over to the picker for a model setting.
    OpenPicker(ModelSlot),
    /// The human answered the `/guardian-disable` confirmation.
    GuardianOff(bool),
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
    pub mention: Token,
    pub matches: Vec<String>,
    pub selected: usize,
}

/// What the human sees of one subagent: its own transcript, drawn like the main chat's.
#[derive(Default)]
pub struct AgentView {
    pub transcript: Transcript,
}

/// What a view that cannot run a command tells the human.
pub const MAIN_ONLY: &str = "switch back to main to run this (↓ then Enter on main)";

pub struct App {
    /// The main conversation.
    pub transcript: Transcript,
    /// The subagents' transcripts, by agent id, filled as their events arrive.
    pub agent_views: HashMap<String, AgentView>,
    /// The agent whose transcript is on screen; `None` is the main conversation.
    pub viewing: Option<String>,
    /// The next render erases the screen and its scrollback and writes the shown transcript
    /// again from the top.
    pub reprint: bool,
    /// The screen wrote transcript rows into scrollback while the agents panel covered the live
    /// area, which leaves the last messages out of sight when it closes.
    pub committed_under_overlay: bool,
    /// The agents listed below the status line.
    pub agent_list: AgentList,
    /// What the human typed to the viewed agent that it has not read yet.
    pub queued_agent: Vec<String>,
    pub input: InputBuffer,
    pub mode: PermissionMode,
    pub model: String,
    pub running: bool,
    pub approval: Option<Approval>,
    /// Approvals waiting behind the open one: several agents can ask at once.
    pub queued_approvals: VecDeque<Approval>,
    /// The model's questions for the human, the one being answered first. They wait behind an
    /// open approval, which can come from a background agent while the root asks.
    pub questions: VecDeque<QuestionPanel>,
    /// The `/agents` panel while it is open.
    pub agents: Option<AgentsPanel>,
    /// The model's task list, shown above the input.
    pub todos: TodoPanel,
    /// A repository's MCP server waiting for the human's say-so.
    pub server: Option<ServerPrompt>,
    /// The `/guardian-disable` confirmation while it is open.
    pub guardian_off_prompt: Option<GuardianOffPrompt>,
    /// The escape hatch is on; the status bar says so for as long as it stays on.
    pub guardian_off: bool,
    /// The `/model` or `/guardian-model` picker while it is open.
    pub models: Option<ModelPicker>,
    /// The `/resume` picker while it is open.
    pub resume: Option<ResumePicker>,
    /// The `/rewind` picker while it is open.
    pub rewind: Option<RewindPicker>,
    /// When an Esc nothing else claimed was pressed, for the second of a double press.
    pub(crate) last_esc: Option<std::time::Instant>,
    /// The `/config` panel while it is open.
    pub config: Option<ConfigPanel>,
    /// The `/login` dialog while it is open.
    pub providers: Option<ProviderDialog>,
    /// The commands and user-invocable skills the `/` menu lists.
    pub menu: Vec<MenuEntry>,
    /// Subagents running right now, for the status bar.
    pub running_agents: usize,
    /// Scheduled tasks, for the status bar.
    pub scheduled: usize,
    /// The active goal's age and whether it waits for the user, for the status bar.
    pub goal: Option<(std::time::Duration, bool)>,
    /// A self-paced loop has its next iteration pending, which Esc at the idle prompt cancels.
    pub wakeup_pending: bool,
    /// `/denied` while it is open.
    pub denied: Option<DeniedList>,
    /// The pinned guardian cannot be used, and the human has to say what to do.
    pub unavailable: Option<UnavailablePrompt>,
    /// Harness notes the next message to the agent carries, such as a retry the human approved.
    /// They are not the user's words and never enter the ledger as such.
    pub pending_notices: Vec<String>,
    /// Set as the session ends: the next render writes out everything still held back.
    pub flush: bool,
    /// A `!` command is running; nothing can be queued or sent until it ends.
    pub shell_running: bool,
    /// Messages waiting for the agent's next tool result, copied from the session.
    pub queued: Vec<String>,
    pub history: History,
    pub search: Option<HistorySearch>,
    pub completion: Option<CompletionList>,
    /// The mention whose list the human closed, which stays closed until the token changes.
    dismissed: Option<(usize, String)>,
    /// Where the `/` token starts and the name typed after it, which the menu selection applies
    /// to, and the selected row. A different token reads as row 0, so typing needs no reset.
    slash_selected: (usize, String, usize),
    /// The input text the human closed the `/` menu on, which keeps it closed until it changes.
    slash_dismissed: Option<String>,
    /// The workspace files `@` completes, as paths under the roots.
    files: Arc<Vec<String>>,
    pub primary_root: PathBuf,
    pub theme: Theme,
    approvals_opened: u64,
    servers_opened: u64,
    guardian_off_opened: u64,
    questions_opened: u64,
}

/// The `/` menu while it is open.
pub struct SlashMenu<'a> {
    /// The `/token` under the cursor; `token.query` is the typed name.
    pub token: Token,
    /// Whether the token is the whole input, the only place Enter belongs to the menu.
    pub bare: bool,
    pub matches: Vec<&'a MenuEntry>,
    pub selected: usize,
}

/// How soon the second Esc must follow the first to open `/rewind`.
const ESC_DOUBLE_PRESS: std::time::Duration = std::time::Duration::from_millis(600);

fn is_cycle_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

impl App {
    pub fn new(mode: PermissionMode, model: String) -> App {
        App {
            transcript: Transcript::default(),
            agent_views: HashMap::new(),
            viewing: None,
            reprint: false,
            committed_under_overlay: false,
            agent_list: AgentList::default(),
            queued_agent: Vec::new(),
            input: InputBuffer::default(),
            mode,
            model,
            running: false,
            approval: None,
            queued_approvals: VecDeque::new(),
            questions: VecDeque::new(),
            agents: None,
            todos: TodoPanel::default(),
            server: None,
            guardian_off_prompt: None,
            guardian_off: false,
            models: None,
            resume: None,
            rewind: None,
            last_esc: None,
            config: None,
            providers: None,
            menu: builtin_menu(),
            running_agents: 0,
            scheduled: 0,
            goal: None,
            wakeup_pending: false,
            denied: None,
            unavailable: None,
            pending_notices: Vec::new(),
            flush: false,
            shell_running: false,
            queued: Vec::new(),
            history: History::in_memory(""),
            search: None,
            completion: None,
            dismissed: None,
            slash_selected: (0, String::new(), 0),
            slash_dismissed: None,
            files: Arc::default(),
            primary_root: PathBuf::new(),
            theme: Theme::default(),
            approvals_opened: 0,
            servers_opened: 0,
            guardian_off_opened: 0,
            questions_opened: 0,
        }
    }

    /// The transcript on screen.
    pub fn shown(&self) -> &Transcript {
        self.viewing
            .as_ref()
            .and_then(|id| self.agent_views.get(id))
            .map_or(&self.transcript, |view| &view.transcript)
    }

    pub fn shown_mut(&mut self) -> &mut Transcript {
        match &self.viewing {
            Some(id) => &mut self.view_of(id.clone()).transcript,
            None => &mut self.transcript,
        }
    }

    /// The shown transcript beside the theme it is drawn with.
    pub(crate) fn shown_with_theme(&mut self) -> (&mut Transcript, &Theme) {
        let App {
            transcript,
            agent_views,
            viewing,
            theme,
            ..
        } = self;
        let shown = match viewing.as_ref().and_then(|id| agent_views.get_mut(id)) {
            Some(view) => &mut view.transcript,
            None => transcript,
        };
        (shown, theme)
    }

    /// The view of `id`, started empty the first time it is asked for.
    pub fn view_of(&mut self, id: String) -> &mut AgentView {
        let glyphs = self.transcript.glyphs;
        self.agent_views.entry(id).or_insert_with(|| {
            let mut view = AgentView::default();
            view.transcript.glyphs = glyphs;
            view
        })
    }

    /// Shows `target`'s transcript, `None` for the main conversation. The screen is erased and
    /// the transcript written again on the next render.
    pub fn view(&mut self, target: Option<String>) {
        if let Some(id) = &target {
            self.view_of(id.clone());
        }
        self.viewing = target;
        self.reprint = true;
        self.committed_under_overlay = false;
        self.agent_list.focused = false;
    }

    /// The list row of the agent on screen, for the input box's header.
    pub fn viewed_row(&self) -> Option<&ListRow> {
        let id = self.viewing.as_deref()?;
        self.agent_list
            .rows()
            .iter()
            .find(|row| row.id.as_deref() == Some(id))
    }

    /// Notes where the main transcript ends, when an agent is on screen: what a command writes
    /// there meanwhile belongs in the view the human is looking at.
    pub(crate) fn notice_mark(&self) -> Option<usize> {
        self.viewing.as_ref().map(|_| self.transcript.entries.len())
    }

    /// Moves what was written to the main transcript since `mark` into the shown one.
    pub(crate) fn move_notices_to_shown(&mut self, mark: Option<usize>) {
        let Some(mark) = mark else {
            return;
        };
        if self.transcript.entries.len() <= mark {
            return;
        }
        let moved = self.transcript.entries.split_off(mark);
        self.shown_mut().entries.extend(moved);
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

    /// Shows the questions, or queues them behind the ones the human is answering.
    pub fn begin_question(
        &mut self,
        agent: String,
        questions: Vec<Question>,
        reply: oneshot::Sender<QuestionReply>,
    ) {
        let mut panel = QuestionPanel::new(agent, questions, reply);
        self.questions_opened += 1;
        panel.id = self.questions_opened;
        self.questions.push_back(panel);
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
        self.questions.retain(|panel| !panel.is_closed());
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

    pub fn begin_guardian_off(&mut self) {
        self.guardian_off_opened += 1;
        let mut prompt = GuardianOffPrompt::new();
        prompt.id = self.guardian_off_opened;
        self.guardian_off_prompt = Some(prompt);
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
        let (root, background): (VecDeque<_>, VecDeque<_>) =
            self.questions.drain(..).partition(|panel| {
                panel.agent.is_empty() || panel.agent == fontaine_tools::gate::ROOT_AGENT
            });
        for mut panel in root {
            panel.reply(QuestionReply::Declined);
        }
        self.questions = background;
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
                    || !self.questions.is_empty()
                    || self.denied.is_some()
                    || self.unavailable.is_some()
                    || self.server.is_some()
                    || self.guardian_off_prompt.is_some()
                    || self.models.is_some()
                    || self.resume.is_some()
                    || self.rewind.is_some()
                    || self.config.is_some();
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
            || !self.questions.is_empty()
            || self.denied.is_some()
            || self.unavailable.is_some()
            || self.server.is_some()
            || self.guardian_off_prompt.is_some()
            || self.models.is_some()
            || self.resume.is_some()
            || self.rewind.is_some()
            || self.config.is_some()
            || self.providers.is_some()
            || self.agents.is_some()
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code != KeyCode::Esc {
            self.last_esc = None;
        }
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
        if let Some(prompt) = &mut self.guardian_off_prompt {
            return match prompt.handle_key(key) {
                Some(answer) => {
                    self.guardian_off_prompt = None;
                    Action::GuardianOff(answer)
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
        if let Some(picker) = &mut self.resume {
            return match picker.handle_key(key) {
                ResumeKey::None => Action::None,
                ResumeKey::Close => {
                    self.resume = None;
                    Action::None
                }
                ResumeKey::Pick(id) => {
                    self.resume = None;
                    Action::ResumeSession(id)
                }
            };
        }
        if let Some(picker) = &mut self.rewind {
            return match picker.handle_key(key) {
                RewindKey::None => Action::None,
                RewindKey::Close => {
                    self.rewind = None;
                    Action::None
                }
                RewindKey::Select(turn) => Action::SelectRewind(turn),
                RewindKey::Run {
                    plan,
                    what,
                    overwrite_changed,
                } => {
                    self.rewind = None;
                    Action::Rewind {
                        plan,
                        what,
                        overwrite_changed,
                    }
                }
            };
        }
        if let Some(panel) = &mut self.config {
            return match panel.handle_key(key) {
                ConfigKey::None => Action::None,
                ConfigKey::Close => {
                    self.config = None;
                    Action::None
                }
                ConfigKey::OpenPicker(slot) => {
                    self.config = None;
                    Action::OpenPicker(slot)
                }
                change @ (ConfigKey::Save { .. } | ConfigKey::Reset(_)) => Action::Config(change),
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
                    // Rows the screen wrote into scrollback while the panel was tall left the
                    // last messages out of sight; the transcript is written again in full.
                    if std::mem::take(&mut self.committed_under_overlay) {
                        self.view(self.viewing.clone());
                    }
                    Action::None
                }
                PanelKey::View(id) => {
                    self.agents = None;
                    if self.viewing.as_deref() != Some(id.as_str()) {
                        self.view(Some(id));
                    } else if std::mem::take(&mut self.committed_under_overlay) {
                        self.view(self.viewing.clone());
                    }
                    Action::None
                }
                PanelKey::Kill(id) => Action::KillAgent(id),
                PanelKey::KillJob(id) => Action::KillJob(id),
            };
        }
        if self.approval.is_none() && !self.questions.is_empty() {
            self.question_key(key);
            return Action::None;
        }
        if self.agent_list.focused && !control && !self.covered() {
            return self.list_key(key);
        }
        if control && key.code == KeyCode::Char('t') && !typing_reason {
            self.agent_list.focused = false;
            self.agents = Some(AgentsPanel::new());
            return Action::None;
        }
        if control && key.code == KeyCode::Char('o') && !typing_reason {
            self.todos.toggle();
            return Action::None;
        }
        if key.code == KeyCode::Esc && self.approval.is_none() && self.close_palette() {
            return Action::None;
        }
        // The main turn is out of sight in an agent's view, and Esc there must not stop what the
        // human cannot see.
        if key.code == KeyCode::Esc && self.running && !typing_reason && self.viewing.is_none() {
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
        if key.code == KeyCode::Esc && self.wakeup_pending && !self.running {
            return Action::CancelWakeup;
        }
        if key.code == KeyCode::Esc
            && !self.running
            && !self.shell_running
            && self.viewing.is_none()
            && self.input.is_empty()
            && !self.history.navigating()
        {
            // Only an Esc nothing above claimed counts, so closing a menu or cancelling a
            // wakeup never starts the double press.
            let now = std::time::Instant::now();
            let double = self
                .last_esc
                .take()
                .is_some_and(|before| now.duration_since(before) <= ESC_DOUBLE_PRESS);
            if double {
                return Action::OpenRewind;
            }
            self.last_esc = Some(now);
            return Action::None;
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

    /// Keys while the agent list below the status line has the keyboard.
    fn list_key(&mut self, key: KeyEvent) -> Action {
        match self.agent_list.handle_key(key, self.viewing.as_deref()) {
            ListKey::None => Action::None,
            ListKey::Blur { typed } => {
                if typed {
                    let action = self.message_key(key);
                    self.refresh_completion();
                    self.refresh_slash();
                    return action;
                }
                Action::None
            }
            ListKey::View(target) => {
                // Showing what is already shown would only erase the terminal's history.
                if target != self.viewing {
                    self.view(target);
                }
                Action::None
            }
            ListKey::Kill(id) => Action::KillAgent(id),
        }
    }

    /// Forgets the menu's dismissal and selection once the text they were made for is gone,
    /// so neither resurfaces when the same text is typed again.
    fn refresh_slash(&mut self) {
        let text = self.input.text();
        if self.slash_dismissed.as_deref() != Some(&text) {
            self.slash_dismissed = None;
        }
        let token = complete::token_at(&text, '/', self.input.cursor());
        let current = token.as_ref().map(|t| (t.start, t.query.as_str()));
        if current != Some((self.slash_selected.0, self.slash_selected.1.as_str())) {
            self.slash_selected = (0, String::new(), 0);
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

    /// The `/` menu, when the cursor is in a `/token` at the start of the input or after
    /// whitespace and nothing else wants the keys.
    pub fn slash_menu(&self) -> Option<SlashMenu<'_>> {
        if self.covered()
            || self.search.is_some()
            || self.completion.is_some()
            || self.history.navigating()
            || self.shell_mode()
        {
            return None;
        }
        let text = self.input.text();
        let token = complete::token_at(&text, '/', self.input.cursor())?;
        if self.slash_dismissed.as_deref() == Some(&text) {
            return None;
        }
        let matches = match_menu(&self.menu, &token.query);
        if matches.is_empty() {
            return None;
        }
        let selected =
            if self.slash_selected.0 == token.start && self.slash_selected.1 == token.query {
                self.slash_selected.2.min(matches.len() - 1)
            } else {
                0
            };
        let bare = token.start == 0 && token.end == text.chars().count();
        Some(SlashMenu {
            token,
            bare,
            matches,
            selected,
        })
    }

    /// Keys the `/` menu takes; `None` when the key is for the input.
    fn slash_key(&mut self, key: KeyEvent) -> Option<Action> {
        let menu = self.slash_menu()?;
        let (token, bare, count, selected) = (
            menu.token.clone(),
            menu.bare,
            menu.matches.len(),
            menu.selected,
        );
        let entry = menu.matches[selected];
        let name = entry.name.clone();
        let argument = entry.hint.is_some();
        let required = entry.requires_argument();
        let prefix = common_prefix(
            &menu
                .matches
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
        );
        let narrows = count > 1
            && prefix.chars().count() > token.query.chars().count()
            && prefix
                .to_lowercase()
                .starts_with(&token.query.to_lowercase());
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up if plain => {
                self.slash_selected = (token.start, token.query, selected.saturating_sub(1));
            }
            KeyCode::Down if plain => {
                self.slash_selected = (token.start, token.query, (selected + 1).min(count - 1));
            }
            KeyCode::Tab if plain && narrows => {
                self.input
                    .replace_range(token.start, token.end, &format!("/{prefix}"));
                self.history.reset();
            }
            KeyCode::Tab if plain => self.accept_slash(&token, bare, &name, argument),
            KeyCode::Enter if plain && bare => {
                if required {
                    self.accept_slash(&token, bare, &name, argument);
                } else {
                    self.accept_slash(&token, bare, &name, false);
                    return Some(self.enter(false));
                }
            }
            _ => return None,
        }
        Some(Action::None)
    }

    /// Writes the chosen name over the typed one: the whole input when the token is bare, else
    /// only the token, with a space after it unless one is already there.
    fn accept_slash(&mut self, token: &Token, bare: bool, name: &str, argument: bool) {
        if bare {
            let space = if argument { " " } else { "" };
            self.input.set_text(&format!("/{name}{space}"));
        } else {
            let followed_by_space = self
                .input
                .text()
                .chars()
                .nth(token.end)
                .is_some_and(char::is_whitespace);
            let space = if followed_by_space { "" } else { " " };
            self.input
                .replace_range(token.start, token.end, &format!("/{name}{space}"));
        }
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

    fn question_key(&mut self, key: KeyEvent) {
        let Some(panel) = self.questions.front_mut() else {
            return;
        };
        let Some(reply) = panel.handle_key(key) else {
            return;
        };
        let declined = reply == QuestionReply::Declined;
        panel.reply(reply);
        self.questions.pop_front();
        if declined {
            self.transcript.notice("question declined".to_string());
        }
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
            KeyCode::Down
                if self.on_last_line()
                    && self.agent_list.has_rows()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.agent_list.focus();
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
        // What is queued for the main conversation is out of sight in an agent's view.
        if self.viewing.is_none() && !self.queued.is_empty() && !self.shell_running {
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
        if let Some(id) = self.viewing.clone() {
            return self.enter_in_view(id, &text);
        }
        if self.shell_running {
            self.transcript
                .notice("a shell command is running; wait for it or press Esc to interrupt");
            return Action::None;
        }
        if let Some(rest) = text.strip_prefix('!') {
            let command = rest.trim();
            if command.is_empty() {
                return Action::None;
            }
            self.input.take();
            if self.running {
                return Action::Queue(Queued::Shell(text.trim().to_string()));
            }
            return Action::Shell(command.to_string());
        }
        if self.running {
            return self.queue_during_turn(text.trim());
        }
        let text = self.input.take();
        Action::Submit(text.trim().to_string())
    }

    /// Enter in an agent's view: plain text goes to that agent. A built-in command runs against
    /// the main conversation as usual, unless it would change that conversation, which the human
    /// cannot see from here; skills and `!` lines are refused too, and the text stays in the
    /// input.
    fn enter_in_view(&mut self, id: String, text: &str) -> Action {
        let text = text.trim();
        if text.starts_with('!') {
            self.shown_mut().notice(MAIN_ONLY);
            return Action::None;
        }
        match parse_command(text) {
            None => {
                self.input.take();
                Action::MessageAgent {
                    id,
                    text: text.to_string(),
                }
            }
            Some(command) if command.main_only() => {
                self.shown_mut().notice(MAIN_ONLY);
                Action::None
            }
            Some(Command::Other { name, .. })
                if self.menu.iter().any(|entry| entry.name == name) =>
            {
                self.shown_mut().notice(MAIN_ONLY);
                Action::None
            }
            Some(_) => {
                if self.running {
                    return self.queue_during_turn(text);
                }
                if self.shell_running {
                    self.shown_mut().notice(
                        "a shell command is running; wait for it or press Esc to interrupt",
                    );
                    return Action::None;
                }
                let text = self.input.take();
                Action::Submit(text.trim().to_string())
            }
        }
    }

    /// What Enter does on input typed while a turn runs: a known skill or any built-in but
    /// `/loop` queues as typed; the synchronous UI commands run at once; unknown slash names
    /// and every `/loop` form keep the existing refusal.
    fn queue_during_turn(&mut self, text: &str) -> Action {
        match parse_command(text) {
            Some(command) if command.runs_mid_turn() => {
                self.input.take();
                Action::Command {
                    command,
                    raw: text.to_string(),
                }
            }
            Some(Command::Other { name, argument })
                if self.menu.iter().any(|entry| entry.name == name) =>
            {
                self.input.take();
                Action::Queue(Queued::Skill {
                    raw: text.to_string(),
                    name,
                    argument,
                })
            }
            Some(Command::Loop(_)) | Some(Command::Other { .. }) => {
                self.transcript
                    .notice("a turn is running; wait for it or press Esc to interrupt");
                Action::None
            }
            Some(_) => {
                self.input.take();
                Action::Queue(Queued::Command(text.to_string()))
            }
            None => {
                self.input.take();
                Action::Queue(Queued::Message(text.to_string()))
            }
        }
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

    fn running_row(id: &str) -> ListRow {
        ListRow {
            id: Some(id.into()),
            depth: 1,
            agent_type: "worker".into(),
            status: Some(fontaine_agents::runner::ChildStatus::Running),
            elapsed: std::time::Duration::from_secs(3),
            spend: "10 tok".into(),
        }
    }

    fn with_agent_list() -> App {
        let mut app = app();
        let main = ListRow {
            id: None,
            depth: 0,
            agent_type: String::new(),
            status: None,
            elapsed: std::time::Duration::ZERO,
            spend: String::new(),
        };
        app.agent_list.set_rows(vec![main, running_row("agent-1")]);
        app
    }

    #[test]
    fn down_on_the_last_line_focuses_the_agent_list_and_enter_views_the_agent() {
        let mut app = with_agent_list();
        press(&mut app, KeyCode::Down);
        assert!(app.agent_list.focused);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.viewing.as_deref(), Some("agent-1"));
        assert!(app.reprint);
        assert!(!app.agent_list.focused);
        // Back on main.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.viewing, None);
    }

    #[test]
    fn down_does_not_focus_the_list_from_a_line_that_is_not_the_last() {
        let mut app = with_agent_list();
        type_text(&mut app, "a");
        app.handle_input(key(KeyCode::Enter, KeyModifiers::ALT));
        type_text(&mut app, "b");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Down);
        assert!(!app.agent_list.focused);
        // Nor while the list is empty.
        let mut plain = self::app();
        press(&mut plain, KeyCode::Down);
        assert!(!plain.agent_list.focused);
    }

    #[test]
    fn down_while_walking_history_goes_to_newer_entries_not_the_list() {
        let mut app = with_agent_list();
        app.history.push("older");
        press(&mut app, KeyCode::Up);
        assert!(app.history.navigating());
        press(&mut app, KeyCode::Down);
        assert!(!app.agent_list.focused);
    }

    #[test]
    fn a_printable_key_with_the_list_focused_is_typed_in_the_prompt() {
        let mut app = with_agent_list();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('h'));
        assert!(!app.agent_list.focused);
        assert_eq!(app.input.text(), "h");
    }

    #[test]
    fn x_on_a_running_row_stops_it() {
        let mut app = with_agent_list();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            Action::KillAgent("agent-1".into())
        );
    }

    fn viewing_app() -> App {
        let mut app = with_agent_list();
        app.view(Some("agent-1".into()));
        app.menu.push(MenuEntry {
            name: "review".into(),
            description: String::new(),
            hint: None,
            aliases: Vec::new(),
        });
        app
    }

    fn submit(app: &mut App, text: &str) -> Action {
        type_text(app, text);
        press(app, KeyCode::Enter)
    }

    #[test]
    fn text_typed_in_an_agents_view_goes_to_that_agent() {
        let mut app = viewing_app();
        assert_eq!(
            submit(&mut app, "also count the lines"),
            Action::MessageAgent {
                id: "agent-1".into(),
                text: "also count the lines".into()
            }
        );
        assert!(app.input.is_empty());
        // Even while the main turn runs, which the human cannot see.
        app.running = true;
        assert_eq!(
            submit(&mut app, "and the words"),
            Action::MessageAgent {
                id: "agent-1".into(),
                text: "and the words".into()
            }
        );
    }

    #[test]
    fn commands_that_change_the_main_conversation_are_refused_and_the_text_stays() {
        for line in [
            "/clear",
            "/compact",
            "/rewind",
            "/branch",
            "/resume",
            "/model",
            "/model a/b",
            "/output-style concise",
            "/goal the build is green",
            "/loop 5m check",
            "/init",
            "/plan add a test",
            "/review",
            "!ls",
        ] {
            let mut app = viewing_app();
            assert_eq!(submit(&mut app, line), Action::None, "{line}");
            assert_eq!(app.input.text(), line, "{line}");
            let shown: String = app
                .shown()
                .lines(80, &app.theme)
                .iter()
                .map(ToString::to_string)
                .collect();
            assert!(shown.contains(MAIN_ONLY), "{line}: {shown}");
            assert!(app.transcript.entries.is_empty(), "{line}");
        }
    }

    #[test]
    fn ui_commands_still_run_in_a_view() {
        let mut app = viewing_app();
        assert_eq!(submit(&mut app, "/plan"), Action::Submit("/plan".into()),);
        assert_eq!(
            submit(&mut app, "/status"),
            Action::Submit("/status".into())
        );
        app.running = true;
        assert!(matches!(
            submit(&mut app, "/status"),
            Action::Command { .. }
        ));
    }

    #[test]
    fn escape_in_a_view_never_interrupts_the_main_turn_and_never_opens_rewind() {
        let mut app = viewing_app();
        app.running = true;
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        app.running = false;
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        // On main the same presses do both.
        app.view(None);
        app.running = true;
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);
        app.running = false;
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::OpenRewind);
    }

    #[test]
    fn ctrl_c_still_interrupts_the_main_turn_from_a_view() {
        let mut app = viewing_app();
        app.running = true;
        assert_eq!(
            app.handle_input(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Interrupt
        );
    }

    #[test]
    fn the_panel_views_an_agent_and_closing_it_after_a_commit_reprints() {
        let mut app = app();
        let mut panel = AgentsPanel::new();
        panel.set_rows(vec![crate::agents_panel::PanelRow {
            id: "agent-1".into(),
            agent_type: "worker".into(),
            model: "m".into(),
            status: fontaine_agents::runner::ChildStatus::Done,
            tokens: 1,
            token_budget: None,
            cost_usd: None,
            cost_budget: None,
            depth: 1,
            stopped_because: None,
        }]);
        app.agents = Some(panel);
        press(&mut app, KeyCode::Esc);
        assert!(
            !app.reprint,
            "closing a panel that committed nothing keeps the screen"
        );
        app.agents = Some(AgentsPanel::new());
        app.committed_under_overlay = true;
        press(&mut app, KeyCode::Esc);
        assert!(app.reprint);
        assert!(!app.committed_under_overlay);
        assert_eq!(app.viewing, None);
    }

    #[test]
    fn what_a_command_wrote_while_an_agent_is_shown_moves_to_that_agents_view() {
        let mut app = viewing_app();
        let mark = app.notice_mark();
        app.transcript.notice("plan mode on");
        app.move_notices_to_shown(mark);
        assert!(app.transcript.entries.is_empty());
        let shown: String = app
            .shown()
            .lines(80, &app.theme)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(shown.contains("plan mode on"), "{shown}");
        // On main nothing moves.
        app.view(None);
        let mark = app.notice_mark();
        app.transcript.notice("kept");
        app.move_notices_to_shown(mark);
        assert_eq!(app.transcript.entries.len(), 1);
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
            Some(crate::transcript::Entry::Notice { headline, .. }) => Some(headline),
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
            preview: None,
        };
        Approval::new(request, reply)
    }

    fn ask(multi: bool) -> Question {
        use fontaine_tools::ask_user::QuestionOption;
        let option = |label: &str| QuestionOption {
            label: label.into(),
            description: "d".into(),
        };
        Question {
            question: "Which?".into(),
            header: "Pick".into(),
            options: vec![option("a"), option("b")],
            multi_select: multi,
        }
    }

    /// A question dialog past its grace period, and the receiver of its reply.
    fn open_question(app: &mut App, agent: &str, multi: bool) -> oneshot::Receiver<QuestionReply> {
        let (tx, rx) = oneshot::channel();
        app.begin_question(agent.into(), vec![ask(multi)], tx);
        app.questions.back_mut().unwrap().settle();
        rx
    }

    #[test]
    fn a_second_question_waits_behind_the_first_and_neither_is_dropped() {
        let mut app = app();
        let mut first = open_question(&mut app, "root", false);
        let mut second = open_question(&mut app, "root", false);
        assert_eq!(app.questions.len(), 2);
        assert!(first.try_recv().is_err());
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            first.try_recv(),
            Ok(QuestionReply::Answered(answers)) if answers[0].selected == ["a"]
        ));
        assert!(second.try_recv().is_err());
        assert_eq!(app.questions.len(), 1);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            second.try_recv(),
            Ok(QuestionReply::Answered(answers)) if answers[0].selected == ["b"]
        ));
        assert!(app.questions.is_empty());
    }

    #[test]
    fn esc_with_a_question_open_while_running_declines_instead_of_interrupting() {
        let mut app = app();
        app.running = true;
        let mut rx = open_question(&mut app, "root", false);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert_eq!(rx.try_recv().unwrap(), QuestionReply::Declined);
        assert!(app.questions.is_empty());
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);
    }

    #[test]
    fn esc_while_typing_other_only_leaves_the_field() {
        let mut app = app();
        app.running = true;
        let mut rx = open_question(&mut app, "root", false);
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Enter);
        assert!(app.questions[0].is_typing());
        type_text(&mut app, "mine");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.questions[0].is_typing());
        assert!(rx.try_recv().is_err());
        press(&mut app, KeyCode::Esc);
        assert_eq!(rx.try_recv().unwrap(), QuestionReply::Declined);
    }

    #[test]
    fn an_interrupt_declines_the_root_questions_and_keeps_a_background_one() {
        let mut app = app();
        let mut root = open_question(&mut app, "root", false);
        let mut other = open_question(&mut app, "worker", false);
        app.deny_pending();
        assert_eq!(root.try_recv().unwrap(), QuestionReply::Declined);
        assert!(other.try_recv().is_err());
        assert_eq!(app.questions.len(), 1);
    }

    #[test]
    fn an_open_approval_takes_the_keys_before_a_waiting_question() {
        let mut app = app();
        let mut rx = open_question(&mut app, "root", false);
        app.approval = Some(approval());
        let before = app.approval.as_ref().unwrap().selected();
        press(&mut app, KeyCode::Down);
        assert_ne!(app.approval.as_ref().unwrap().selected(), before);
        std::thread::sleep(crate::server_prompt::ANSWER_GRACE);
        press(&mut app, KeyCode::Enter);
        assert!(rx.try_recv().is_err(), "the question is still waiting");
        assert_eq!(app.questions.len(), 1);
        let overlay = crate::view::overlay(&app, 60, 30, false);
        assert_eq!(overlay.title, "approval");
        app.approval = None;
        assert_eq!(crate::view::overlay(&app, 60, 30, false).title, "question");
    }

    #[test]
    fn a_closed_question_is_pruned() {
        let mut app = app();
        let rx = open_question(&mut app, "root", false);
        drop(rx);
        app.prune_closed();
        assert!(app.questions.is_empty());
    }

    #[test]
    fn a_short_terminal_keeps_the_selected_option_in_view() {
        let mut app = app();
        let _rx = open_question(&mut app, "root", false);
        press(&mut app, KeyCode::Down);
        let overlay = crate::view::overlay(&app, 40, 14, false);
        let shown: Vec<String> = overlay
            .body
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(shown.iter().any(|row| row.starts_with("2. b")), "{shown:?}");
    }

    #[test]
    fn a_message_during_a_turn_is_queued_and_an_unknown_command_is_refused() {
        let mut app = app();
        app.running = true;
        app.menu.push(MenuEntry {
            name: "review".into(),
            hint: Some("[pr-number]".into()),
            description: "Review a pull request".into(),
            aliases: Vec::new(),
        });

        type_text(&mut app, "also this");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Queue(Queued::Message("also this".into()))
        );
        assert!(app.input.is_empty());

        type_text(&mut app, "/review 42");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Queue(Queued::Skill {
                raw: "/review 42".into(),
                name: "review".into(),
                argument: "42".into(),
            })
        );
        assert!(app.input.is_empty());

        type_text(&mut app, "/rename later");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Queue(Queued::Command("/rename later".into()))
        );
        assert!(app.input.is_empty());

        type_text(&mut app, "!echo hi");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Queue(Queued::Shell("!echo hi".into()))
        );
        assert!(app.input.is_empty());

        // A bare `!` is ignored either way.
        type_text(&mut app, "!");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.input.text(), "!");
        app.input.take();

        // The immediate UI set runs at once.
        type_text(&mut app, "/todos");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Command {
                command: Command::Todos,
                raw: "/todos".into(),
            }
        );
        assert!(app.input.is_empty());

        for refused in ["/nope with args", "/loop watch CI"] {
            type_text(&mut app, refused);
            assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
            assert_eq!(
                last_notice(&app),
                Some("a turn is running; wait for it or press Esc to interrupt")
            );
            assert_eq!(app.input.text(), refused);
            app.input.take();
        }
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
        for open in ["approval", "denied", "unavailable", "question"] {
            let mut app = app_with_files();
            type_text(&mut app, "@ma");
            assert!(app.completion.is_some());
            match open {
                "approval" => app.approval = Some(approval()),
                "denied" => app.denied = Some(DeniedList::new(Vec::new())),
                "question" => {
                    app.begin_question("root".into(), vec![ask(false)], oneshot::channel().0);
                }
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
    fn the_slash_menu_opens_for_a_name_being_typed() {
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
    }

    fn with_zork(app: &mut App) {
        for name in ["zork-alpha", "zork-alphabet", "zork-beta"] {
            app.menu.push(MenuEntry {
                name: name.into(),
                hint: None,
                description: "d".into(),
                aliases: Vec::new(),
            });
        }
    }

    #[test]
    fn the_menu_opens_for_a_slash_token_after_whitespace() {
        let mut app = app();
        type_text(&mut app, "hello /mod");
        assert_eq!(menu_names(&app), ["model", "guardian-model"]);
        assert!(!app.slash_menu().unwrap().bare);
        app.input.set_text("a/mod");
        assert!(app.slash_menu().is_none());
        app.input.set_text("hello /mod x");
        assert!(app.slash_menu().is_none());
        app.input.set_text("a\n/add");
        assert!(app.slash_menu().is_some());
    }

    #[test]
    fn the_menu_stays_shut_in_shell_mode() {
        let mut app = app();
        type_text(&mut app, "!ls /u");
        assert!(app.slash_menu().is_none());
        app.input.set_text("!cd /p");
        assert!(app.slash_menu().is_none());
    }

    #[test]
    fn inline_tab_replaces_only_the_token() {
        let mut app = app();
        type_text(&mut app, "please run /add");
        assert_eq!(press(&mut app, KeyCode::Tab), Action::None);
        assert_eq!(app.input.text(), "please run /add-dir ");
        assert!(app.slash_menu().is_none());

        app.input.set_text("run /cle now");
        for _ in 0.." now".len() {
            press(&mut app, KeyCode::Left);
        }
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "run /clear now");
        assert!(app.slash_menu().is_some());
    }

    #[test]
    fn tab_extends_to_the_common_prefix_before_accepting() {
        let mut app = app();
        with_zork(&mut app);
        type_text(&mut app, "/zork-al");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/zork-alpha");
        assert_eq!(menu_names(&app), ["zork-alpha", "zork-alphabet"]);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/zork-alpha");
        assert_eq!(selected_name(&app), "zork-alpha");

        app.input.set_text("/zork-");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/zork-alpha");

        app.input.set_text("go /zork-al");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "go /zork-alpha");
    }

    #[test]
    fn tab_through_a_contains_match_does_not_narrow() {
        let mut app = app();
        type_text(&mut app, "/mod");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input.text(), "/model ");
    }

    #[test]
    fn inline_enter_submits_the_text_as_typed() {
        let mut app = app();
        type_text(&mut app, "go /mod");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Submit("go /mod".into())
        );
    }

    #[test]
    fn the_selection_is_keyed_on_the_token() {
        let mut app = app();
        type_text(&mut app, "/mod /mod");
        press(&mut app, KeyCode::Down);
        assert_eq!(selected_name(&app), "guardian-model");
        for _ in 0..5 {
            press(&mut app, KeyCode::Left);
        }
        assert_eq!(selected_name(&app), "model");
    }

    #[test]
    fn esc_dismisses_an_inline_menu_until_the_text_changes() {
        let mut app = app();
        type_text(&mut app, "go /mod");
        press(&mut app, KeyCode::Esc);
        assert!(app.slash_menu().is_none());
        type_text(&mut app, "e");
        assert!(app.slash_menu().is_some());
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
    fn ctrl_o_toggles_the_task_list_unless_the_agents_panel_has_the_keys() {
        let mut app = app();
        assert_eq!(ctrl(&mut app, 'o'), Action::None);
        assert!(app.todos.expanded);
        app.agents = Some(AgentsPanel::new());
        ctrl(&mut app, 'o');
        assert!(app.todos.expanded);
        app.agents = None;
        ctrl(&mut app, 'o');
        assert!(!app.todos.expanded);
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
            |a| a.begin_question("root".into(), vec![ask(false)], oneshot::channel().0),
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

        let mut app = app_with_files();
        type_text(&mut app, "hi /mod");
        ctrl(&mut app, 'r');
        assert!(app.slash_menu().is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.slash_menu().is_some());
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

    #[test]
    fn two_escapes_at_an_idle_empty_prompt_open_the_rewind_picker() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::OpenRewind);
        // The press is spent: a third starts over.
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    }

    #[test]
    fn a_slow_second_escape_or_a_key_between_does_not_open_it() {
        let mut app = app();
        press(&mut app, KeyCode::Esc);
        app.last_esc = Some(std::time::Instant::now() - ESC_DOUBLE_PRESS * 2);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Left);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    }

    #[test]
    fn escapes_other_meanings_win_and_do_not_start_the_double_press() {
        // Typed text: Esc has nothing to cancel, but the prompt is not empty.
        let mut app = app();
        type_text(&mut app, "draft");
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);

        // A running turn: Esc interrupts, twice.
        let mut app = self::app();
        app.running = true;
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::Interrupt);

        // A pending loop wakeup: Esc cancels it, and the second Esc is a first press.
        let mut app = self::app();
        app.wakeup_pending = true;
        assert_eq!(press(&mut app, KeyCode::Esc), Action::CancelWakeup);
        app.wakeup_pending = false;
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::OpenRewind);

        // The slash menu closes on Esc without arming.
        let mut app = self::app();
        type_text(&mut app, "/");
        assert!(app.slash_menu().is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.slash_menu().is_none());
        app.input.set_text("");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    }

    #[test]
    fn the_rewind_picker_takes_the_keyboard_and_a_confirmed_rewind_closes_it() {
        use fontaine_core::checkpoint::{RewindPlan, TurnSummary};
        let mut app = app();
        app.rewind = Some(RewindPicker::new(vec![TurnSummary {
            turn: 1,
            prompt: "first".into(),
            files: 0,
        }]));
        assert!(app.covered());
        type_text(&mut app, "ignored");
        assert!(app.input.is_empty());
        assert_eq!(press(&mut app, KeyCode::Enter), Action::SelectRewind(1));
        app.rewind.as_mut().unwrap().offer(RewindPlan {
            turn: 1,
            prompt: "first".into(),
            conv_n: 0,
            user_messages_before: 0,
            files: Vec::new(),
        });
        press(&mut app, KeyCode::Down);
        let action = press(&mut app, KeyCode::Enter);
        assert!(matches!(
            action,
            Action::Rewind {
                what: RewindWhat::Conversation,
                ..
            }
        ));
        assert!(app.rewind.is_none());
        app.restore_to_input("first");
        assert_eq!(app.input.text(), "first");
    }
}
