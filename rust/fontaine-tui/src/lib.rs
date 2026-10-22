//! The interactive terminal UI. [`run`] takes over the terminal; [`drive`] is the same session
//! loop with the terminal swapped for an input channel and a render callback, which is how the
//! tests exercise it.

pub mod app;
pub mod approval;
pub mod input;
pub mod statusbar;
pub mod transcript;
pub mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fontaine_core::agent::{AgentRuntime, AgentSetup};
use fontaine_core::ledger::Record;
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_core::turn::{
    DEFAULT_MAX_TURNS, Flow, SessionEvent, TurnOutcome, TurnParams, run_turn,
};
use futures::future::BoxFuture;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

pub use app::{Action, App, Command, Input, parse_command};

/// What the permission pipeline sends to the interface.
enum Prompt {
    Ask(Box<ApprovalRequest>, oneshot::Sender<Answer>),
    Notice(String),
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

    fn notice(&self, message: &str) {
        let _ = self.tx.send(Prompt::Notice(message.to_string()));
    }
}

/// How a turn ended from the interface's side.
enum TurnEnd {
    Finished(TurnOutcome),
    Interrupted,
    InputClosed,
}

struct Ui<R> {
    app: App,
    input: UnboundedReceiver<Input>,
    prompts: UnboundedReceiver<Prompt>,
    render: R,
}

impl<R: FnMut(&App)> Ui<R> {
    fn draw(&mut self, runtime: &AgentRuntime) {
        self.app.mode = runtime.state.mode();
        (self.render)(&self.app);
    }

    fn on_prompt(&mut self, prompt: Prompt) {
        match prompt {
            Prompt::Ask(request, reply) => self.app.begin_approval(*request, reply),
            Prompt::Notice(message) => self.app.transcript.notice(message),
        }
    }

    fn cycle_mode(&mut self, runtime: &AgentRuntime) {
        let state = &runtime.state;
        state.set_mode(statusbar::next_mode(state.mode()));
    }

    async fn run(&mut self, runtime: &mut AgentRuntime, launch_dir: &Path) {
        loop {
            self.draw(runtime);
            let action = tokio::select! {
                input = self.input.recv() => match input {
                    Some(input) => self.app.handle_input(input),
                    None => return,
                },
                Some(prompt) = self.prompts.recv() => {
                    self.on_prompt(prompt);
                    Action::None
                }
            };
            match action {
                Action::Quit => return,
                Action::CycleMode => self.cycle_mode(runtime),
                Action::Submit(text) => {
                    if self.submit(runtime, launch_dir, text).await.is_break() {
                        return;
                    }
                }
                Action::None | Action::Interrupt => {}
            }
        }
    }

    async fn submit(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        text: String,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        match parse_command(&text) {
            Some(Command::Exit) => return Break(()),
            Some(Command::AddDir(path)) => self.add_dir(runtime, launch_dir, &path),
            Some(Command::Unknown(name)) => self.app.transcript.error(format!(
                "unknown command /{name}; try /add-dir <path> or /exit"
            )),
            None => {
                self.app.transcript.user(text.clone());
                if matches!(self.turn(runtime, text).await, TurnEnd::InputClosed) {
                    return Break(());
                }
            }
        }
        Continue(())
    }

    fn add_dir(&mut self, runtime: &mut AgentRuntime, launch_dir: &Path, path: &str) {
        if path.is_empty() {
            self.app.transcript.error("usage: /add-dir <path>");
            return;
        }
        let path = path.trim_matches(|c| c == '"' || c == '\'');
        match runtime.add_dir(Path::new(path), launch_dir) {
            Ok(root) => self
                .app
                .transcript
                .notice(format!("added workspace root {}", root.display())),
            Err(e) => self.app.transcript.error(format!("/add-dir: {e}")),
        }
    }

    /// Runs one user message while still serving keys and prompts.
    async fn turn(&mut self, runtime: &AgentRuntime, text: String) -> TurnEnd {
        runtime.append(Record::UserMessage { text: text.clone() });
        self.app.running = true;
        let (events, mut seen) = unbounded_channel();
        let mut on_event = move |event: SessionEvent| {
            let _ = events.send(event);
            Flow::Continue
        };
        let mut run = Box::pin(async {
            runtime
                .session
                .compact_if_needed(&runtime.models.main.completion)
                .await;
            run_turn(
                TurnParams {
                    agent: &runtime.agent,
                    state: &runtime.state,
                    prompt: text,
                    max_turns: DEFAULT_MAX_TURNS,
                },
                &mut on_event,
            )
            .await
        });
        let end = loop {
            self.draw(runtime);
            tokio::select! {
                biased;
                Some(event) = seen.recv() => {
                    self.app.on_event(event);
                    // One redraw for everything already queued, not one per streamed delta.
                    while let Ok(event) = seen.try_recv() {
                        self.app.on_event(event);
                    }
                }
                outcome = &mut run => break TurnEnd::Finished(outcome),
                input = self.input.recv() => match input {
                    None => break TurnEnd::InputClosed,
                    Some(input) => match self.app.handle_input(input) {
                        Action::Interrupt | Action::Quit => break TurnEnd::Interrupted,
                        Action::CycleMode => self.cycle_mode(runtime),
                        Action::None | Action::Submit(_) => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(prompt),
            }
        };
        drop(run);
        while let Ok(event) = seen.try_recv() {
            self.app.on_event(event);
        }
        self.finish_turn(runtime, &end).await;
        end
    }

    async fn finish_turn(&mut self, runtime: &AgentRuntime, end: &TurnEnd) {
        self.app.running = false;
        self.app.deny_pending();
        match end {
            TurnEnd::Finished(TurnOutcome::Failed(message)) => {
                self.app.transcript.error(message.clone());
            }
            TurnEnd::Finished(TurnOutcome::MaxTurns(message)) => {
                self.app.transcript.notice(message.clone());
            }
            TurnEnd::Finished(_) | TurnEnd::InputClosed => {}
            TurnEnd::Interrupted => {
                self.app.transcript.interrupt_pending();
                self.app.transcript.notice("interrupted");
                // Decisions the cut-short calls already made belong to this turn, not the next.
                let mut feed = runtime.state.feed().await;
                while feed.try_next().is_some() {}
            }
        }
    }
}

/// Runs the interface until the user quits or `input` closes. `render` is called with the
/// interface state after every change.
pub async fn drive(
    mut setup: AgentSetup,
    launch_dir: PathBuf,
    input: UnboundedReceiver<Input>,
    render: impl FnMut(&App),
) -> Result<(), String> {
    let (tx, prompts) = unbounded_channel();
    setup.approver = Arc::new(ChannelApprover { tx });
    let model = setup.models.main.reference.to_string();
    let mode = setup.mode;
    let mut runtime = AgentRuntime::new(setup)?;
    runtime.record_session_start().await;
    let mut ui = Ui {
        app: App::new(mode, model),
        input,
        prompts,
        render,
    };
    ui.run(&mut runtime, &launch_dir).await;
    Ok(())
}

/// Puts the terminal back however the interface exits, panics included.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

fn read_terminal(tx: &UnboundedSender<Input>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) && !tx.is_closed() {
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

pub async fn run(setup: AgentSetup, launch_dir: PathBuf) -> Result<(), String> {
    let mut terminal =
        ratatui::try_init().map_err(|e| format!("could not start the terminal UI: {e}"))?;
    let _guard = TerminalGuard;
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);

    let (tx, rx) = unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || read_terminal(&tx, &stop))
    };
    let result = drive(setup, launch_dir, rx, |app| {
        let _ = terminal.draw(|frame| view::draw(frame, app));
    })
    .await;
    stop.store(true, Ordering::Relaxed);
    let _ = reader.await;
    result
}
