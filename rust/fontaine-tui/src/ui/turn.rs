use std::collections::VecDeque;
use std::path::Path;

use crate::config_panel::ConfigKey;
use crate::input::{MAX_ATTACHMENT_BYTES, PendingAttachment};
use crate::transcript::Transcript;
use crate::{Action, App, Command, Ui, parse_command};
use fontaine_core::RuntimeError;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::Decision;
use fontaine_core::config::HookEvent;
use fontaine_core::gate::AgentViewEvent;
use fontaine_core::goal::Continue;
use fontaine_core::ledger::{MessageOrigin, Record};
use fontaine_core::model_roles::Tried;
use fontaine_core::quota::UsageRole;
use fontaine_core::schedule::{Fired, Kind};
use fontaine_core::steering::Queued;
use fontaine_core::tier::Tier;
use fontaine_core::turn::{
    Attachment, AttachmentMedia, Flow, RETRY_NOTE, SessionEvent, TurnFailure, TurnOutcome,
    TurnParams, run_turn, user_message, wait_out_usage_limit,
};
use fontaine_core::turn_recovery::{Fallback, Unsettled};
use fontaine_core::user_hooks::PromptGate;
use tokio::sync::mpsc::unbounded_channel;

/// How a turn ended from the interface's side.
pub(crate) enum TurnEnd {
    Finished(TurnOutcome),
    Interrupted,
    /// The AFK temperature ceiling stopped the session and the turn was dropped.
    Stopped,
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
#[derive(Debug, PartialEq)]
pub(crate) enum EndOfTurn {
    /// The agent answered without another tool result: run what was queued, in order.
    Submit(Vec<Submission>),
    /// The turn did not complete: give it all back so nothing the human typed is sent unseen.
    RestoreToInput(String),
    Nothing,
}

/// One unit of queued replay: what a retracted entry becomes once the turn ends.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Submission {
    /// Consecutive queued messages joined with a blank line, as a plain message reads today.
    Message(String),
    /// A plain message with inline attachments: a clipboard image or a dropped PDF.
    Attached {
        text: String,
        attachments: Vec<PendingAttachment>,
    },
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
            Submission::Attached { text, .. } => text,
            Submission::Skill { raw, .. } => raw,
            Submission::Command(raw) => raw,
            Submission::Shell(raw) => raw,
            Submission::Expanded { text, .. } => text,
        }
    }
}

/// What dispatching one queued command came to.
#[derive(Debug, PartialEq)]
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

/// Reads an attached file into what a turn carries. A file that vanished or cannot be read
/// since it was attached is an error the transcript names, not a failed turn.
fn into_attachment(pending: PendingAttachment) -> Result<Attachment, String> {
    use crate::input::PendingMedia;

    // Stat before opening: metadata does not block on a FIFO or device, but
    // reading one would, and the recorded size cannot be trusted for the cap.
    let metadata = std::fs::metadata(&pending.path).map_err(|e| {
        format!(
            "{} could not be read and was not sent: {e}",
            pending.path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "{} is not a regular file and was not sent",
            pending.path.display()
        ));
    }
    if metadata.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!(
            "{} is {} and was not sent: attachments are capped at {}",
            pending.label,
            fontaine_tools::notebook::human_size(metadata.len()),
            fontaine_tools::notebook::human_size(MAX_ATTACHMENT_BYTES),
        ));
    }
    let bytes = std::fs::read(&pending.path).map_err(|e| {
        format!(
            "{} could not be read and was not sent: {e}",
            pending.path.display()
        )
    })?;
    let media = match pending.media {
        PendingMedia::Image(media) => AttachmentMedia::Image(media),
        PendingMedia::Pdf => AttachmentMedia::Pdf,
    };
    Ok(Attachment {
        label: pending.label,
        bytes,
        media,
    })
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
        TurnEnd::Finished(_) | TurnEnd::Interrupted | TurnEnd::Stopped => {
            EndOfTurn::RestoreToInput(
                retracted
                    .iter()
                    .map(|entry| entry.raw())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            )
        }
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

fn push_verdict(app: &mut App, event: &SessionEvent) {
    if let SessionEvent::Decision(record) = event {
        app.push_verdict(record);
    }
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// Brings the agent in line with the mode and rules now in force, which a mode switch or a
    /// session rule may have changed since it was built: the provider's search tool and the AFK
    /// prompt guidance.
    ///
    /// When the agent cannot be rebuilt to drop a search tool the gate now withholds, the turn
    /// must not run: this returns how it ends instead.
    fn refresh_before_turn(&mut self, runtime: &mut AgentRuntime) -> Option<TurnEnd> {
        let e = runtime.refresh_before_turn().err()?;
        let message = format!(
            "the provider's web search could not be turned off, so the turn did not run: {}",
            fontaine_tools::error_chain::chain(&e)
        );
        self.app.transcript.error(message.clone());
        Some(TurnEnd::Finished(TurnOutcome::Failed(message.into())))
    }

    /// Folds a turn an earlier fold failed to keep into the conversation, so it sits before
    /// the turn about to run and its rewind checkpoint rather than after them.
    async fn keep_leftover_turn(runtime: &AgentRuntime) {
        runtime
            .keep_unsettled_turn(Unsettled::SessionEnded, None)
            .await;
    }

    /// A turn on what the subagents reported. It is not the user speaking, so it is not
    /// recorded as a user message.
    pub(crate) async fn notification_turn(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
    ) -> std::ops::ControlFlow<()> {
        if self.quitting() {
            return std::ops::ControlFlow::Break(());
        }
        let kinds = runtime.state.agents().root_notification_kinds();
        let reminders = !kinds.is_empty()
            && kinds
                .iter()
                .all(|kind| *kind == fontaine_agents::tree::NotificationKind::Reminder);
        let Some(prompt) = runtime.notification_prompt().await else {
            return std::ops::ControlFlow::Continue(());
        };
        if let Some(end) = self.refresh_before_turn(runtime) {
            return self.harness_turn_tail(runtime, launch_dir, end).await;
        }
        Self::keep_leftover_turn(runtime).await;
        // The job label wins a mixed inbox, as the prompt's own note says jobs first.
        self.app.transcript.notice(
            if kinds.contains(&fontaine_agents::tree::NotificationKind::Job) {
                "a background job reported back"
            } else if reminders {
                "tasks on your board are waiting for agents"
            } else {
                "an agent reported back"
            },
        );
        self.app.automatic_next = true;
        let end = self
            .turn(runtime, prompt, None, MessageOrigin::Other, Vec::new())
            .await;
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
                self.converse(
                    runtime,
                    launch_dir,
                    VecDeque::from(submissions),
                    MessageOrigin::Other,
                )
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
        if self.quitting() {
            return std::ops::ControlFlow::Break(());
        }
        let prompt = runtime.scheduled_prompt(&fired).await;
        self.app
            .transcript
            .notice(format!("scheduled: {}", fired.job.id));
        let end = match self.refresh_before_turn(runtime) {
            Some(end) => end,
            None => {
                Self::keep_leftover_turn(runtime).await;
                self.app.automatic_next = true;
                self.turn(runtime, prompt, None, MessageOrigin::Other, Vec::new())
                    .await
            }
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
        if self.quitting() {
            return std::ops::ControlFlow::Break(());
        }
        let prompt = runtime.goal_prompt(&next).await;
        let end = match self.refresh_before_turn(runtime) {
            Some(end) => end,
            None => {
                Self::keep_leftover_turn(runtime).await;
                self.app.automatic_next = true;
                self.turn(runtime, prompt, None, MessageOrigin::Other, Vec::new())
                    .await
            }
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
            let tick = super::soonest_tick(&self.app, &[]);
            tokio::select! {
                biased;
                // Input before the work: a quit already waiting must not lose a same-poll race.
                input = self.input.recv() => match input {
                    None => return Err(TurnEnd::InputClosed),
                    Some(input) => match self.act_on(runtime, input) {
                        Action::Quit => return Err(TurnEnd::InputClosed),
                        Action::Interrupt => return Err(TurnEnd::Interrupted),
                        Action::CycleMode => self.cycle_mode(runtime, true),
                        Action::RetryDenied(index) if retry => self.retry_denied(runtime, index),
                        Action::Guardian(answer) => self.answer_unavailable(runtime, answer).await,
                        Action::UsagePause(answer) => self.answer_usage_pause(runtime, answer).await,
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
                        Action::AttachClipboard => self.attach_clipboard(runtime).await,
                        Action::EditQueued if queueing => self.edit_queued(runtime),
                        Action::Command { command, raw } => {
                            self.remember(&raw);
                            self.run_immediate(runtime, &command);
                            if self.quitting() {
                                return Err(TurnEnd::InputClosed);
                            }
                        }
                        _ => {}
                    },
                },
                result = &mut *fut => return Ok(result),
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
                Some(first) = self.agent_events.recv() => self.on_agent_events(runtime, first),
                () = async {
                    match tick {
                        Some(wait) => tokio::time::sleep(wait).await,
                        None => std::future::pending().await,
                    }
                } => {}
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
        match self.prompt_passes(runtime, raw).await {
            Break(()) => return Break(()),
            Continue(false) => return Continue(SkillRun::Spent),
            Continue(true) => {}
        }
        // Esc during the expansion interrupts it, at idle and queued alike.
        self.app.set_running(true);
        // Recorded before the skill's commands run so the guardian judging them has
        // this turn's intent in the ledger.
        runtime.append(Record::UserMessage {
            origin: Default::default(),
            text: raw.to_string(),
            attachments: Vec::new(),
            agent: None,
        });
        let result = self.expand_serving(runtime, name, argument).await;
        self.app.set_running(false);
        match result {
            Ok(Ok(prompt)) => Continue(SkillRun::Prompt(prompt)),
            Ok(Err(e)) => {
                self.app.deny_pending();
                self.drain_prompts();
                self.app.transcript.error(format!(
                    "/{name}: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                runtime.clear_prompt_context();
                Continue(SkillRun::Spent)
            }
            Err(TurnEnd::InputClosed) => Break(()),
            Err(_) => {
                self.app.transcript.notice("interrupted");
                runtime.clear_prompt_context();
                Continue(SkillRun::Spent)
            }
        }
    }

    /// `/mcp__<server>__<prompt> arguments`: fetches the prompt's text and hands it back as
    /// the invocation's turn, mirroring `expand_skill_command`. The raw line is recorded as
    /// the user's message before the fetch, so the guardian judging the turn sees the
    /// invocation; a failure shows a transcript error and starts no turn.
    pub(crate) async fn run_mcp_prompt_command(
        &mut self,
        runtime: &mut AgentRuntime,
        raw: &str,
        name: &str,
        argument: &str,
    ) -> std::ops::ControlFlow<(), CommandOutcome> {
        use std::ops::ControlFlow::{Break, Continue};
        if self.refuse_if_stopped(runtime, raw) {
            return Continue(CommandOutcome::Done);
        }
        match self.prompt_passes(runtime, raw).await {
            Break(()) => return Break(()),
            Continue(false) => return Continue(CommandOutcome::Done),
            Continue(true) => {}
        }
        self.app.set_running(true);
        // Recorded before the fetch so the guardian judging the turn has this intent.
        runtime.append(Record::UserMessage {
            origin: Default::default(),
            text: raw.to_string(),
            attachments: Vec::new(),
            agent: None,
        });
        let mut fetch = Box::pin(runtime.run_mcp_prompt(name, argument));
        let result = self.serve_while(runtime, &mut fetch, Serve::Prompts).await;
        drop(fetch);
        self.app.set_running(false);
        match result {
            Ok(Ok(prompt)) => Continue(CommandOutcome::Turn(Submission::Expanded {
                text: raw.to_string(),
                prompt,
            })),
            Ok(Err(e)) => {
                self.app.deny_pending();
                self.drain_prompts();
                self.app.transcript.error(format!(
                    "/{name}: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                runtime.clear_prompt_context();
                Continue(CommandOutcome::Done)
            }
            Err(end) => self.end_interrupted_prompt_fetch(runtime, end),
        }
    }

    /// Settles a prompt fetch the serve loop cut short. The open and queued prompts belong
    /// to calls that no longer exist, so they are denied and drained before the end is
    /// handled, mirroring `expand_serving`.
    pub(crate) fn end_interrupted_prompt_fetch(
        &mut self,
        runtime: &AgentRuntime,
        end: TurnEnd,
    ) -> std::ops::ControlFlow<(), CommandOutcome> {
        use std::ops::ControlFlow::{Break, Continue};
        self.app.deny_pending();
        self.drain_prompts();
        match end {
            TurnEnd::InputClosed => Break(()),
            _ => {
                self.app.transcript.notice("interrupted");
                runtime.clear_prompt_context();
                Continue(CommandOutcome::Done)
            }
        }
    }

    /// How a typed message ends when a `user_prompt_submit` hook refuses it or the wait is cut
    /// short; `None` lets it through. Nothing is recorded for a refused prompt.
    async fn refuse_by_hook(&mut self, runtime: &AgentRuntime, text: &str) -> Option<TurnEnd> {
        match self.prompt_allowed(runtime, text).await {
            Ok(true) => None,
            Ok(false) => Some(TurnEnd::Finished(TurnOutcome::Answer(String::new()))),
            Err(end) => {
                self.finish_turn(runtime, &end).await;
                Some(end)
            }
        }
    }

    /// Refuses a turn-starting line while a stop stands and puts the text back in the input.
    /// Callers check before recording the line or setting a goal, so the ledger never holds
    /// intent for a turn that did not run. Returns whether it refused.
    pub(crate) fn refuse_if_stopped(&mut self, runtime: &AgentRuntime, raw: &str) -> bool {
        if runtime.state.afk().stopped.is_none() {
            return false;
        }
        self.refuse_stopped(raw);
        true
    }

    fn refuse_stopped(&mut self, raw: &str) {
        self.app
            .transcript
            .error("the session is stopped (AFK temperature ceiling); run /afk review");
        self.app.restore_to_input(raw);
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
        first_origin: MessageOrigin,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        let mut first = Some(first_origin);
        while let Some(submission) = pending.pop_front() {
            // Only an idle-prompt submission was typed after the previous answer was on screen;
            // queued input was typed while a turn ran, before its reply was shown.
            let origin = first.take().unwrap_or(MessageOrigin::Other);
            if self.quitting() {
                runtime.steering.set_delivery_hold(false);
                return Break(());
            }
            self.review_afk_exits(runtime).await;
            if runtime.state.afk().stopped.is_some()
                && matches!(
                    submission,
                    Submission::Message(_)
                        | Submission::Attached { .. }
                        | Submission::Expanded { .. }
                        | Submission::Skill { .. }
                )
            {
                self.refuse_stopped(submission.raw());
                continue;
            }
            runtime.steering.set_delivery_hold(!pending.is_empty());
            let end = match submission {
                Submission::Message(text) => {
                    self.app.transcript.user(text.clone());
                    match self.refresh_before_turn(runtime) {
                        Some(end) => end,
                        None => {
                            Self::keep_leftover_turn(runtime).await;
                            if let Some(end) = self.refuse_by_hook(runtime, &text).await {
                                end
                            } else {
                                // A prompt from the user resumes a paused goal.
                                runtime.goal_user_prompt();
                                runtime.begin_turn(&text, false);
                                self.turn(runtime, text.clone(), Some(&text), origin, Vec::new())
                                    .await
                            }
                        }
                    }
                }
                Submission::Attached { text, attachments } => {
                    self.app.transcript.user(text.clone());
                    let mut files = Vec::new();
                    for pending in attachments {
                        let label = pending.label.clone();
                        let size = pending.size;
                        match into_attachment(pending) {
                            Ok(attachment) => {
                                let kind = match attachment.media {
                                    AttachmentMedia::Image(_) => "image",
                                    AttachmentMedia::Pdf => "pdf",
                                };
                                self.app.transcript.attached(kind, &label, size);
                                files.push(attachment);
                            }
                            Err(why) => self.app.transcript.error(why),
                        }
                    }
                    match self.refresh_before_turn(runtime) {
                        Some(end) => end,
                        None => {
                            Self::keep_leftover_turn(runtime).await;
                            if let Some(end) = self.refuse_by_hook(runtime, &text).await {
                                end
                            } else {
                                // A prompt from the user resumes a paused goal.
                                runtime.goal_user_prompt();
                                runtime.begin_turn(&text, false);
                                self.turn(runtime, text.clone(), Some(&text), origin, files)
                                    .await
                            }
                        }
                    }
                }
                Submission::Expanded { text, prompt } => {
                    self.app.transcript.user(text.clone());
                    // A prompt from the user resumes a paused goal.
                    runtime.goal_user_prompt();
                    match self.refresh_before_turn(runtime) {
                        Some(end) => {
                            runtime.clear_prompt_context();
                            end
                        }
                        None => {
                            Self::keep_leftover_turn(runtime).await;
                            // Recorded as the human's message before this point.
                            runtime.begin_turn(&text, true);
                            self.turn(runtime, prompt, None, MessageOrigin::Other, Vec::new())
                                .await
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
                biased;
                input = self.input.recv() => match input {
                    None => return Break(()),
                    Some(input) => self.act_on(runtime, input),
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
                Action::Command {
                    command: Command::Exit,
                    ..
                } => {
                    self.request_quit(runtime);
                    return Break(());
                }
                Action::CycleMode => {
                    self.cycle_mode(runtime, false);
                    self.review_afk_exits(runtime).await;
                }
                Action::RetryDenied(index) => self.retry_denied(runtime, index),
                Action::Guardian(answer) => self.answer_unavailable(runtime, answer).await,
                Action::UsagePause(answer) => self.answer_usage_pause(runtime, answer).await,
                Action::KillAgent(id) => self.kill_agent(runtime, &id),
                Action::MessageAgent { id, text } => self.message_agent(runtime, id, text),
                Action::KillJob(id) => self.kill_job(runtime, &id),
                Action::None
                | Action::Submit(..)
                | Action::AttachClipboard
                | Action::Queue(_)
                | Action::Command { .. }
                | Action::Shell(_)
                | Action::EditQueued
                | Action::Server(_)
                | Action::GuardianOff(_)
                | Action::Bug(_)
                | Action::AfkReview(_)
                | Action::SkillTrust(_)
                | Action::PluginApprove(_)
                | Action::SetupReview(_)
                | Action::SetupComposition(_)
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
    /// harness started, whose prompt is not. `attachments` are the message's inline files,
    /// already read from disk.
    ///
    /// A model that ran out of quota hands the session to the next entry of its list and the
    /// turn carries on there, before the failure is reported and before a goal hears of it.
    async fn turn(
        &mut self,
        runtime: &mut AgentRuntime,
        text: String,
        human: Option<&str>,
        origin: MessageOrigin,
        attachments: Vec<Attachment>,
    ) -> TurnEnd {
        self.app.automatic_turn = std::mem::take(&mut self.app.automatic_next);
        let mut text = text;
        let mut human = human;
        let mut attachments = attachments;
        loop {
            // A retry, a fall-through or a stop hook's continuation is a new run: none starts
            // once the session is leaving.
            if self.quitting() {
                return TurnEnd::InputClosed;
            }
            let mut end = self
                .attempt(runtime, text, human, origin, attachments)
                .await;
            let mut tried = Tried::default();
            while let TurnEnd::Finished(TurnOutcome::Failed(failure)) = &end
                && failure.falls_through
            {
                let class = failure.class.unwrap_or("quota");
                if runtime.fall_through(class, &mut tried).await.is_some() {
                    end = self
                        .attempt(
                            runtime,
                            RETRY_NOTE.to_string(),
                            None,
                            MessageOrigin::Other,
                            Vec::new(),
                        )
                        .await;
                    continue;
                }
                // The list is spent. A model the human picked while the window was spent
                // takes the request; otherwise a spent usage window holds the run until it
                // resets, and the request goes again with every entry tried afresh.
                if failure.usage_limit.is_some() && self.apply_usage_switch(runtime).await {
                    tried = Tried::default();
                    end = self
                        .attempt(
                            runtime,
                            RETRY_NOTE.to_string(),
                            None,
                            MessageOrigin::Other,
                            Vec::new(),
                        )
                        .await;
                    continue;
                }
                if let Some(spent) = runtime.fall_through_spent()
                    && let TurnEnd::Finished(TurnOutcome::Failed(failure)) = &mut end
                {
                    failure.message.push('\n');
                    failure.message.push_str(&spent);
                }
                if let TurnEnd::Finished(TurnOutcome::Failed(failure)) = &mut end
                    && failure.usage_limit.is_some()
                {
                    match self.wait_out_usage(runtime, failure).await {
                        Ok(true) => {
                            tried = Tried::default();
                            self.apply_usage_switch(runtime).await;
                            end = self
                                .attempt(
                                    runtime,
                                    RETRY_NOTE.to_string(),
                                    None,
                                    MessageOrigin::Other,
                                    Vec::new(),
                                )
                                .await;
                            continue;
                        }
                        Ok(false) => {}
                        Err(interrupted) => end = interrupted,
                    }
                }
                break;
            }
            self.finish_turn(runtime, &end).await;
            // Before the goal hears of the turn: a `stop` hook that blocks continues the turn,
            // and only the turn that finally ends reaches the goal.
            if let TurnEnd::Finished(outcome) = &end {
                match self.hooks_after_turn(runtime, outcome).await {
                    Ok(Some(note)) => {
                        self.app
                            .transcript
                            .notice("a stop hook asked the agent to keep working");
                        // The mode or rules may have changed during the turn, as for any other turn.
                        if let Some(refused) = self.refresh_before_turn(runtime) {
                            // The turn that ran still ends here, so the goal hears of it.
                            self.goal_after_turn(runtime, &end);
                            runtime.refresh_files();
                            return refused;
                        }
                        text = note;
                        human = None;
                        attachments = Vec::new();
                        continue;
                    }
                    Ok(None) => {}
                    Err(TurnEnd::InputClosed) => return TurnEnd::InputClosed,
                    Err(_) => {}
                }
            }
            self.goal_after_turn(runtime, &end);
            runtime.refresh_files();
            return end;
        }
    }

    /// Holds the turn while the main model's usage window is spent, still serving keys and
    /// prompts. `Ok(true)` when the wait ended and the request should go again. Esc gives up on
    /// the wait and cancels the pause.
    async fn wait_out_usage(
        &mut self,
        runtime: &AgentRuntime,
        failure: &mut TurnFailure,
    ) -> Result<bool, TurnEnd> {
        let main = &runtime.models.main;
        let (account, quota) = (main.reference.account.clone(), main.quota.clone());
        let mut waiting = Box::pin(wait_out_usage_limit(
            &runtime.state,
            failure,
            UsageRole::Main,
            &account,
            quota,
        ));
        let waited = self
            .serve_while(runtime, &mut waiting, Serve::Prompts)
            .await;
        drop(waiting);
        if waited.is_err() {
            runtime.state.usage_pause().cancel();
        }
        waited
    }

    /// One run of the agent on `text`, to its end.
    async fn attempt(
        &mut self,
        runtime: &AgentRuntime,
        text: String,
        human: Option<&str>,
        origin: MessageOrigin,
        attachments: Vec<Attachment>,
    ) -> TurnEnd {
        // A wait that ended in failure stopped the run before; this is a new one.
        runtime.state.usage_pause().clear_failure();
        // A refusal left by a step that reset `running` without settling would show here, with
        // no input event to clear it.
        self.app.refusal = None;
        self.app.attach_notice = None;
        if let Some(typed) = human {
            let labels: Vec<String> = attachments.iter().map(|a| a.label.clone()).collect();
            runtime.append(Record::UserMessage {
                origin,
                text: typed.to_string(),
                attachments: labels,
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
        self.app.set_running(true);
        let (events, mut seen) = unbounded_channel();
        let mut on_event = move |event: SessionEvent| {
            let _ = events.send(event);
            Flow::Continue
        };
        let modalities = runtime
            .models
            .main
            .completion
            .spec()
            .map_or_else(rig_core::catalog::Modalities::default, |spec| spec.input);
        // What a dropped run is kept as when it committed nothing: the message it sent, after
        // the conversation as it stood once compaction was done with it. An unreadable count
        // refuses the turn instead: the fallback a stop would fold from a made-up length
        // would misplace the settled run's save.
        let sent = user_message(&prompt, &attachments, &modalities);
        let started_at = std::sync::atomic::AtomicUsize::new(usize::MAX);
        let record_prompt = sent.clone();
        let first_record = sent.clone();
        let mut run = Box::pin(async {
            // Inside the future the input select guards, so a persistence call that blocks
            // cannot keep a quit from being served.
            let first_len = match runtime.conversation_len().await {
                Ok(len) => {
                    started_at.store(len, std::sync::atomic::Ordering::Relaxed);
                    len
                }
                Err(e) => {
                    return TurnOutcome::Failed(
                        format!(
                            "the conversation's length could not be read, so the turn did not run: {}",
                            fontaine_tools::error_chain::chain(&e)
                        )
                        .into(),
                    );
                }
            };
            // The run's durable record goes before compaction: a process that dies during it
            // still leaves the prompt for the fold that resumes the session. A failed write
            // refuses the turn: nothing is journaled yet, and a kill in the unrecorded window
            // would lose the prompt.
            if let Err(e) = runtime.record_run_start(first_record, first_len).await {
                return TurnOutcome::Failed(
                    format!(
                        "the turn could not be journaled: {}",
                        fontaine_tools::error_chain::chain(&e)
                    )
                    .into(),
                );
            }
            runtime.compact_if_needed().await;
            // A compaction that saved shrank the conversation; the run record's positions
            // are post-compaction, like the fallback's. An unreadable count refuses the
            // turn, like the pre-compaction path: the stale pre-compaction count would
            // misplace the fallback a stop folds from, and could duplicate an
            // already-saved prompt.
            let len = match runtime.conversation_len().await {
                Ok(len) => len,
                Err(e) => {
                    let message = format!(
                        "the conversation's length could not be read after compaction, so the turn did not run: {}",
                        fontaine_tools::error_chain::chain(&e)
                    );
                    runtime.approver().warn(&message);
                    // The turn never runs, and the record its start wrote must not be
                    // read later as this turn's fallback.
                    runtime.clear_run_start().await;
                    return TurnOutcome::Failed(message.into());
                }
            };
            if started_at.swap(len, std::sync::atomic::Ordering::Relaxed) != len
                && let Err(e) = runtime.record_run_start(record_prompt, len).await
            {
                // The run is journaled by now; the record is best-effort evidence, and a
                // stale one errs on folding the turn again, never on dropping it.
                let message = format!(
                    "the run's pending record could not be rewritten after compaction: {}",
                    fontaine_tools::error_chain::chain(&e)
                );
                runtime.approver().warn(&message);
            }
            // After the compaction, which can queue the context of a `compact` session start.
            let late = runtime.take_hook_context();
            let prompt = if late.is_empty() {
                prompt
            } else {
                let prompt = format!("{}\n\n{prompt}", late.join("\n\n"));
                let record_prompt = user_message(&prompt, &attachments, &modalities);
                if let Err(e) = runtime.record_run_start(record_prompt, len).await {
                    runtime.approver().warn(&format!(
                        "the run's pending record could not be rewritten after hook context was added: {}",
                        fontaine_tools::error_chain::chain(&e)
                    ));
                }
                prompt
            };
            let ids = runtime.models.main.wire_ids();
            let (model, provider) = (ids.model, ids.provider);
            run_turn(
                TurnParams {
                    agent: &runtime.agent,
                    state: &runtime.state,
                    prompt,
                    attachments,
                    modalities,
                    max_turns: None,
                    priced: runtime.models.main.catalog_billed(),
                    model,
                    provider,
                    native_search: runtime.native_search_active(),
                    generation_options: runtime.turn_generation_options(),
                    max_tokens: runtime.turn_max_tokens(),
                    served: runtime.served(),
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
            let tick = super::soonest_tick(
                &self.app,
                &[self
                    .app
                    .agent_list
                    .counts_time()
                    .then_some(super::LIST_TICK)],
            );
            tokio::select! {
                biased;
                // Input first: a quit already waiting must not lose a same-poll race to the run.
                input = self.input.recv() => match input {
                    None => break TurnEnd::InputClosed,
                    Some(input) => match self.act_on(runtime, input) {
                        // Leaving is not an interruption: nothing is restored or replayed, and
                        // no hook or continuation follows.
                        Action::Quit => break TurnEnd::InputClosed,
                        Action::Interrupt => break TurnEnd::Interrupted,
                        Action::CycleMode => self.cycle_mode(runtime, true),
                        Action::RetryDenied(index) => self.retry_denied(runtime, index),
                        Action::Guardian(answer) => self.answer_unavailable(runtime, answer).await,
                        Action::UsagePause(answer) => self.answer_usage_pause(runtime, answer).await,
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
                        Action::AttachClipboard => self.attach_clipboard(runtime).await,
                        Action::EditQueued => self.edit_queued(runtime),
                        Action::Command { command, raw } => {
                            self.remember(&raw);
                            self.run_immediate(runtime, &command);
                            if self.quitting() {
                                break TurnEnd::InputClosed;
                            }
                        }
                        // A turn or a sign-in is under way, so a rewind cannot start: the picker closes.
                        Action::SelectRewind(_) => self.app.rewind = None,
                        Action::OpenRewind | Action::Rewind { .. } => {}
                        Action::Server(_)
                        | Action::None
                        | Action::Submit(..)
                        | Action::Shell(_)
                        | Action::GuardianOff(_)
                        | Action::Bug(_)
                        | Action::AfkReview(_)
                        | Action::SkillTrust(_)
                        | Action::PluginApprove(_)
                        | Action::SetupReview(_)
                        | Action::SetupComposition(_)
                        | Action::CancelWakeup => {}
                    },
                },
                // Before the turn: a stopped session gets no further tool-capable model turn.
                Ok(()) = self.afk_stops.changed() => break TurnEnd::Stopped,
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
                Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
                Some(first) = self.agent_events.recv() => self.on_agent_events(runtime, first),
                () = async {
                    match tick {
                        Some(wait) => tokio::time::sleep(wait).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        };
        drop(run);
        // A turn that never reached its drain leaves the gate's note for the next one.
        runtime.clear_prompt_context();
        // Only a run cut short is unsettled: one that answered was saved, even when a stop
        // landed in the same poll and is relabelled below. A failure with the stop standing
        // was ended by the stop's dispatch denial and is kept with its marker.
        let unsettled = match &end {
            // A quit keeps the partial turn too, unless the run never got as far as reading
            // the conversation, when there is no position to fold from.
            TurnEnd::InputClosed
                if self.quitting()
                    && started_at.load(std::sync::atomic::Ordering::Relaxed) != usize::MAX =>
            {
                Some(Unsettled::SessionEnded)
            }
            TurnEnd::Interrupted => Some(Unsettled::User),
            TurnEnd::Stopped => Some(Unsettled::AfkStop),
            TurnEnd::Finished(TurnOutcome::Failed(_)) if runtime.state.afk().stopped.is_some() => {
                Some(Unsettled::AfkStop)
            }
            TurnEnd::Finished(_) | TurnEnd::InputClosed => None,
        };
        if let Some(cause) = unsettled {
            let fallback = Fallback {
                prompt: sent,
                log_len_before: started_at.into_inner(),
            };
            match self.quit_deadline() {
                // Under the one deadline the whole exit shares, beside the hooks and the
                // process cleanup that start with it.
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    let _ = tokio::time::timeout(remaining, async {
                        tokio::join!(
                            runtime.keep_unsettled_turn(cause, Some(fallback)),
                            runtime.finish_final_shutdown(deadline),
                        )
                    })
                    .await;
                }
                None => runtime.keep_unsettled_turn(cause, Some(fallback)).await,
            }
        }
        // A stop that lands inside the poll that also finishes the turn is still a stop.
        let end = match end {
            TurnEnd::Finished(_) if runtime.state.afk().stopped.is_some() => {
                // The signal fired in the same poll; it is handled here, not again by the idle
                // loop.
                self.afk_stops.borrow_and_update();
                TurnEnd::Stopped
            }
            other => other,
        };
        while let Ok(event) = seen.try_recv() {
            self.on_event(runtime, event);
        }
        self.show_delivered(runtime);
        end
    }

    /// Fires the `stop` and `stop_failure` hooks while still serving keys and prompts. `Some` is
    /// the note a blocking `stop` hook continues the turn on.
    async fn hooks_after_turn(
        &mut self,
        runtime: &AgentRuntime,
        outcome: &TurnOutcome,
    ) -> Result<Option<String>, TurnEnd> {
        if !runtime.has_hook(HookEvent::Stop) && !runtime.has_hook(HookEvent::StopFailure) {
            return Ok(None);
        }
        self.app.set_running(true);
        let mut hooks = Box::pin(runtime.turn_ended(outcome));
        let result = self.serve_while(runtime, &mut hooks, Serve::Prompts).await;
        drop(hooks);
        self.app.set_running(false);
        result
    }

    /// Whether the hooks let a line the human typed start a turn, for the commands that record
    /// it themselves. `Break` is the input closing; an interrupt ends the wait as a refusal.
    pub(crate) async fn prompt_passes(
        &mut self,
        runtime: &AgentRuntime,
        raw: &str,
    ) -> std::ops::ControlFlow<(), bool> {
        match self.prompt_allowed(runtime, raw).await {
            Ok(allowed) => std::ops::ControlFlow::Continue(allowed),
            Err(TurnEnd::InputClosed) => std::ops::ControlFlow::Break(()),
            Err(end) => {
                self.finish_turn(runtime, &end).await;
                std::ops::ControlFlow::Continue(false)
            }
        }
    }

    /// Runs the `user_prompt_submit` hooks for a prompt the human sent, while still serving keys
    /// and prompts. `Ok(true)` sends the prompt; a block is shown and sends nothing.
    pub(crate) async fn prompt_allowed(
        &mut self,
        runtime: &AgentRuntime,
        prompt: &str,
    ) -> Result<bool, TurnEnd> {
        if !runtime.has_hook(HookEvent::UserPromptSubmit) {
            // Still resets the `stop` streak, which every human prompt does.
            runtime.prompt_submit(prompt).await;
            return Ok(true);
        }
        self.app.set_running(true);
        let mut gate = Box::pin(runtime.prompt_submit(prompt));
        let result = self.serve_while(runtime, &mut gate, Serve::Prompts).await;
        drop(gate);
        self.app.set_running(false);
        match result? {
            PromptGate::Proceed { .. } => Ok(true),
            PromptGate::Blocked(message) => {
                self.app.transcript.error(message);
                Ok(false)
            }
        }
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
        push_verdict(&mut self.app, &event);
        self.app.turn_tokens.observe(&event);
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
        if let AgentViewEvent::Event(ref e) = event {
            push_verdict(&mut self.app, e);
        }
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

    pub(crate) async fn finish_turn(&mut self, runtime: &AgentRuntime, end: &TurnEnd) {
        self.settle_turn(end);
        // The run that met a failed pause is over: idle requests that follow (`/compact`, `/btw`,
        // the goal evaluator) are not part of it and must not inherit its failure. A pick for a
        // pause this turn did not need is stale too.
        runtime.state.usage_pause().clear_failure();
        self.pending_usage_switch = None;
        if matches!(end, TurnEnd::Stopped) {
            // Before the goal hears how the turn ended, so the stop is the reason it pauses.
            self.afk_stopped(runtime).await;
        }
        if matches!(end, TurnEnd::Interrupted | TurnEnd::Stopped) {
            // Decisions the cut-short calls already made belong to this turn, not the next.
            let mut feed = runtime.state.feed().await;
            while feed.try_next().is_some() {}
            while feed.try_next_preview().is_some() {}
        }
    }

    /// Leaves nothing of the turn open: a call without a result would otherwise hold the live
    /// area, and every line behind it, until the session ends.
    pub(crate) fn settle_turn(&mut self, end: &TurnEnd) {
        self.app.set_running(false);
        let answered = matches!(end, TurnEnd::Finished(TurnOutcome::Answer(_)));
        self.app.last_turn_finished = answered;
        self.app.completed_turns += u32::from(answered);
        self.app.suggestion_due = !matches!(end, TurnEnd::InputClosed);
        self.app.note_turn_settled();
        self.app.refusal = None;
        self.app.attach_notice = None;
        self.app.deny_pending();
        self.drain_prompts();
        self.app.transcript.interrupt_pending();
        match end {
            TurnEnd::Finished(TurnOutcome::Failed(failure)) => {
                self.app.transcript.error(failure.message.clone());
            }
            TurnEnd::Finished(TurnOutcome::MaxTurns(message)) => {
                self.app.transcript.notice(message.clone());
            }
            TurnEnd::Finished(_) | TurnEnd::InputClosed => {}
            TurnEnd::Interrupted => self.app.transcript.notice("interrupted"),
            TurnEnd::Stopped => self.app.transcript.notice(
                "stopped: the AFK temperature ceiling was reached, so the turn was cut short",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PendingMedia;

    #[test]
    fn an_attachment_over_the_cap_is_refused_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        std::fs::write(&path, vec![0u8; (MAX_ATTACHMENT_BYTES + 1) as usize]).unwrap();
        let pending = PendingAttachment {
            label: "big.png".into(),
            path,
            media: PendingMedia::Image(rig_core::message::ImageMediaType::PNG),
            size: MAX_ATTACHMENT_BYTES + 1,
        };
        let err = into_attachment(pending).expect_err("an oversized attachment is refused");
        assert!(err.contains("capped at 5.0 MB"), "{err}");
    }

    #[test]
    fn a_non_regular_attachment_is_refused_at_send() {
        let dir = tempfile::tempdir().unwrap();
        let pending = PendingAttachment {
            label: "adir".into(),
            path: dir.path().join("adir"),
            media: PendingMedia::Image(rig_core::message::ImageMediaType::PNG),
            size: 3,
        };
        std::fs::create_dir(&pending.path).unwrap();
        let err = into_attachment(pending).expect_err("a directory is refused");
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn an_attachment_that_grew_past_the_cap_is_refused_at_send() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grown.png");
        std::fs::write(&path, vec![0u8; (MAX_ATTACHMENT_BYTES + 1) as usize]).unwrap();
        let pending = PendingAttachment {
            label: "grown.png".into(),
            path,
            media: PendingMedia::Image(rig_core::message::ImageMediaType::PNG),
            size: 3,
        };
        let err =
            into_attachment(pending).expect_err("an attachment grown past the cap is refused");
        assert!(err.contains("capped at 5.0 MB"), "{err}");
    }

    #[test]
    fn an_attachment_under_the_cap_sends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"png").unwrap();
        let pending = PendingAttachment {
            label: "shot.png".into(),
            path,
            media: PendingMedia::Image(rig_core::message::ImageMediaType::PNG),
            size: 3,
        };
        let attachment = into_attachment(pending).expect("a small attachment sends");
        assert_eq!(attachment.label, "shot.png");
        assert_eq!(attachment.bytes, b"png");
    }
}
