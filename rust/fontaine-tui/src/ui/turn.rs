use std::collections::VecDeque;
use std::path::Path;

use crate::config_panel::ConfigKey;
use crate::transcript::Transcript;
use crate::{Action, App, Ui, parse_command};
use fontaine_core::RuntimeError;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::Decision;
use fontaine_core::gate::AgentViewEvent;
use fontaine_core::goal::Continue;
use fontaine_core::ledger::Record;
use fontaine_core::schedule::{Fired, Kind};
use fontaine_core::steering::Queued;
use fontaine_core::tier::Tier;
use fontaine_core::turn::{Flow, SessionEvent, TurnOutcome, TurnParams, run_turn};
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

/// What becomes of input typed during a turn that the agent never received.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EndOfTurn {
    /// The agent answered without another tool result: run what was queued, in order.
    Submit(Vec<Submission>),
    /// The turn did not complete: give it all back so nothing the human typed is sent unseen.
    RestoreToInput(String),
    Nothing,
}

/// One unit of queued replay: what a retracted entry becomes once the turn ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Submission {
    /// Consecutive queued messages joined with a blank line, as a plain message reads today.
    Message(String),
    Skill {
        raw: String,
        name: String,
        argument: String,
    },
    /// A built-in command as typed; re-parsed and executed through the idle dispatch.
    Command(String),
    /// A `!` shell command as typed, prefix included; run through the idle `!` path.
    Shell(String),
    /// A turn ready to run: the text the idle path displays and checkpoints (not necessarily
    /// the raw command), plus its expanded prompt. Its user-message record has already been
    /// appended, as at idle. It never waits behind another submission: a dispatcher hands it
    /// back to the loop, which front-pushes it.
    Expanded { text: String, prompt: String },
}

impl Submission {
    /// The text as typed, used for the queue display's restore and the history.
    pub(crate) fn raw(&self) -> &str {
        match self {
            Submission::Message(text) => text,
            Submission::Skill { raw, .. } => raw,
            Submission::Command(raw) => raw,
            Submission::Shell(raw) => raw,
            Submission::Expanded { text, .. } => text,
        }
    }
}

/// What dispatching one queued command came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    /// The command did its work; the next submission runs.
    Done,
    /// The command opened a picker or dialog; replay suspends until it closes.
    AwaitingModal,
    /// The command prepared a turn, which front-pushes and runs next.
    Turn(Submission),
}

/// What expanding a skill invocation came to.
pub(crate) enum SkillRun {
    /// The expansion to run as the invocation's turn.
    Prompt(String),
    /// An error or an interrupt: the notices are shown and the raw is spent.
    Spent,
}

/// A dispatcher hand-off runs next, ahead of everything already pending, so nothing executes
/// between a command's preparation and its turn.
pub(crate) fn front_push(pending: &mut VecDeque<Submission>, submission: Submission) {
    pending.push_front(submission);
}

/// The text a replay interruption restores to the input: the older pending submissions' raws
/// front-to-back, then the newly retracted raws. Restoring prepends the unsubmitted draft, so
/// it stays last.
pub(crate) fn restore_block(pending: &VecDeque<Submission>, retracted: &str) -> String {
    let mut raws: Vec<&str> = pending.iter().map(Submission::raw).collect();
    if !retracted.is_empty() {
        raws.push(retracted);
    }
    raws.join("\n\n")
}

pub(crate) fn end_of_turn(end: &TurnEnd, retracted: Vec<Queued>) -> EndOfTurn {
    if retracted.is_empty() {
        return EndOfTurn::Nothing;
    }
    match end {
        TurnEnd::Finished(TurnOutcome::Answer(_)) => EndOfTurn::Submit(group(retracted)),
        TurnEnd::Finished(_) | TurnEnd::Interrupted => EndOfTurn::RestoreToInput(
            retracted
                .iter()
                .map(|entry| entry.raw())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        TurnEnd::InputClosed => EndOfTurn::Nothing,
    }
}

/// Groups retracted entries into submissions: consecutive messages join into one, and each
/// skill, command or shell line is a submission of its own, keeping the typed order.
fn group(entries: Vec<Queued>) -> Vec<Submission> {
    let mut submissions: Vec<Submission> = Vec::new();
    for entry in entries {
        match entry {
            Queued::Message(text) => match submissions.last_mut() {
                Some(Submission::Message(joined)) => {
                    joined.push_str("\n\n");
                    joined.push_str(&text);
                }
                _ => submissions.push(Submission::Message(text)),
            },
            Queued::Skill {
                raw,
                name,
                argument,
            } => submissions.push(Submission::Skill {
                raw,
                name,
                argument,
            }),
            Queued::Command(raw) => submissions.push(Submission::Command(raw)),
            Queued::Shell(raw) => submissions.push(Submission::Shell(raw)),
        }
    }
    submissions
}

/// Shows an event in `transcript`. A call the guardian refused is shown with its rule label and
/// its own words rather than the note the agent is given.
fn apply_event(transcript: &mut Transcript, runtime: &AgentRuntime, event: SessionEvent) {
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
    transcript.on_event(event);
    if let Some(call) = denial {
        transcript.guardian_denied(
            call.tool_use_id.as_deref().unwrap_or(""),
            &call.tool,
            &call.rule,
            &call.explanation,
        );
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

    /// A turn on what the subagents reported. It is not the user speaking, so it is not
    /// recorded as a user message.
    pub(crate) async fn notification_turn(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
    ) -> std::ops::ControlFlow<()> {
        let jobs = runtime.state.agents().has_job_notifications()
            && !runtime.state.agents().has_agent_notifications();
        let Some(prompt) = runtime.notification_prompt().await else {
            return std::ops::ControlFlow::Continue(());
        };
        if let Some(end) = self.refresh_native_search(runtime) {
            return self.harness_turn_tail(runtime, launch_dir, end).await;
        }
        self.app.transcript.notice(if jobs {
            "a background job reported back"
        } else {
            "an agent reported back"
        });
        let end = self.turn(runtime, prompt, None).await;
        self.harness_turn_tail(runtime, launch_dir, end).await
    }

    /// What follows a turn the harness started (a notification, a scheduled task or a goal
    /// continuation): a picker or provider action taken during it applies first, then what the
    /// human queued while it ran executes in order, or returns to the input.
    async fn harness_turn_tail(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        end: TurnEnd,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        if matches!(end, TurnEnd::InputClosed) {
            return Break(());
        }
        if self.drain_deferred(runtime).await.is_break() {
            return Break(());
        }
        match end_of_turn(&end, runtime.steering.retract()) {
            EndOfTurn::Submit(submissions) => {
                self.converse(runtime, launch_dir, VecDeque::from(submissions))
                    .await
            }
            EndOfTurn::RestoreToInput(back) => {
                self.app.restore_to_input(&back);
                Continue(())
            }
            EndOfTurn::Nothing => Continue(()),
        }
    }

    /// A turn on a scheduled task that came due. Like a subagent's report it is not
    /// the user speaking, so it is not recorded as a user message; unlike one, what the human
    /// queued while it ran is sent as the next turn or handed back to the input.
    pub(crate) async fn scheduled_turn(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        fired: Fired,
    ) -> std::ops::ControlFlow<()> {
        let prompt = runtime.scheduled_prompt(&fired).await;
        self.app
            .transcript
            .notice(format!("scheduled: {}", fired.job.id));
        let end = match self.refresh_native_search(runtime) {
            Some(end) => end,
            None => self.turn(runtime, prompt, None).await,
        };
        if matches!(end, TurnEnd::InputClosed) {
            return std::ops::ControlFlow::Break(());
        }
        if matches!(fired.job.kind, Kind::Wakeup { .. }) {
            self.end_of_iteration(runtime);
        }
        self.harness_turn_tail(runtime, launch_dir, end).await
    }

    /// A turn on a goal the evaluator found not met yet. A harness note, never the user
    /// speaking; what the human queued meanwhile is handled as after a scheduled turn.
    pub(crate) async fn goal_turn(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        next: Continue,
    ) -> std::ops::ControlFlow<()> {
        let prompt = runtime.goal_prompt(&next).await;
        let end = match self.refresh_native_search(runtime) {
            Some(end) => end,
            None => self.turn(runtime, prompt, None).await,
        };
        self.harness_turn_tail(runtime, launch_dir, end).await
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
                        Action::MessageAgent { id, text } => self.message_agent(runtime, id, text),
                        Action::KillJob(id) => self.kill_job(runtime, &id),
                        Action::PickModel(purpose, reference) => {
                            self.pending_pick = Some((purpose, reference));
                        }
                        Action::ResumeSession(id) => self.pending_resume = Some(id),
                        Action::Providers(request) => self.queue_provider(request),
                        Action::Config(change) => self.pending_config.push_back(change),
                        Action::OpenPicker(slot) => {
                            self.pending_config.push_back(ConfigKey::OpenPicker(slot));
                        }
                        Action::Queue(entry) if queueing => {
                            self.remember(entry.raw());
                            runtime.steering.queue(entry);
                        }
                        Action::EditQueued if queueing => self.edit_queued(runtime),
                        Action::Command { command, raw } => {
                            self.remember(&raw);
                            self.run_immediate(runtime, &command);
                        }
                        _ => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
                Some(first) = self.agent_events.recv() => self.on_agent_events(runtime, first),
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

    /// Expands a skill invocation exactly as the idle path does: the raw line is recorded as
    /// the user's message before the skill's commands run, the expansion serves approvals, and
    /// an error or interrupt shows the idle notices and spends the raw. `Err` is the user
    /// leaving.
    pub(crate) async fn expand_skill_command(
        &mut self,
        runtime: &mut AgentRuntime,
        raw: &str,
        name: &str,
        argument: &str,
    ) -> std::ops::ControlFlow<(), SkillRun> {
        use std::ops::ControlFlow::{Break, Continue};
        // Esc during the expansion interrupts it, at idle and queued alike.
        self.app.running = true;
        // Recorded before the skill's commands run so the guardian judging them has
        // this turn's intent in the ledger.
        runtime.append(Record::UserMessage {
            text: raw.to_string(),
            agent: None,
        });
        let result = self.expand_serving(runtime, name, argument).await;
        self.app.running = false;
        match result {
            Ok(Ok(prompt)) => Continue(SkillRun::Prompt(prompt)),
            Ok(Err(e)) => {
                self.app.deny_pending();
                self.drain_prompts();
                self.app.transcript.error(format!(
                    "/{name}: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                Continue(SkillRun::Spent)
            }
            Err(TurnEnd::InputClosed) => Break(()),
            Err(_) => {
                self.app.transcript.notice("interrupted");
                Continue(SkillRun::Spent)
            }
        }
    }

    /// Runs the submissions in `pending`, one at a time, in order. A turn submission runs the
    /// way the idle path runs it; a skill is expanded against live state as it pops; a command
    /// dispatches through the idle handlers, and one that opens a picker or dialog suspends
    /// replay until it closes; a `!` line runs as a shell run. While older submissions wait,
    /// steering delivery is held, so nothing newly typed overtakes the backlog: it retracts and
    /// back-extends in typed order instead.
    pub(crate) async fn converse(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        mut pending: VecDeque<Submission>,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        while let Some(submission) = pending.pop_front() {
            runtime.steering.set_delivery_hold(!pending.is_empty());
            let end = match submission {
                Submission::Message(text) => {
                    self.app.transcript.user(text.clone());
                    // A prompt from the user resumes a paused goal.
                    runtime.goal_user_prompt();
                    match self.refresh_native_search(runtime) {
                        Some(end) => end,
                        None => {
                            runtime.begin_turn(&text, false);
                            self.turn(runtime, text.clone(), Some(&text)).await
                        }
                    }
                }
                Submission::Expanded { text, prompt } => {
                    self.app.transcript.user(text.clone());
                    // A prompt from the user resumes a paused goal.
                    runtime.goal_user_prompt();
                    match self.refresh_native_search(runtime) {
                        Some(end) => end,
                        None => {
                            // Recorded as the human's message before this point.
                            runtime.begin_turn(&text, true);
                            self.turn(runtime, prompt, None).await
                        }
                    }
                }
                Submission::Skill {
                    raw,
                    name,
                    argument,
                } => {
                    match self
                        .expand_skill_command(runtime, &raw, &name, &argument)
                        .await
                    {
                        Break(()) => {
                            runtime.steering.set_delivery_hold(false);
                            return Break(());
                        }
                        Continue(SkillRun::Prompt(prompt)) => {
                            front_push(&mut pending, Submission::Expanded { text: raw, prompt });
                        }
                        Continue(SkillRun::Spent) => {}
                    }
                    continue;
                }
                Submission::Command(raw) => {
                    let Some(command) = parse_command(&raw) else {
                        continue;
                    };
                    match self
                        .dispatch_command(runtime, launch_dir, command, &raw, !pending.is_empty())
                        .await
                    {
                        Break(()) => {
                            runtime.steering.set_delivery_hold(false);
                            return Break(());
                        }
                        Continue(CommandOutcome::Done) => {}
                        Continue(CommandOutcome::AwaitingModal) => {
                            if self.serve_modal(runtime, &pending).await.is_break() {
                                runtime.steering.set_delivery_hold(false);
                                return Break(());
                            }
                        }
                        Continue(CommandOutcome::Turn(sub)) => front_push(&mut pending, sub),
                    }
                    continue;
                }
                Submission::Shell(raw) => {
                    let command = raw.strip_prefix('!').map(str::trim).unwrap_or_default();
                    if self.shell(runtime, command.to_string()).await.is_break() {
                        runtime.steering.set_delivery_hold(false);
                        return Break(());
                    }
                    continue;
                }
            };
            if matches!(end, TurnEnd::InputClosed) {
                runtime.steering.set_delivery_hold(false);
                return Break(());
            }
            match end_of_turn(&end, runtime.steering.retract()) {
                EndOfTurn::Submit(submissions) => pending.extend(submissions),
                EndOfTurn::RestoreToInput(back) => {
                    runtime.steering.set_delivery_hold(false);
                    let block = restore_block(&pending, &back);
                    self.app.restore_to_input(&block);
                    return Continue(());
                }
                EndOfTurn::Nothing => {}
            }
        }
        runtime.steering.set_delivery_hold(false);
        Continue(())
    }

    /// Serves a picker or dialog a queued command opened, with the same handlers the idle loop
    /// gives it, applying choices directly; replay stays suspended until it closes. A session
    /// switch is refused while older submissions wait, so typed input is never lost to it.
    async fn serve_modal(
        &mut self,
        runtime: &mut AgentRuntime,
        pending: &VecDeque<Submission>,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        while self.app.covered() {
            self.draw(runtime);
            let action = tokio::select! {
                input = self.input.recv() => match input {
                    None => return Break(()),
                    Some(input) => self.app.handle_input(input),
                },
                Some(prompt) = self.prompts.recv() => {
                    self.on_prompt(Some(runtime), prompt);
                    Action::None
                }
                Some(first) = self.agent_events.recv() => {
                    self.on_agent_events(runtime, first);
                    Action::None
                }
            };
            match action {
                Action::PickModel(purpose, reference) => {
                    self.apply_pick(runtime, purpose, &reference).await
                }
                Action::ResumeSession(id) => {
                    if !pending.is_empty() {
                        self.refuse_switch("/resume");
                        return Continue(());
                    }
                    return self.switch_to(runtime, &id);
                }
                Action::Providers(request) => self.provider_request(runtime, request).await,
                Action::Config(change) => self.config_change(runtime, change),
                Action::OpenPicker(slot) => self.open_config_picker(runtime, slot).await,
                Action::SelectRewind(turn) => self.offer_rewind(runtime, turn),
                Action::Rewind {
                    plan,
                    what,
                    overwrite_changed,
                } => {
                    self.run_rewind(runtime, plan, what, overwrite_changed)
                        .await
                }
                Action::OpenRewind => self.open_rewind(runtime),
                Action::Interrupt | Action::Quit => return Break(()),
                Action::CycleMode => self.cycle_mode(runtime),
                Action::RetryDenied(index) => self.retry_denied(runtime, index),
                Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                Action::KillAgent(id) => self.kill_agent(runtime, &id),
                Action::MessageAgent { id, text } => self.message_agent(runtime, id, text),
                Action::KillJob(id) => self.kill_job(runtime, &id),
                Action::None
                | Action::Submit(_)
                | Action::Queue(_)
                | Action::Command { .. }
                | Action::Shell(_)
                | Action::EditQueued
                | Action::Server(_)
                | Action::GuardianOff(_)
                | Action::SetupReview(_)
                | Action::CancelWakeup => {}
            }
        }
        Continue(())
    }

    /// The notice a queued `/resume` or `/branch` gets while older submissions wait: the
    /// switch is refused, and replay carries on in this session.
    pub(crate) fn refuse_switch(&mut self, command: &str) {
        self.app.transcript.error(format!(
            "{command} is refused while queued input still waits to run; switching sessions would lose it"
        ));
    }

    /// Runs one user message while still serving keys and prompts.
    ///
    /// `human` is what the user typed, recorded in the ledger as theirs; `None` is a turn the
    /// harness started, whose prompt is not.
    async fn turn(&mut self, runtime: &AgentRuntime, text: String, human: Option<&str>) -> TurnEnd {
        // A refusal left by a step that reset `running` without settling would show here, with
        // no input event to clear it.
        self.app.refusal = None;
        if let Some(typed) = human {
            runtime.append(Record::UserMessage {
                text: typed.to_string(),
                agent: None,
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
                    max_turns: None,
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
            let display = tree.display_changed().notified();
            tokio::pin!(display);
            display.as_mut().enable();
            self.draw(runtime);
            let has_list = self.app.agent_list.counts_time();
            tokio::select! {
                biased;
                _ = &mut changed => {}
                _ = &mut display => {}
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
                        Action::MessageAgent { id, text } => self.message_agent(runtime, id, text),
                        Action::KillJob(id) => self.kill_job(runtime, &id),
                        Action::PickModel(purpose, reference) => {
                            self.pending_pick = Some((purpose, reference));
                        }
                        Action::ResumeSession(id) => self.pending_resume = Some(id),
                        Action::Providers(request) => self.queue_provider(request),
                        Action::Config(change) => self.pending_config.push_back(change),
                        Action::OpenPicker(slot) => {
                            self.pending_config.push_back(ConfigKey::OpenPicker(slot));
                        }
                        Action::Queue(entry) => {
                            self.remember(entry.raw());
                            runtime.steering.queue(entry);
                        }
                        Action::EditQueued => self.edit_queued(runtime),
                        Action::Command { command, raw } => {
                            self.remember(&raw);
                            self.run_immediate(runtime, &command);
                        }
                        // A turn or a sign-in is under way, so a rewind cannot start: the picker closes.
                        Action::SelectRewind(_) => self.app.rewind = None,
                        Action::OpenRewind | Action::Rewind { .. } => {}
                        Action::Server(_)
                        | Action::None
                        | Action::Submit(_)
                        | Action::Shell(_)
                        | Action::GuardianOff(_)
                        | Action::SetupReview(_)
                        | Action::CancelWakeup => {}
                    },
                },
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
                Some(first) = self.agent_events.recv() => self.on_agent_events(runtime, first),
                () = async {
                    if has_list {
                        tokio::time::sleep(super::LIST_TICK).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
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

    /// Shows an event of the main conversation.
    fn on_event(&mut self, runtime: &AgentRuntime, event: SessionEvent) {
        let todos_changed = matches!(
            &event,
            SessionEvent::ToolResult { tool, .. } if tool == "todo_write"
        );
        apply_event(&mut self.app.transcript, runtime, event);
        if todos_changed {
            self.app.todos.set_items(runtime.session.todos().root());
        }
    }

    /// Shows what subagents did: the event that woke the loop and everything already queued
    /// behind it, for one redraw.
    pub(crate) fn on_agent_events(
        &mut self,
        runtime: &AgentRuntime,
        first: (String, AgentViewEvent),
    ) {
        self.on_agent_event(runtime, first);
        while let Ok(next) = self.agent_events.try_recv() {
            self.on_agent_event(runtime, next);
        }
    }

    fn on_agent_event(&mut self, runtime: &AgentRuntime, (agent, event): (String, AgentViewEvent)) {
        // What the human typed to the agent reached it before this event happened.
        self.show_delivered_to(runtime, &agent);
        let transcript = &mut self.app.view_of(agent).transcript;
        match event {
            AgentViewEvent::Prompt(text) => transcript.user(text),
            AgentViewEvent::Event(event) => apply_event(transcript, runtime, event),
        }
    }

    /// Moves messages the agents have read from the queue to where each agent's transcript
    /// shows them.
    pub(crate) fn show_delivered_to_agents(&mut self, runtime: &AgentRuntime) {
        let ids: Vec<String> = self.app.agent_views.keys().cloned().collect();
        for id in ids {
            self.show_delivered_to(runtime, &id);
        }
    }

    fn show_delivered_to(&mut self, runtime: &AgentRuntime, agent: &str) {
        for text in runtime.steering.take_delivered_for(agent) {
            self.app.view_of(agent.to_string()).transcript.user(text);
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
        self.app.refusal = None;
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
