//! The interactive terminal UI. [`run`] takes over the terminal; [`drive`] is the same session
//! loop with the terminal swapped for an input channel and a render callback, which is how the
//! tests exercise it.

pub mod accessibility;
pub mod agent_list;
pub mod agents_panel;
pub mod app;
pub mod approval;
pub mod bug_prompt;
pub mod clipboard;
pub mod commands;
pub mod complete;
pub mod config_panel;
pub mod confirm_prompt;
pub mod denials;
pub mod diff;
pub mod elicitation;
pub mod guardian_prompt;
pub mod history;
mod hyperlink;
pub mod input;
pub mod keymap;
pub mod linear;
mod markdown;
pub mod model_picker;
mod notify;
mod paste_burst;
pub mod picker;
pub mod plugin_prompt;
pub mod provider_dialog;
pub mod question;
pub mod recent;
pub mod reports;
pub mod resume_picker;
pub mod rewind;
pub mod screen;
pub mod server_prompt;
pub mod setup;
pub mod setup_prompt;
pub mod shutdown;
pub mod skill_trust_prompt;
pub mod spinner;
pub mod statusbar;
pub mod theme;
pub mod todo_panel;
pub mod transcript;
pub mod transcript_view;
pub mod trust_prompt;
mod ui;
pub mod url_elicitation;
pub mod usage_prompt;
pub mod view;

use crate::config_panel::ConfigKey;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fontaine_core::agent::{AgentRuntime, AgentSetup, EnvLookup};
use fontaine_core::config::PermissionMode;
use fontaine_core::gate::AgentViewEvent;
use fontaine_core::notice::{Notice, Severity};
use fontaine_core::permissions::{
    Answer, ApprovalRequest, Approver, GuardianUnavailable, PauseChoice, UnavailableChoice,
};
use fontaine_core::statusline::{StatuslineScript, StatuslineState};
use fontaine_core::usage_pause::PauseInfo;
use fontaine_tools::ask_user::{Question, QuestionReply};
use fontaine_tools::kills::KILL_SETTLE;
use fontaine_tools::mcp::{ElicitReply, ElicitRequest, ElicitUrl};
use futures::future::BoxFuture;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use ratatui::crossterm::terminal::{EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

pub use app::{Action, App, Input};
pub use commands::{Command, MenuEntry, builtin_menu, parse_command};
pub use denials::UnavailableAnswer;
pub use shutdown::{DriveOptions, ExitWatchdog, FINAL_SHUTDOWN_BUDGET};

use model_picker::PickerPurpose;
use provider_dialog::DialogKey;
use recent::RecentModels;
use reports::McpState;
use screen::{Anchored, FrameWriter, Screen, Tty};
use theme::Theme;

/// The session a `/resume` or `/branch` asks the caller to start in place of this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTarget {
    /// The id of the session to open, which already exists on disk.
    pub source: String,
    /// Whether opening it appends a `Resumed` record. A copy made by `/branch` has its own.
    pub record_resume: bool,
}

/// What [`run`] hands the binary: how the session ended, and the watchdog still armed when it
/// ended for good, which the caller keeps alive past its own teardown and disarms last.
pub struct RunOutcome {
    pub outcome: DriveOutcome,
    pub watchdog: Option<ExitWatchdog>,
}

/// How [`drive`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveOutcome {
    Exit,
    Switch(SessionTarget),
}

/// What the `fontaine` binary knows about its own build; `env!` values exist only there.
#[derive(Clone, Debug)]
pub struct BuildInfo {
    pub version: &'static str,
    pub git_sha: &'static str,
    pub target: &'static str,
}

pub(crate) static BUILD_INFO: std::sync::OnceLock<BuildInfo> = std::sync::OnceLock::new();

/// The version `--version` shows; the package version when no build was recorded (tests).
pub fn build_version() -> &'static str {
    version_or_package(BUILD_INFO.get())
}

fn version_or_package(info: Option<&BuildInfo>) -> &'static str {
    info.map_or(env!("CARGO_PKG_VERSION"), |b| b.version)
}

/// Records the build for `/bug`; the first call wins.
pub fn set_build_info(info: BuildInfo) {
    let _ = BUILD_INFO.set(info);
}

/// What the permission pipeline sends to the interface.
enum Prompt {
    Ask(Box<ApprovalRequest>, oneshot::Sender<Answer>),
    /// The pinned guardian cannot be used and the human has to decide what to do.
    Unavailable(Box<GuardianUnavailable>, oneshot::Sender<UnavailableChoice>),
    /// The run is held for a spent usage window; the human may resume, cancel or switch account.
    UsagePaused(Box<PauseInfo>, oneshot::Sender<PauseChoice>),
    /// The model's questions for the human, with the agent that asks. A dropped sender means
    /// nobody could answer.
    Question(String, Vec<Question>, oneshot::Sender<QuestionReply>),
    /// An MCP server's request for input. A dropped sender means nobody could answer.
    Elicit(Box<ElicitRequest>, oneshot::Sender<ElicitReply>),
    Notice(Severity, String),
}

/// Notices produced outside the session, such as the plugin auto-update pass's outcomes. The
/// receiver is shared so a `/resume` or `/branch`, which ends one `drive` and starts another in
/// the same process, hands whatever was not yet shown to the next session.
pub type NoticeFeed = Arc<tokio::sync::Mutex<UnboundedReceiver<Notice>>>;

/// What the caller of [`run`] or [`drive`] shows besides the session's own notices.
#[derive(Default)]
pub struct Notices {
    /// Shown before every other notice.
    pub startup: Vec<Notice>,
    /// Shown as they arrive while the session runs.
    pub feed: Option<NoticeFeed>,
}

/// A sender for [`NoticeFeed`] and the feed that receives from it.
pub fn notice_feed() -> (UnboundedSender<Notice>, NoticeFeed) {
    let (tx, rx) = unbounded_channel();
    (tx, Arc::new(tokio::sync::Mutex::new(rx)))
}

/// Hands approval requests to the interface and waits for the human's answer. With the
/// interface gone, nobody can approve anything.
struct ChannelApprover {
    tx: UnboundedSender<Prompt>,
}

impl Approver for ChannelApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async move {
            let deny = Answer::Deny { feedback: None };
            let (reply, answer) = oneshot::channel();
            if self.tx.send(Prompt::Ask(Box::new(request), reply)).is_err() {
                return deny;
            }
            answer.await.unwrap_or(deny)
        })
    }

    fn ask_user<'a>(
        &'a self,
        agent: &'a str,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, QuestionReply> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            if self
                .tx
                .send(Prompt::Question(agent.to_string(), questions, reply))
                .is_err()
            {
                return QuestionReply::Unavailable;
            }
            answer.await.unwrap_or(QuestionReply::Unavailable)
        })
    }

    fn elicit<'a>(&'a self, form: fontaine_tools::mcp::ElicitForm) -> BoxFuture<'a, ElicitReply> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            let request = ElicitRequest::Form(form);
            if self
                .tx
                .send(Prompt::Elicit(Box::new(request), reply))
                .is_err()
            {
                return ElicitReply::Cancel;
            }
            answer.await.unwrap_or(ElicitReply::Cancel)
        })
    }

    fn elicit_url<'a>(&'a self, request: ElicitUrl) -> BoxFuture<'a, ElicitReply> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            let request = ElicitRequest::Url(request);
            if self
                .tx
                .send(Prompt::Elicit(Box::new(request), reply))
                .is_err()
            {
                return ElicitReply::Cancel;
            }
            answer.await.unwrap_or(ElicitReply::Cancel)
        })
    }

    fn notice(&self, message: &str) {
        let _ = self
            .tx
            .send(Prompt::Notice(Severity::Info, message.to_string()));
    }

    fn warn(&self, message: &str) {
        // The channel carries it to the UI, which keeps the ring buffer the bug report reads.
        let _ = self
            .tx
            .send(Prompt::Notice(Severity::Warning, message.to_string()));
    }

    fn guardian_unavailable<'a>(
        &'a self,
        info: GuardianUnavailable,
    ) -> BoxFuture<'a, UnavailableChoice> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            if self
                .tx
                .send(Prompt::Unavailable(Box::new(info), reply))
                .is_err()
            {
                return UnavailableChoice::Deny;
            }
            answer.await.unwrap_or(UnavailableChoice::Deny)
        })
    }

    fn usage_paused<'a>(&'a self, info: PauseInfo) -> BoxFuture<'a, PauseChoice> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            if self
                .tx
                .send(Prompt::UsagePaused(Box::new(info), reply))
                .is_err()
            {
                return PauseChoice::Wait;
            }
            // A prompt that was dropped unanswered leaves the pause to run its course.
            answer.await.unwrap_or(PauseChoice::Wait)
        })
    }
}

type ClipboardSink = Box<dyn FnMut(&str) -> std::io::Result<()>>;

/// Hands the terminal to a child process for the duration of the closure: the interface steps
/// aside, the child owns the terminal, and the interface is redrawn afterwards.
pub type Suspend = Box<dyn FnMut(&mut dyn FnMut())>;

/// A [`Suspend`] for a surface with no terminal to hand over: it just runs the closure.
pub fn no_suspend() -> Suspend {
    Box::new(|work| work())
}

/// Lets the terminal reader step aside, so a child process that owns the terminal gets every
/// key.
#[derive(Default)]
pub(crate) struct ReaderPause {
    requested: AtomicBool,
    parked: AtomicBool,
}

impl ReaderPause {
    /// Asks the reader to stop reading and waits until it has.
    fn pause(&self) {
        self.requested.store(true, Ordering::SeqCst);
        for _ in 0..100 {
            if self.parked.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn resume(&self) {
        self.requested.store(false, Ordering::SeqCst);
    }
}

struct Ui<R> {
    app: App,
    /// Sees each prompt that opens, for the `notification` hook, whatever `[tui] notify` is.
    hook_notifier: notify::Notifier,
    /// Sees each AFK temperature stop, so a stop that lands while a turn runs, or while the
    /// session is idle with a subagent working, is acted on at once.
    afk_stops: tokio::sync::watch::Receiver<u64>,
    input: UnboundedReceiver<Input>,
    prompts: UnboundedReceiver<Prompt>,
    /// What every subagent run does, for the agents' own transcripts.
    agent_events: UnboundedReceiver<(String, AgentViewEvent)>,
    render: R,
    /// Where the spinner's animation clock starts.
    spinner_epoch: std::time::Instant,
    recent: RecentModels,
    /// A model picked while a turn ran, applied once the turn is over: only the idle loop owns
    /// the runtime mutably.
    pending_pick: Option<(PickerPurpose, String)>,
    /// A model the human picked while the main model's usage window was spent; the turn applies
    /// it before it tries the request again.
    pending_usage_switch: Option<String>,
    /// The usage pause's total when the turn on screen started, so its clock leaves the waits out.
    paused_base: Duration,
    paused_base_for: Option<Instant>,
    /// A session picked in `/resume` while a turn ran, switched to once it is over.
    pending_resume: Option<String>,
    /// The AFK stretches the human left whose independent review has not run yet, by start
    /// position: a mode switch is immediate, the review waits for the end of the work in progress.
    pending_afk_review: Vec<u64>,
    /// Settings saved from `/config` that apply from the next launch, by key, as the label the
    /// panel shows; they outlive the panel so reopening it still shows them.
    config_pending: std::collections::BTreeMap<String, String>,
    /// Panel changes and picker requests made while a turn ran, carried out once it is over.
    pending_config: VecDeque<ConfigKey>,
    /// Requests the provider dialog made while a turn ran, carried out once it is over.
    pending_provider: VecDeque<DialogKey>,
    data_dir: PathBuf,
    pending_drafts: fontaine_core::bug_drafts::Pending,
    user_config: Option<PathBuf>,
    env: EnvLookup,
    /// What starting each MCP server came to, for `/mcp`.
    mcp_status: Vec<(String, McpState)>,
    /// The MCP listing revision the command menu's prompt rows were built at: a move
    /// re-merges them, so the draw loop does not rebuild the rows on every frame.
    prompts_revision: Option<u64>,
    /// Where `/copy` sends text; the write's result comes back to the command.
    clipboard: ClipboardSink,
    /// Runs the editor `/memory` opens with the terminal handed over.
    suspend: Suspend,
    launcher: ui::Launcher,
    /// Set by `/resume` and `/branch`: the session the caller starts once this one has ended.
    switch: Option<SessionTarget>,
    /// One poller per role, main first: the two accounts can differ.
    quota: [fontaine_core::quota::QuotaPoller; 2],
    quota_rx: [tokio::sync::watch::Receiver<fontaine_core::quota::QuotaSnapshot>; 2],
    /// Accounts whose quota failure was already reported, so a poller re-pointed by a credential
    /// rebuild does not repeat it.
    quota_warned: std::collections::HashSet<String>,
    /// How long the final exit may take, from the first quit request.
    quit_budget: Duration,
    /// Called once with the deadline when the final exit starts.
    on_quit: Option<Box<dyn FnMut(Instant)>>,
    /// When the first quit request came. Latched: it never clears, and it decides every
    /// "does anything else start" question after it.
    quit_started: Option<Instant>,
    /// The task running the status-line command, which a quit ends instead of leaving to the
    /// runtime's teardown.
    statusline_task: Option<tokio::task::JoinHandle<()>>,
    /// Sends a request to run the configured status-line command. Drained in the run loop.
    statusline_tx: Option<tokio::sync::mpsc::UnboundedSender<StatuslineTrigger>>,
    statusline_rx: Option<tokio::sync::mpsc::UnboundedReceiver<StatuslineTrigger>>,
    /// Receives the result of a status-line command run.
    statusline_result_tx: tokio::sync::mpsc::UnboundedSender<Option<StatuslineState>>,
    statusline_result_rx: tokio::sync::mpsc::UnboundedReceiver<Option<StatuslineState>>,
    /// The configured status-line script, when `[statusline] command` is set.
    statusline_script: Option<StatuslineScript>,
    /// Whether a status-line run is in flight.
    statusline_running: bool,
    /// When the last status-line run started, for the draw-trigger minimum interval.
    statusline_last_run: Option<Instant>,
    /// A draw trigger was dropped (in flight or debounced), so one trailing run is owed.
    statusline_dirty: bool,
    /// The next draw is the redraw of a run's own result and must not owe another run.
    statusline_result_redraw: bool,
}

/// Why the status-line command should run.
pub enum StatuslineTrigger {
    /// Startup or the refresh timer: not subject to the draw debounce.
    Scheduled,
    /// A draw happened while no run was in flight.
    Draw,
}

/// Runs the interface until the user quits or `input` closes. `render` is called with the
/// interface state after every change; it may advance the transcript's commit cursor, and is
/// called once more with `App::flush` set so whatever is still held back reaches the terminal.
/// `notices.startup` is shown before every other notice; whatever arrives on `notices.feed`
/// while the session runs is shown as it comes.
#[allow(clippy::too_many_arguments)] // each is an independent input of the session
pub async fn drive(
    mut setup: AgentSetup,
    launch_dir: PathBuf,
    notices: Notices,
    input: UnboundedReceiver<Input>,
    render: impl FnMut(&mut App),
    clipboard: impl FnMut(&str) -> std::io::Result<()> + 'static,
    suspend: Suspend,
    options: DriveOptions,
) -> Result<DriveOutcome, String> {
    let Notices { startup, feed } = notices;
    let DriveOptions { budget, on_quit } = options;
    let (tx, prompts) = unbounded_channel();
    let feed_tx = tx.clone();
    let warn_tx = tx.clone();
    setup.approver = Arc::new(ChannelApprover { tx });
    let model = setup.models.main.reference.to_string();
    let mode = setup.mode;
    let primary_root = setup.roots[0].clone();
    let (history, history_error) = history::History::load(
        &setup.data_dir.join("history.jsonl"),
        &primary_root.display().to_string(),
    );
    let (recent, recent_error) = RecentModels::load(&setup.data_dir.join("recent_models.json"));
    let data_dir = setup.data_dir.clone();
    let user_config = setup.user_config.clone();
    let env = setup.env.clone();
    let env_lookup = env.clone();
    let launcher = ui::Launcher::for_platform(setup.git_bash.clone());
    let mut runtime =
        AgentRuntime::new(setup).map_err(|e| fontaine_tools::error_chain::chain(&e))?;
    let (agent_sink, agent_events) = unbounded_channel();
    runtime.state.set_agent_event_sink(agent_sink);
    // A pause waits for as long as it takes while the human can cancel it; while AFK nobody
    // can, so it gets the cap a headless run has.
    runtime
        .state
        .set_usage_wait(fontaine_core::gate::UsageWait {
            cap: None,
            afk_cap: Some(Duration::from_secs(
                runtime.config.headless.max_usage_wait_secs,
            )),
            exit: false,
        });
    runtime.record_session_start().await;
    // Merged into the prompts so a notice shows the way the approver's own do. Stopped with the
    // session; what it had not yet taken stays in the feed for the next one.
    let forwarder = feed.map(|feed| {
        tokio::spawn(async move {
            loop {
                let next = feed.lock().await.recv().await;
                let Some(notice) = next else {
                    return;
                };
                if feed_tx
                    .send(Prompt::Notice(notice.severity, notice.to_string()))
                    .is_err()
                {
                    return;
                }
            }
        })
    });
    let theme = Theme::preset(runtime.config.tui.theme);
    let quota = [
        fontaine_core::quota::QuotaPoller::new(),
        fontaine_core::quota::QuotaPoller::new(),
    ];
    let (statusline_tx, statusline_rx) = tokio::sync::mpsc::unbounded_channel();
    let (statusline_result_tx, statusline_result_rx) = tokio::sync::mpsc::unbounded_channel();
    let statusline_script = runtime.config.statusline.command.as_ref().map(|command| {
        StatuslineScript::new(
            command,
            runtime.scrub_env().to_vec(),
            fontaine_core::tools::subprocess_env(&runtime.config),
            runtime.command_shell(),
            runtime.state.scope().primary().to_path_buf(),
        )
    });
    let mut ui = Ui {
        hook_notifier: notify::Notifier::new(fontaine_core::config::NotifyMethod::Off),
        afk_stops: runtime.state.afk_stop_signal(),
        app: App::new(mode, model),
        input,
        prompts,
        agent_events,
        render,
        spinner_epoch: std::time::Instant::now(),
        recent,
        pending_usage_switch: None,
        paused_base: Duration::ZERO,
        paused_base_for: None,
        pending_pick: None,
        pending_resume: None,
        pending_afk_review: Vec::new(),
        config_pending: Default::default(),
        pending_config: VecDeque::new(),
        pending_provider: VecDeque::new(),
        data_dir,
        pending_drafts: Default::default(),
        user_config,
        env,
        mcp_status: Vec::new(),
        prompts_revision: None,
        clipboard: Box::new(clipboard),
        suspend,
        launcher,
        switch: None,
        quota_rx: [quota[0].subscribe(), quota[1].subscribe()],
        quota,
        quota_warned: Default::default(),
        quit_budget: budget,
        on_quit,
        quit_started: None,
        statusline_task: None,
        statusline_tx: statusline_script.is_some().then_some(statusline_tx),
        statusline_rx: statusline_script.is_some().then_some(statusline_rx),
        statusline_result_tx,
        statusline_result_rx,
        statusline_script,
        statusline_running: false,
        statusline_last_run: None,
        statusline_dirty: false,
        statusline_result_redraw: false,
    };
    ui.app.theme = theme;
    // A link the browser refused reaches the person the way the approver's warnings do: the
    // transcript and the bug report's ring buffer.
    ui.app.set_warn_sink(Arc::new(move |message: &str| {
        let _ = warn_tx.send(Prompt::Notice(Severity::Warning, message.to_string()));
    }));
    // Config validation has parsed and scope-checked every entry, so a resolve failure here
    // would mean the merged config came from somewhere other than `config::load`.
    ui.app.keymap = keymap::Keymap::resolve(&runtime.config.keybindings)
        .expect("config validation has rejected invalid keybindings");
    ui.app.input.mode = runtime.config.tui.editor_mode;
    ui.app.transcript.glyphs = theme::Glyphs::from_env(
        (env_lookup)(theme::ASCII_GLYPHS_VAR)
            .and_then(|v| v.into_string().ok())
            .as_deref(),
    );
    ui.app.transcript.show_reasoning = runtime.config.tui.show_reasoning;
    ui.app.history = history;
    ui.app.history_completion = runtime.config.tui.history_completion;
    ui.app.primary_root = primary_root;
    ui.app.todos.set_items(runtime.session.todos().root());
    for notice in startup
        .into_iter()
        .chain(runtime.startup_warnings().iter().cloned())
    {
        ui.app.transcript.push_notice(notice);
    }
    for error in [history_error, recent_error].into_iter().flatten() {
        ui.app.transcript.notice(error);
    }
    for notice in runtime
        .notices()
        .iter()
        .chain(runtime.scanner_notices())
        .cloned()
    {
        ui.app.transcript.push_notice(notice);
    }
    if let fontaine_core::agent::SessionOrigin::Continued { source, how, .. } = runtime.origin() {
        ui.app.transcript.notice(format!(
            "continuing session {source} ({how}) in {mode} mode, with no grants carried over",
            mode = mode.as_str()
        ));
    }
    let (skill_rows, hidden_skills) = commands::skill_menu(runtime.slash_skills());
    ui.app.menu.extend(skill_rows);
    for name in hidden_skills {
        ui.app.transcript.notice(format!(
            "skill /{name} is hidden by the built-in command of the same name"
        ));
    }
    ui.app.menu.sort_by(|a, b| a.name.cmp(&b.name));
    if mode.rules() == PermissionMode::Auto {
        ui.announce_auto(&runtime);
    } else {
        ui.announce_signed_out_guardian(&runtime);
    }
    if runtime.state.afk().stopped.is_some() {
        ui.app.transcript.notice(
            "this session was stopped by the AFK temperature ceiling and no tool call will run until you review it: /afk report shows why, /afk review lets it continue",
        );
    }
    if runtime.state.afk_on() {
        ui.app.transcript.notice(
            "AFK mode is on: nothing is asked of you; each call that would have needed approval is denied. Shift+Tab leaves it.",
        );
    }
    if ui.start_mcp(&mut runtime).await {
        ui.run(&mut runtime, &launch_dir).await;
    }
    // Stopped with the session; what it had not yet taken stays in the feed for the next one.
    if let Some(forwarder) = &forwarder {
        forwarder.abort();
    }
    if ui.switch.is_some() {
        // The hooks hear of the end before what runs for the session is stopped.
        runtime.fire_session_end("resume").await;
        // Whatever is still running is stopped with the session.
        runtime.state.agents().kill_all();
        runtime.state.agents().settled(KILL_SETTLE).await;
        runtime.jobs().terminate_all().await;
        runtime.lsp().shutdown_all().await;
    } else {
        // An input that closed with no quit request still starts the clock here.
        ui.request_quit(&runtime);
        runtime
            .finish_final_shutdown(ui.quit_deadline().expect("the quit was just latched"))
            .await;
    }
    // What stays in the terminal is the main conversation, not an agent's view.
    if ui.app.viewing.is_some() {
        ui.app.view(None);
    }
    ui.app.flush = true;
    (ui.render)(&mut ui.app);
    Ok(ui
        .switch
        .take()
        .map_or(DriveOutcome::Exit, DriveOutcome::Switch))
}

type LiveScreen = Screen<Anchored<CrosstermBackend<FrameWriter>>>;

/// Undoes what `run` set up, apart from the live area.
pub(crate) fn restore_terminal() {
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        EndSynchronizedUpdate,
        DisableBracketedPaste,
        Show
    );
    let _ = disable_raw_mode();
}

/// Puts the terminal back however the interface exits, panics included. Only the graphical
/// renderer has a live area to clear; the linear one leaves the cursor on a fresh line.
struct TerminalGuard {
    screen: Option<Arc<Mutex<LiveScreen>>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // After a panic the hook has already restored the terminal and printed its message in
        // the live area, which clearing would erase.
        if !std::thread::panicking() {
            match &self.screen {
                Some(screen) => {
                    if let Ok(mut screen) = screen.lock() {
                        let _ = screen.clear_live();
                    }
                }
                None => {
                    use std::io::Write;
                    let mut out = std::io::stdout();
                    let _ = out.write_all(b"\r\n");
                    let _ = out.flush();
                }
            }
        }
        restore_terminal();
    }
}

pub(crate) fn read_terminal(tx: &UnboundedSender<Input>, stop: &AtomicBool, pause: &ReaderPause) {
    while !stop.load(Ordering::Relaxed) && !tx.is_closed() {
        if pause.requested.load(Ordering::SeqCst) {
            pause.parked.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        pause.parked.store(false, Ordering::SeqCst);
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => return,
        }
        let event = match event::read() {
            Ok(event) => event,
            Err(_) => return,
        };
        let input = match event {
            Event::Key(key) if paste_burst::enabled() && paste_burst::is_burst_key(&key) => {
                let (inputs, error) = paste_burst::coalesce(key, |gap| {
                    if stop.load(Ordering::Relaxed) || pause.requested.load(Ordering::SeqCst) {
                        return Ok(None);
                    }
                    if event::poll(gap)? {
                        event::read().map(Some)
                    } else {
                        Ok(None)
                    }
                });
                for input in inputs {
                    if tx.send(input).is_err() {
                        return;
                    }
                }
                if error.is_some() {
                    return;
                }
                continue;
            }
            Event::Key(key) => Input::Key(key),
            Event::Paste(text) => Input::Paste(text),
            Event::Resize(..) => Input::Resize,
            _ => continue,
        };
        if tx.send(input).is_err() {
            return;
        }
    }
}

/// Takes over the terminal and runs the session. With `screen_reader` the interface is the
/// linear one instead of the live area.
pub async fn run(
    setup: AgentSetup,
    launch_dir: PathBuf,
    screen_reader: Option<accessibility::Source>,
    notices: Notices,
) -> Result<RunOutcome, String> {
    enable_raw_mode().map_err(|e| {
        format!(
            "could not start the terminal UI: {}",
            fontaine_tools::error_chain::chain(&e)
        )
    })?;
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous_hook(info);
    }));
    let screen = if screen_reader.is_some() {
        None
    } else {
        match Screen::new(
            Anchored::new(CrosstermBackend::new(FrameWriter::stdout())),
            || Anchored::new(CrosstermBackend::new(FrameWriter::discarding(Tty::std()))),
        ) {
            Ok(screen) => Some(Arc::new(Mutex::new(
                screen.with_notify(setup.config.tui.notify),
            ))),
            Err(e) => {
                restore_terminal();
                return Err(format!(
                    "could not start the terminal UI: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
            }
        }
    };
    let _guard = TerminalGuard {
        screen: screen.clone(),
    };

    let (tx, rx) = unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(ReaderPause::default());
    let reader = {
        let (stop, pause) = (stop.clone(), pause.clone());
        tokio::task::spawn_blocking(move || read_terminal(&tx, &stop, &pause))
    };
    let suspend: Suspend = {
        let screen = screen.clone();
        let pause = pause.clone();
        Box::new(move |work| {
            pause.pause();
            match &screen {
                Some(screen) => {
                    if let Ok(mut screen) = screen.lock() {
                        let _ = screen.clear_live();
                    }
                }
                None => {
                    use std::io::Write;
                    let mut out = std::io::stdout();
                    let _ = out.write_all(b"\r\n");
                    let _ = out.flush();
                }
            }
            restore_terminal();
            work();
            // The editor may have left the terminal anywhere; a failure here shows as the next
            // frame failing, which ends the session like any lost terminal.
            let _ = enable_raw_mode();
            let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
            if let Some(screen) = &screen
                && let Ok(mut screen) = screen.lock()
            {
                let _ = screen.resume();
            }
            pause.resume();
        })
    };
    // A failed write means stdout is gone: ending the reader closes the input and the session
    // ends the way it does when the terminal closes.
    let on_error = stop.clone();
    let mut linear = linear::Linear::new(std::io::stdout());
    let hyperlinks = hyperlink::supported(|k| std::env::var(k).ok(), cfg!(windows));
    // Armed from inside the session at the first quit request, and kept out of it so it outlives
    // everything the caller tears down after this returns.
    let armed: Arc<Mutex<Option<ExitWatchdog>>> = Arc::default();
    let on_quit = {
        let armed = armed.clone();
        move |deadline: Instant| {
            let watchdog = ExitWatchdog::arm(deadline, restore_terminal, || std::process::exit(0));
            // Display-only slot for the watchdog handle; a poisoned lock still holds it.
            *fontaine_tools::lock::lock_recovering(&armed) = Some(watchdog);
        }
    };
    let result = drive(
        setup,
        launch_dir,
        notices,
        rx,
        move |app| {
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
        },
        clipboard::copy,
        suspend,
        DriveOptions {
            budget: FINAL_SHUTDOWN_BUDGET,
            on_quit: Some(Box::new(on_quit)),
        },
    )
    .await;
    stop.store(true, Ordering::Relaxed);
    // The slot only holds the watchdog handle; a poisoned lock still holds it.
    let watchdog = fontaine_tools::lock::lock_recovering(&armed).take();
    // A reader stuck in a read must not hold the exit past its deadline.
    let _ = match watchdog.as_ref() {
        Some(watchdog) => tokio::time::timeout(
            watchdog
                .deadline()
                .saturating_duration_since(Instant::now()),
            reader,
        )
        .await
        .ok(),
        None => Some(reader.await),
    };
    result.map(|outcome| RunOutcome { outcome, watchdog })
}

#[cfg(test)]
mod build_version_tests {
    use super::*;

    #[test]
    fn the_recorded_version_wins_and_the_package_version_is_the_fallback() {
        let info = BuildInfo {
            version: "9.9.9-dev+abc123",
            git_sha: "abc123",
            target: "t",
        };
        assert_eq!(version_or_package(Some(&info)), "9.9.9-dev+abc123");
        assert_eq!(version_or_package(None), env!("CARGO_PKG_VERSION"));
    }
}
