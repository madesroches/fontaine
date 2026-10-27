use crate::config_panel::ConfigKey;
use crate::{Action, App, Ui};
use fontaine_core::RuntimeError;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::Decision;
use fontaine_core::goal::Continue;
use fontaine_core::ledger::Record;
use fontaine_core::schedule::{Fired, Kind};
use fontaine_core::tier::Tier;
use fontaine_core::turn::{
    DEFAULT_MAX_TURNS, Flow, SessionEvent, TurnOutcome, TurnParams, run_turn,
};
use tokio::sync::mpsc::unbounded_channel;

/// How a turn ended from the interface's side.
pub(crate) enum TurnEnd {
    Finished(TurnOutcome),
    Interrupted,
    InputClosed,
}

/// The keys `serve_while` acts on beyond the ones every wait serves.
#[derive(Clone, Copy)]
pub(crate) enum Serve {
    /// Prompts and mode keys only: a skill expansion.
    Prompts,
    /// Also retrying a denied call: a `!` command, where typed text has no turn to go to.
    Retry,
    /// Also queueing typed text as steering, which the caller hands back to the input.
    Queueing,
}

/// What becomes of messages typed during a turn that the agent never received.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EndOfTurn {
    /// The agent answered without another tool result: send them as the next turn.
    Submit(String),
    /// The turn did not complete: give them back so nothing the human typed is sent unseen.
    RestoreToInput(String),
    Nothing,
}

pub(crate) fn end_of_turn(end: &TurnEnd, retracted: Vec<String>) -> EndOfTurn {
    if retracted.is_empty() {
        return EndOfTurn::Nothing;
    }
    let text = retracted.join("\n\n");
    match end {
        TurnEnd::Finished(TurnOutcome::Answer(_)) => EndOfTurn::Submit(text),
        TurnEnd::Finished(_) | TurnEnd::Interrupted => EndOfTurn::RestoreToInput(text),
        TurnEnd::InputClosed => EndOfTurn::Nothing,
    }
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// Brings the provider's search tool in line with the mode and rules now in force, which a
    /// mode switch or a session rule may have changed since the agent was built.
    ///
    /// When the agent cannot be rebuilt to drop a search tool the gate now withholds, the turn
    /// must not run: this returns how it ends instead.
    fn refresh_native_search(&mut self, runtime: &mut AgentRuntime) -> Option<TurnEnd> {
        let e = runtime.refresh_native_search().err()?;
        let message = format!(
            "the provider's web search could not be turned off, so the turn did not run: {}",
            fontaine_tools::error_chain::chain(&e)
        );
        self.app.transcript.error(message.clone());
        Some(TurnEnd::Finished(TurnOutcome::Failed(message)))
    }

    /// A turn on what the background agents reported. It is not the user speaking, so it is not
    /// recorded as a user message.
    pub(crate) async fn notification_turn(
        &mut self,
        runtime: &mut AgentRuntime,
    ) -> Option<TurnEnd> {
        let prompt = runtime.notification_prompt().await?;
        if let Some(end) = self.refresh_native_search(runtime) {
            return Some(end);
        }
        self.app
            .transcript
            .notice("a background agent reported back");
        Some(self.turn(runtime, prompt, None).await)
    }

    /// A turn on a scheduled task that came due. Like a background agent's report it is not
    /// the user speaking, so it is not recorded as a user message; unlike one, what the human
    /// queued while it ran is sent as the next turn or handed back to the input.
    pub(crate) async fn scheduled_turn(
        &mut self,
        runtime: &mut AgentRuntime,
        fired: Fired,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        let prompt = runtime.scheduled_prompt(&fired).await;
        self.app
            .transcript
            .notice(format!("scheduled: {}", fired.job.id));
        let end = match self.refresh_native_search(runtime) {
            Some(end) => end,
            None => self.turn(runtime, prompt, None).await,
        };
        if matches!(end, TurnEnd::InputClosed) {
            return Break(());
        }
        if matches!(fired.job.kind, Kind::Wakeup { .. }) {
            self.end_of_iteration(runtime);
        }
        match end_of_turn(&end, runtime.steering.retract()) {
            EndOfTurn::Submit(next) => self.converse(runtime, next, None).await,
            EndOfTurn::RestoreToInput(back) => {
                self.app.restore_to_input(&back);
                Continue(())
            }
            EndOfTurn::Nothing => Continue(()),
        }
    }

    /// A turn on a goal the evaluator found not met yet. A harness note, never the user
    /// speaking; what the human queued meanwhile is handled as after a scheduled turn.
    pub(crate) async fn goal_turn(
        &mut self,
        runtime: &mut AgentRuntime,
        next: Continue,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue as Go};
        let prompt = runtime.goal_prompt(&next).await;
        let end = match self.refresh_native_search(runtime) {
            Some(end) => end,
            None => self.turn(runtime, prompt, None).await,
        };
        if matches!(end, TurnEnd::InputClosed) {
            return Break(());
        }
        match end_of_turn(&end, runtime.steering.retract()) {
            EndOfTurn::Submit(next) => self.converse(runtime, next, None).await,
            EndOfTurn::RestoreToInput(back) => {
                self.app.restore_to_input(&back);
                Go(())
            }
            EndOfTurn::Nothing => Go(()),
        }
    }

    /// Drives `fut` to completion while serving keys and prompts. `serve` says which of the
    /// retry and queueing keys the caller handles. `Err` is the user leaving or interrupting;
    /// the caller cleans up.
    pub(crate) async fn serve_while<F: std::future::Future + Unpin>(
        &mut self,
        runtime: &AgentRuntime,
        fut: &mut F,
        serve: Serve,
    ) -> Result<F::Output, TurnEnd> {
        let retry = !matches!(serve, Serve::Prompts);
        let queueing = matches!(serve, Serve::Queueing);
        loop {
            self.draw(runtime);
            tokio::select! {
                biased;
                result = &mut *fut => return Ok(result),
                input = self.input.recv() => match input {
                    None => return Err(TurnEnd::InputClosed),
                    Some(input) => match self.app.handle_input(input) {
                        Action::Interrupt | Action::Quit => return Err(TurnEnd::Interrupted),
                        Action::CycleMode => self.cycle_mode(runtime),
                        Action::RetryDenied(index) if retry => self.retry_denied(runtime, index),
                        Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                        Action::KillAgent(id) => self.kill_agent(runtime, &id),
                        Action::PickModel(purpose, reference) => {
                            self.pending_pick = Some((purpose, reference));
                        }
                        Action::ResumeSession(id) => self.pending_resume = Some(id),
                        Action::Providers(request) => self.queue_provider(request),
                        Action::Config(change) => self.pending_config.push_back(change),
                        Action::OpenPicker(slot) => {
                            self.pending_config.push_back(ConfigKey::OpenPicker(slot));
                        }
                        Action::Queue(text) if queueing => {
                            self.remember(&text);
                            runtime.steering.queue(text);
                        }
                        Action::EditQueued if queueing => self.edit_queued(runtime),
                        _ => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
            }
        }
    }

    /// Expands a skill while still serving prompts and keys: its `!` commands can need a
    /// human answer, and nothing else would be listening for it. `Err` is the user leaving or
    /// interrupting.
    pub(crate) async fn expand_serving(
        &mut self,
        runtime: &AgentRuntime,
        name: &str,
        argument: &str,
    ) -> Result<Result<String, RuntimeError>, TurnEnd> {
        let mut expansion = Box::pin(runtime.expand_skill(name, argument));
        let end = match self
            .serve_while(runtime, &mut expansion, Serve::Prompts)
            .await
        {
            Ok(result) => return Ok(result),
            Err(end) => end,
        };
        drop(expansion);
        self.app.deny_pending();
        self.drain_prompts();
        Err(end)
    }

    /// Runs `text` as a turn, then whatever the human queued during it that the agent did not
    /// receive: sent as the next turn after an answer, handed back to the input otherwise.
    /// `expanded` is the prompt a skill expanded `text` into, already recorded as the user's
    /// message.
    pub(crate) async fn converse(
        &mut self,
        runtime: &mut AgentRuntime,
        mut text: String,
        mut expanded: Option<String>,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        loop {
            // A prompt with an expansion was recorded as the human's message before this point.
            runtime.begin_turn(&text, expanded.is_some());
            self.app.transcript.user(text.clone());
            // A prompt from the user resumes a paused goal.
            runtime.goal_user_prompt();
            let end = match self.refresh_native_search(runtime) {
                Some(end) => end,
                None => match expanded.take() {
                    Some(prompt) => self.turn(runtime, prompt, None).await,
                    None => self.turn(runtime, text.clone(), Some(&text)).await,
                },
            };
            if matches!(end, TurnEnd::InputClosed) {
                return Break(());
            }
            match end_of_turn(&end, runtime.steering.retract()) {
                EndOfTurn::Submit(next) => text = next,
                EndOfTurn::RestoreToInput(back) => {
                    self.app.restore_to_input(&back);
                    return Continue(());
                }
                EndOfTurn::Nothing => return Continue(()),
            }
        }
    }

    /// Runs one user message while still serving keys and prompts.
    ///
    /// `human` is what the user typed, recorded in the ledger as theirs; `None` is a turn the
    /// harness started, whose prompt is not.
    async fn turn(&mut self, runtime: &AgentRuntime, text: String, human: Option<&str>) -> TurnEnd {
        if let Some(typed) = human {
            runtime.append(Record::UserMessage {
                text: typed.to_string(),
            });
        }
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
            runtime.compact_if_needed().await;
            let ids = runtime.models.main.wire_ids();
            let (model, provider) = (ids.model, ids.provider);
            run_turn(
                TurnParams {
                    agent: &runtime.agent,
                    state: &runtime.state,
                    prompt,
                    max_turns: DEFAULT_MAX_TURNS,
                    priced: runtime.models.main.catalog_billed(),
                    model,
                    provider,
                    native_search: runtime.native_search_active(),
                },
                &mut on_event,
            )
            .await
        });
        let tree = runtime.state.agents().clone();
        let end = loop {
            let changed = tree.changed().notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.draw(runtime);
            tokio::select! {
                biased;
                _ = &mut changed => {}
                Some(event) = seen.recv() => {
                    self.on_event(runtime, event);
                    // One redraw for everything already queued, not one per streamed delta.
                    while let Ok(event) = seen.try_recv() {
                        self.on_event(runtime, event);
                    }
                    self.show_delivered(runtime);
                }
                outcome = &mut run => break TurnEnd::Finished(outcome),
                input = self.input.recv() => match input {
                    None => break TurnEnd::InputClosed,
                    Some(input) => match self.app.handle_input(input) {
                        Action::Interrupt | Action::Quit => break TurnEnd::Interrupted,
                        Action::CycleMode => self.cycle_mode(runtime),
                        Action::RetryDenied(index) => self.retry_denied(runtime, index),
                        Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                        Action::KillAgent(id) => self.kill_agent(runtime, &id),
                        Action::PickModel(purpose, reference) => {
                            self.pending_pick = Some((purpose, reference));
                        }
                        Action::ResumeSession(id) => self.pending_resume = Some(id),
                        Action::Providers(request) => self.queue_provider(request),
                        Action::Config(change) => self.pending_config.push_back(change),
                        Action::OpenPicker(slot) => {
                            self.pending_config.push_back(ConfigKey::OpenPicker(slot));
                        }
                        Action::Queue(text) => {
                            self.remember(&text);
                            runtime.steering.queue(text);
                        }
                        Action::EditQueued => self.edit_queued(runtime),
                        // A turn or a sign-in is under way, so a rewind cannot start: the picker closes.
                        Action::SelectRewind(_) => self.app.rewind = None,
                        Action::OpenRewind | Action::Rewind { .. } => {}
                        Action::Server(_)
                        | Action::None
                        | Action::Submit(_)
                        | Action::Shell(_)
                        | Action::CancelWakeup => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
            }
        };
        drop(run);
        while let Ok(event) = seen.try_recv() {
            self.on_event(runtime, event);
        }
        self.show_delivered(runtime);
        self.finish_turn(runtime, &end).await;
        self.goal_after_turn(runtime, &end);
        runtime.refresh_files();
        end
    }

    /// Shows queued messages where the agent received them.
    fn show_delivered(&mut self, runtime: &AgentRuntime) {
        for text in runtime.steering.take_delivered() {
            // The user spoke into the turn, so a paused goal resumes.
            runtime.goal_user_prompt();
            self.app.transcript.user(text);
        }
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
        let todos_changed = matches!(
            &event,
            SessionEvent::ToolResult { tool, .. } if tool == "todo_write"
        );
        self.app.on_event(event);
        if todos_changed {
            self.app.todos.set_items(runtime.session.todos().root());
        }
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
        self.settle_turn(end);
        if matches!(end, TurnEnd::Interrupted) {
            // Decisions the cut-short calls already made belong to this turn, not the next.
            let mut feed = runtime.state.feed().await;
            while feed.try_next().is_some() {}
            while feed.try_next_preview().is_some() {}
        }
    }

    /// Leaves nothing of the turn open: a call without a result would otherwise hold the live
    /// area, and every line behind it, until the session ends.
    pub(crate) fn settle_turn(&mut self, end: &TurnEnd) {
        self.app.running = false;
        self.app.deny_pending();
        self.drain_prompts();
        self.app.transcript.interrupt_pending();
        match end {
            TurnEnd::Finished(TurnOutcome::Failed(message)) => {
                self.app.transcript.error(message.clone());
            }
            TurnEnd::Finished(TurnOutcome::MaxTurns(message)) => {
                self.app.transcript.notice(message.clone());
            }
            TurnEnd::Finished(_) | TurnEnd::InputClosed => {}
            TurnEnd::Interrupted => self.app.transcript.notice("interrupted"),
        }
    }
}
