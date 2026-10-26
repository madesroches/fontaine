//! The interactive terminal UI. [`run`] takes over the terminal; [`drive`] is the same session
//! loop with the terminal swapped for an input channel and a render callback, which is how the
//! tests exercise it.

pub mod accessibility;
pub mod agents_panel;
pub mod app;
pub mod approval;
pub mod clipboard;
pub mod commands;
pub mod complete;
pub mod config_panel;
pub mod denials;
pub mod history;
pub mod input;
pub mod linear;
pub mod model_picker;
pub mod provider_dialog;
pub mod question;
pub mod recent;
pub mod reports;
pub mod screen;
pub mod server_prompt;
pub mod setup;
pub mod statusbar;
pub mod theme;
pub mod todo_panel;
pub mod transcript;
pub mod trust_prompt;
mod ui;
pub mod view;

use crate::config_panel::ConfigKey;
use std::collections::VecDeque;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_core::agent::{AgentRuntime, AgentSetup, EnvLookup};
use fontaine_core::config::PermissionMode;
use fontaine_core::notice::{Notice, Severity};
use fontaine_core::permissions::{
    Answer, ApprovalRequest, Approver, GuardianUnavailable, UnavailableChoice,
};
use fontaine_tools::ask_user::{Question, QuestionReply};
use futures::future::BoxFuture;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use ratatui::crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

pub use app::{Action, App, Input};
pub use commands::{Command, MenuEntry, builtin_menu, parse_command};
pub use denials::UnavailableAnswer;

use model_picker::PickerPurpose;
use provider_dialog::DialogKey;
use recent::RecentModels;
use reports::McpState;
use screen::{Anchored, Screen};
use theme::Theme;

/// What the permission pipeline sends to the interface.
enum Prompt {
    Ask(Box<ApprovalRequest>, oneshot::Sender<Answer>),
    /// The pinned guardian cannot be used and the human has to decide what to do.
    Unavailable(Box<GuardianUnavailable>, oneshot::Sender<UnavailableChoice>),
    /// The model's questions for the human, with the agent that asks. A dropped sender means
    /// nobody could answer.
    Question(String, Vec<Question>, oneshot::Sender<QuestionReply>),
    Notice(Severity, String),
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

    fn notice(&self, message: &str) {
        let _ = self
            .tx
            .send(Prompt::Notice(Severity::Info, message.to_string()));
    }

    fn warn(&self, message: &str) {
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
    input: UnboundedReceiver<Input>,
    prompts: UnboundedReceiver<Prompt>,
    render: R,
    recent: RecentModels,
    /// A model picked while a turn ran, applied once the turn is over: only the idle loop owns
    /// the runtime mutably.
    pending_pick: Option<(PickerPurpose, String)>,
    /// Settings saved from `/config` that apply from the next launch, by key, as the label the
    /// panel shows; they outlive the panel so reopening it still shows them.
    config_pending: std::collections::BTreeMap<String, String>,
    /// Panel changes and picker requests made while a turn ran, carried out once it is over.
    pending_config: VecDeque<ConfigKey>,
    /// Requests the provider dialog made while a turn ran, carried out once it is over.
    pending_provider: VecDeque<DialogKey>,
    data_dir: PathBuf,
    user_config: Option<PathBuf>,
    env: EnvLookup,
    /// What starting each MCP server came to, for `/mcp`.
    mcp_status: Vec<(String, McpState)>,
    /// Where `/copy` sends text; the write's result comes back to the command.
    clipboard: ClipboardSink,
    /// Runs the editor `/memory` opens with the terminal handed over.
    suspend: Suspend,
    launcher: ui::Launcher,
}

/// Runs the interface until the user quits or `input` closes. `render` is called with the
/// interface state after every change; it may advance the transcript's commit cursor, and is
/// called once more with `App::flush` set so whatever is still held back reaches the terminal.
/// `startup` is shown before every other notice.
pub async fn drive(
    mut setup: AgentSetup,
    launch_dir: PathBuf,
    startup: Vec<Notice>,
    input: UnboundedReceiver<Input>,
    render: impl FnMut(&mut App),
    clipboard: impl FnMut(&str) -> std::io::Result<()> + 'static,
    suspend: Suspend,
) -> Result<(), String> {
    let (tx, prompts) = unbounded_channel();
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
    runtime.record_session_start().await;
    let theme = Theme::preset(runtime.config.tui.theme);
    let mut ui = Ui {
        app: App::new(mode, model),
        input,
        prompts,
        render,
        recent,
        pending_pick: None,
        config_pending: Default::default(),
        pending_config: VecDeque::new(),
        pending_provider: VecDeque::new(),
        data_dir,
        user_config,
        env,
        mcp_status: Vec::new(),
        clipboard: Box::new(clipboard),
        suspend,
        launcher,
    };
    ui.app.theme = theme;
    ui.app.transcript.glyphs = theme::Glyphs::from_env(
        (env_lookup)(theme::ASCII_GLYPHS_VAR)
            .and_then(|v| v.into_string().ok())
            .as_deref(),
    );
    ui.app.history = history;
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
    if let fontaine_core::agent::SessionOrigin::Continued { source, how } = runtime.origin() {
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
    if mode == PermissionMode::Auto {
        ui.announce_auto(&runtime);
    }
    if ui.start_mcp(&mut runtime).await {
        ui.run(&mut runtime, &launch_dir).await;
    }
    // Whatever is still running is stopped with the session.
    runtime.state.agents().kill_all();
    ui.app.flush = true;
    (ui.render)(&mut ui.app);
    Ok(())
}

type LiveScreen = Screen<Anchored<CrosstermBackend<Stdout>>>;

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
        let input = match event::read() {
            Ok(Event::Key(key)) => Input::Key(key),
            Ok(Event::Paste(text)) => Input::Paste(text),
            Ok(Event::Resize(..)) => Input::Resize,
            Ok(_) => continue,
            Err(_) => return,
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
    startup: Vec<Notice>,
) -> Result<(), String> {
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
            Anchored::new(CrosstermBackend::new(std::io::stdout())),
            || Anchored::new(CrosstermBackend::new(std::io::stdout())),
        ) {
            Ok(screen) => Some(Arc::new(Mutex::new(screen))),
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
    let result = drive(
        setup,
        launch_dir,
        startup,
        rx,
        move |app| {
            let rendered = match &screen {
                Some(screen) => match screen.lock() {
                    Ok(mut screen) => {
                        // A rebuild clears rows before redrawing them; one frame hides that.
                        let mut out = std::io::stdout();
                        let _ = ratatui::crossterm::execute!(out, BeginSynchronizedUpdate);
                        let ok = if app.flush {
                            screen.commit_all(app)
                        } else {
                            screen.render(app)
                        }
                        .is_ok();
                        let _ = ratatui::crossterm::execute!(out, EndSynchronizedUpdate);
                        ok
                    }
                    Err(_) => false,
                },
                None => linear.render(app).is_ok(),
            };
            if !rendered {
                on_error.store(true, Ordering::Relaxed);
            }
        },
        clipboard::copy,
        suspend,
    )
    .await;
    stop.store(true, Ordering::Relaxed);
    let _ = reader.await;
    result
}
