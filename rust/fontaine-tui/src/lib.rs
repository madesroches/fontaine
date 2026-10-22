//! The interactive terminal UI. [`run`] takes over the terminal; [`drive`] is the same session
//! loop with the terminal swapped for an input channel and a render callback, which is how the
//! tests exercise it.

pub mod app;
pub mod approval;
pub mod denials;
pub mod input;
pub mod statusbar;
pub mod transcript;
pub mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fontaine_core::agent::{AgentRuntime, AgentSetup};
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode;
use fontaine_core::ledger::Record;
use fontaine_core::permissions::{
    Answer, ApprovalRequest, Approver, GuardianUnavailable, UnavailableChoice,
};
use fontaine_core::tier::Tier;
use fontaine_core::turn::{
    DEFAULT_MAX_TURNS, Flow, SessionEvent, TurnOutcome, TurnParams, run_turn,
};
use futures::future::BoxFuture;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

pub use app::{Action, App, Command, Input, parse_command};
pub use denials::UnavailableAnswer;

use denials::{DeniedList, UnavailablePrompt};

/// What the permission pipeline sends to the interface.
enum Prompt {
    Ask(Box<ApprovalRequest>, oneshot::Sender<Answer>),
    /// The pinned guardian cannot be used and the human has to decide what to do.
    Unavailable(Box<GuardianUnavailable>, oneshot::Sender<UnavailableChoice>),
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

    fn warn(&self, message: &str) {
        self.notice(message);
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
        self.app.model = runtime.models.main.reference.to_string();
        (self.render)(&self.app);
    }

    fn on_prompt(&mut self, runtime: Option<&AgentRuntime>, prompt: Prompt) {
        match prompt {
            // The call's turn was cut short while the prompt sat in the queue.
            Prompt::Ask(_, reply) if reply.is_closed() => {}
            Prompt::Unavailable(_, reply) if reply.is_closed() => {}
            Prompt::Ask(request, reply) => self.app.begin_approval(*request, reply),
            Prompt::Unavailable(info, reply) => {
                let candidates = runtime.map_or_else(Vec::new, AgentRuntime::guardian_candidates);
                self.app
                    .begin_unavailable(UnavailablePrompt::new(*info, candidates, reply));
            }
            Prompt::Notice(message) => self.app.transcript.notice(message),
        }
    }

    /// Handles what queued up during a turn that has ended: the asks belong to calls that no
    /// longer exist, so they are dropped, but notices still count.
    fn drain_prompts(&mut self) {
        while let Ok(prompt) = self.prompts.try_recv() {
            if let Prompt::Notice(message) = prompt {
                self.app.transcript.notice(message);
            }
        }
    }

    fn cycle_mode(&mut self, runtime: &AgentRuntime) {
        let state = &runtime.state;
        state.set_mode(statusbar::next_mode(state.mode()));
        if state.mode() == PermissionMode::Auto {
            self.announce_auto(runtime);
        }
    }

    /// What the human should know on entering `auto`: the allow rules it sets aside, and a
    /// guardian that cannot be asked.
    fn announce_auto(&mut self, runtime: &AgentRuntime) {
        let suspended = runtime.state.suspended_allow_rules();
        if !suspended.is_empty() {
            self.app.transcript.notice(format!(
                "auto mode sets aside these broad allow rules while it is on: {}",
                suspended.join(", ")
            ));
        }
        let guardian = runtime.state.guardian();
        if let Some(reason) = guardian.unavailable_reason() {
            self.app.transcript.error(format!(
                "the guardian model {} is unavailable ({reason}); calls that need it are denied until you fix it with /guardian-model",
                guardian.label()
            ));
        }
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
                    self.on_prompt(Some(runtime), prompt);
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
                Action::RetryDenied(index) => self.retry_denied(runtime, index),
                Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
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
            Some(Command::Denied) => {
                self.app.denied = Some(DeniedList::new(runtime.state.denied_calls()));
            }
            Some(Command::Model(reference)) => self.model(runtime, &reference).await,
            Some(Command::GuardianModel(reference)) => self.guardian_model(runtime, &reference),
            Some(Command::Unknown(name)) => self.app.transcript.error(format!(
                "unknown command /{name}; try /add-dir <path>, /denied, /model [account/model], /guardian-model [account/model] or /exit"
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

    /// `/model`: shows the model, or switches the live session to another and saves the choice.
    async fn model(&mut self, runtime: &mut AgentRuntime, reference: &str) {
        if reference.is_empty() {
            self.app.transcript.notice(format!(
                "model {} · guardian {}",
                runtime.models.main.reference,
                runtime.state.guardian().label()
            ));
            return;
        }
        match runtime.switch_main_model(reference).await {
            Ok(switched) => {
                let mut message = format!("model is now {}", switched.reference);
                if switched.guardian_followed {
                    message.push_str("; the guardian runs on it too");
                }
                self.finish_switch(message, switched.persist_error);
            }
            Err(e) => self.app.transcript.error(format!("/model: {e}")),
        }
    }

    /// `/guardian-model`: shows the guardian and the models it could be, or pins it to one.
    fn guardian_model(&mut self, runtime: &mut AgentRuntime, reference: &str) {
        if reference.is_empty() {
            let guardian = runtime.state.guardian();
            let others: Vec<String> = runtime
                .guardian_candidates()
                .iter()
                .map(ToString::to_string)
                .collect();
            let mut message = format!("guardian {}", guardian.label());
            if let Some(reason) = guardian.unavailable_reason() {
                message.push_str(&format!(" (unavailable: {reason})"));
            }
            if !others.is_empty() {
                message.push_str(&format!(" · available: {}", others.join(", ")));
            }
            self.app.transcript.notice(message);
            return;
        }
        match runtime.set_guardian_model(reference) {
            Ok(switched) => {
                let guardian = runtime.state.guardian();
                match guardian.unavailable_reason() {
                    Some(reason) => self.app.transcript.error(format!(
                        "guardian is now {}, but it is unavailable: {reason}",
                        switched.reference
                    )),
                    None => self.finish_switch(
                        format!("guardian is now {}", switched.reference),
                        switched.persist_error,
                    ),
                }
            }
            Err(e) => self.app.transcript.error(format!("/guardian-model: {e}")),
        }
    }

    fn finish_switch(&mut self, message: String, persist_error: Option<String>) {
        match persist_error {
            None => self
                .app
                .transcript
                .notice(format!("{message} (saved to your settings)")),
            Some(why) => self
                .app
                .transcript
                .notice(format!("{message} for this session only: {why}")),
        }
    }

    /// `r` in `/denied`: a human approval of that call, once, and a note to the agent.
    fn retry_denied(&mut self, runtime: &AgentRuntime, index: usize) {
        let Some(call) = runtime.state.approve_retry(index) else {
            return;
        };
        if let Some(list) = &mut self.app.denied {
            list.mark_retried(index);
        }
        self.app.pending_notices.push(format!(
            "[Harness note, not the user's words: the user reviewed your denied `{}` call ({}) and approved it once. You may retry exactly that call.]",
            call.tool, call.summary
        ));
        self.app.transcript.notice(format!(
            "approved once: {} {}; the agent is told it may retry",
            call.tool, call.summary
        ));
    }

    /// The human's answer to an unavailable guardian: the session is changed first, then the
    /// waiting call is told what to do.
    fn answer_unavailable(&mut self, runtime: &AgentRuntime, answer: UnavailableAnswer) {
        let choice = match answer {
            UnavailableAnswer::Retry => match runtime.retry_guardian() {
                Ok(true) => UnavailableChoice::Retry,
                Ok(false) => {
                    self.app
                        .transcript
                        .error("the guardian model is still unavailable");
                    UnavailableChoice::Deny
                }
                Err(e) => {
                    self.app.transcript.error(e);
                    UnavailableChoice::Deny
                }
            },
            UnavailableAnswer::Pick(reference) => match runtime.use_guardian(&reference) {
                Ok(true) => {
                    self.app
                        .transcript
                        .notice(format!("guardian is now {reference} for this session"));
                    UnavailableChoice::Retry
                }
                Ok(false) => {
                    self.app
                        .transcript
                        .error(format!("{reference} is unavailable too"));
                    UnavailableChoice::Deny
                }
                Err(e) => {
                    self.app.transcript.error(e);
                    UnavailableChoice::Deny
                }
            },
            UnavailableAnswer::Manual => {
                self.app
                    .transcript
                    .notice("switched this session to manual mode");
                UnavailableChoice::Manual
            }
            UnavailableAnswer::Deny => UnavailableChoice::Deny,
        };
        self.app.finish_unavailable(choice);
    }

    /// Runs one user message while still serving keys and prompts.
    async fn turn(&mut self, runtime: &AgentRuntime, text: String) -> TurnEnd {
        runtime.append(Record::UserMessage { text: text.clone() });
        // What the agent reads is the user's words behind any note the harness owes it.
        let mut prompt = String::new();
        for note in self.app.pending_notices.drain(..) {
            prompt.push_str(&note);
            prompt.push_str("\n\n");
        }
        prompt.push_str(&text);
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
                    prompt,
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
                    self.on_event(runtime, event);
                    // One redraw for everything already queued, not one per streamed delta.
                    while let Ok(event) = seen.try_recv() {
                        self.on_event(runtime, event);
                    }
                }
                outcome = &mut run => break TurnEnd::Finished(outcome),
                input = self.input.recv() => match input {
                    None => break TurnEnd::InputClosed,
                    Some(input) => match self.app.handle_input(input) {
                        Action::Interrupt | Action::Quit => break TurnEnd::Interrupted,
                        Action::CycleMode => self.cycle_mode(runtime),
                        Action::RetryDenied(index) => self.retry_denied(runtime, index),
                        Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                        Action::None | Action::Submit(_) => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
            }
        };
        drop(run);
        while let Ok(event) = seen.try_recv() {
            self.on_event(runtime, event);
        }
        self.finish_turn(runtime, &end).await;
        end
    }

    /// Shows an event. A call the guardian refused is shown with its rule label and its own
    /// words rather than the note the agent is given.
    fn on_event(&mut self, runtime: &AgentRuntime, event: SessionEvent) {
        let denial = match &event {
            SessionEvent::Decision(record)
                if record.tier == Tier::Guardian && record.decision == Decision::Deny =>
            {
                runtime
                    .state
                    .denied_calls()
                    .into_iter()
                    .rev()
                    .find(|call| call.tool_use_id == record.tool_use_id)
            }
            _ => None,
        };
        self.app.on_event(event);
        if let Some(call) = denial {
            self.app.transcript.guardian_denied(
                call.tool_use_id.as_deref().unwrap_or(""),
                &call.tool,
                &call.rule,
                &call.explanation,
            );
        }
    }

    async fn finish_turn(&mut self, runtime: &AgentRuntime, end: &TurnEnd) {
        self.app.running = false;
        self.app.deny_pending();
        self.drain_prompts();
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
    for notice in runtime.notices.clone() {
        ui.app.transcript.notice(notice);
    }
    if mode == PermissionMode::Auto {
        ui.announce_auto(&runtime);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::config::PermissionMode;
    use fontaine_core::permissions::Offers;
    use fontaine_core::tier::Tier;

    fn ui(prompts: UnboundedReceiver<Prompt>) -> Ui<impl FnMut(&App)> {
        Ui {
            app: App::new(PermissionMode::Manual, "m".into()),
            input: unbounded_channel().1,
            prompts,
            render: |_: &App| {},
        }
    }

    fn ask() -> (Prompt, oneshot::Receiver<Answer>) {
        let (reply, answer) = oneshot::channel();
        let request = ApprovalRequest {
            tool: "shell".into(),
            agent: "main".into(),
            tool_use_id: None,
            args: serde_json::json!({}),
            summary: "ls".into(),
            tier: Tier::AllowRule,
            reason: "r".into(),
            offers: Offers::default(),
            proposed_rule: None,
        };
        (Prompt::Ask(Box::new(request), reply), answer)
    }

    #[test]
    fn an_ask_queued_when_the_turn_ends_never_opens() {
        let (tx, rx) = unbounded_channel();
        let mut ui = ui(rx);
        let (prompt, answer) = ask();
        tx.send(prompt).unwrap();
        tx.send(Prompt::Notice("kept".into())).unwrap();
        ui.drain_prompts();
        assert!(ui.app.approval.is_none());
        assert!(ui.prompts.try_recv().is_err());
        drop(answer);
    }

    #[test]
    fn an_ask_whose_call_is_gone_is_ignored() {
        let (_tx, rx) = unbounded_channel();
        let mut ui = ui(rx);
        let (prompt, answer) = ask();
        drop(answer);
        ui.on_prompt(None, prompt);
        assert!(ui.app.approval.is_none());
    }
}
