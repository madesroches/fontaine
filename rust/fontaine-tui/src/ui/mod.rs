mod commands;
mod goal;
pub(crate) use commands::Launcher;
mod mcp;
mod providers;
mod schedule;
#[cfg(test)]
mod tests;
mod turn;

use std::collections::VecDeque;
use std::path::Path;
use std::time::SystemTime;

use crate::agent_list;
use crate::agents_panel::{AgentsPanel, JobRow, PanelRow};
use crate::config_panel::ConfigKey;
use crate::denials::{DeniedList, UnavailablePrompt};
use crate::input::{PendingAttachment, PendingMedia};
use crate::resume_picker::ResumePicker;
use crate::ui::schedule::IDLE_POLL;
use crate::ui::turn::{CommandOutcome, SkillRun, Submission};
use crate::{Action, App, Command, Prompt, Ui, parse_command};
use crate::{reports, statusbar};
use fontaine_core::agent::{AgentRuntime, Delivered, MessageRefusal};
use fontaine_core::config::PermissionMode;
use fontaine_core::goal::{GoalArg, first_turn_prompt, parse_goal_arg};
use fontaine_core::instructions;
use fontaine_core::ledger::Record;
use fontaine_core::notice::Notice;
use fontaine_core::prompt;
use fontaine_core::session;
use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::lock::read_recovering;
use std::time::Duration;

/// How often the agent list redraws while it lists anything.
const LIST_TICK: Duration = Duration::from_secs(1);

impl<R: FnMut(&mut App)> Ui<R> {
    fn draw(&mut self, runtime: &AgentRuntime) {
        self.app.mode = runtime.state.mode();
        self.app.guardian_off = runtime.state.guardian_off();
        self.app.model = runtime.models.main.reference.to_string();
        self.app.model_accepts_images = runtime
            .models
            .main
            .completion
            .spec()
            .is_some_and(|spec| spec.input.image);
        self.app.prune_closed();
        let rows = runtime.state.agents().rows();
        self.app.running_agents = rows.iter().filter(|r| r.status.is_running()).count();
        let main_writing = runtime.state.agents().writing_of(ROOT_AGENT);
        self.refresh_agent_list(&rows, main_writing);
        if self.app.agents.is_some() {
            self.refresh_panel(runtime, &rows);
        }
        self.show_delivered_to_agents(runtime);
        self.app.queued = runtime.steering.queued();
        self.app.queued_agent = self
            .app
            .viewing
            .as_deref()
            .map(|id| runtime.steering.queued_for(id))
            .unwrap_or_default();
        let scheduler = runtime.session.scheduler();
        let scheduling = runtime.config.schedule.enabled;
        self.app.scheduled = if scheduling { scheduler.count() } else { 0 };
        self.app.wakeup_pending = scheduling && scheduler.wakeup_pending();
        self.app.goal = runtime.session.goal().indicator();
        // The completion index is advisory: a stale list only misses a suggestion.
        let files = read_recovering(&runtime.files).clone();
        self.app.set_files(files);
        (self.render)(&mut self.app);
    }

    /// The list below the status line, from the tree as it stands now.
    fn refresh_agent_list(&mut self, rows: &[fontaine_agents::tree::AgentRow], main_writing: u64) {
        let list = &mut self.app.agent_list;
        list.forget_running(rows);
        let shown = agent_list::rows(
            rows,
            self.app.viewing.as_deref(),
            self.app.agent_list.dismissed(),
            std::time::Instant::now(),
            main_writing,
        );
        self.app.agent_list.set_rows(shown);
        // A call whose agent stopped never gets a result, and would hold the live area.
        for row in rows.iter().filter(|row| !row.status.is_running()) {
            if let Some(view) = self.app.agent_views.get_mut(&row.id) {
                view.transcript.interrupt_pending();
            }
        }
    }

    /// Brings the agents panel up to date with the tree and the jobs.
    fn refresh_panel(&mut self, runtime: &AgentRuntime, rows: &[fontaine_agents::tree::AgentRow]) {
        let panel_rows: Vec<PanelRow> = rows
            .iter()
            .map(|row| PanelRow {
                id: row.id.clone(),
                agent_type: row.agent_type.clone(),
                model: row.model.clone(),
                status: row.status,
                tokens: row.spend.tokens(),
                token_budget: row.budget.tokens,
                cost_usd: row.spend.cost_usd,
                cost_budget: row.budget.cost_usd,
                cost_partial: row.spend.is_partial_cost(),
                depth: row.depth,
                writing: row.writing,
                stopped_because: row.exhausted.map(|why| why.describe()),
            })
            .collect();
        let job_rows: Vec<JobRow> = runtime
            .jobs()
            .snapshot()
            .into_iter()
            .map(|job| JobRow {
                id: job.id,
                command: job.command,
                monitor: job.monitor,
                age: job.age,
                running: job.running,
                state: job.state,
            })
            .collect();
        if let Some(panel) = &mut self.app.agents {
            panel.set_rows(panel_rows);
            panel.set_jobs(job_rows);
        }
    }

    fn on_prompt(&mut self, runtime: Option<&AgentRuntime>, prompt: Prompt) {
        match prompt {
            // The call's turn was cut short while the prompt sat in the queue.
            Prompt::Ask(_, reply) if reply.is_closed() => {}
            Prompt::Unavailable(_, reply) if reply.is_closed() => {}
            Prompt::Question(_, _, reply) if reply.is_closed() => {}
            Prompt::Question(agent, questions, reply) => {
                self.app.begin_question(agent, questions, reply);
            }
            Prompt::Ask(request, reply) => self.app.begin_approval(*request, reply),
            Prompt::Unavailable(info, reply) => {
                let candidates = runtime.map_or_else(Vec::new, AgentRuntime::guardian_candidates);
                self.app
                    .begin_unavailable(UnavailablePrompt::new(*info, candidates, reply));
            }
            Prompt::Notice(severity, message) => self.app.transcript.push_notice(Notice {
                severity,
                headline: message,
                hint: None,
            }),
        }
    }

    /// Handles what queued up during a turn that has ended: the asks belong to calls that no
    /// longer exist, so they are dropped, but notices still count.
    fn drain_prompts(&mut self) {
        while let Ok(prompt) = self.prompts.try_recv() {
            match prompt {
                Prompt::Notice(severity, message) => self.app.transcript.push_notice(Notice {
                    severity,
                    headline: message,
                    hint: None,
                }),
                // A subagent's call is still waiting for its answer.
                Prompt::Ask(request, reply)
                    if request.agent != ROOT_AGENT && !reply.is_closed() =>
                {
                    self.app.begin_approval(*request, reply);
                }
                Prompt::Ask(..) | Prompt::Unavailable(..) | Prompt::Question(..) => {}
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
    pub(crate) fn announce_auto(&mut self, runtime: &AgentRuntime) {
        if let Some(notice) = runtime.setup_notice_on_auto() {
            self.app.transcript.push_notice(notice);
        }
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

    pub(crate) async fn run(&mut self, runtime: &mut AgentRuntime, launch_dir: &Path) {
        let tree = runtime.state.agents().clone();
        loop {
            // Registered before the tree is read: a child that finishes between the read and
            // the wait would otherwise go unseen until the next key.
            let changed = tree.changed().notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let display = tree.display_changed().notified();
            tokio::pin!(display);
            display.as_mut().enable();
            if self.drain_deferred(runtime).await.is_break() {
                return;
            }
            // A subagent that finished while nothing was running has news for the
            // main conversation, which gets a turn of its own to read it.
            if tree.has_notifications(ROOT_AGENT) {
                if self.notification_turn(runtime, launch_dir).await.is_break() {
                    return;
                }
                continue;
            }
            let mut tick = None;
            if runtime.config.schedule.enabled {
                let scheduler = runtime.session.scheduler().clone();
                if let Some(fired) = scheduler.due() {
                    self.show_scheduler_warning(runtime);
                    if self
                        .scheduled_turn(runtime, launch_dir, fired)
                        .await
                        .is_break()
                    {
                        return;
                    }
                    continue;
                }
                self.show_scheduler_warning(runtime);
                tick = scheduler.until_next_wake().map(|wait| wait.min(IDLE_POLL));
            }
            if let Some(flow) = self.goal_step(runtime, launch_dir).await {
                if flow.is_break() {
                    return;
                }
                continue;
            }
            // Elapsed times in the agent list, and the end of an ended row's 30 seconds there,
            // are shown without anything else happening.
            if self.app.agent_list.counts_time() {
                tick = Some(tick.map_or(LIST_TICK, |wait: Duration| wait.min(LIST_TICK)));
            }
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
                Some(first) = self.agent_events.recv() => {
                    self.on_agent_events(runtime, first);
                    Action::None
                }
                _ = &mut changed => Action::None,
                _ = &mut display => Action::None,
                () = async {
                    match tick {
                        Some(wait) => tokio::time::sleep(wait).await,
                        None => std::future::pending().await,
                    }
                } => Action::None,
            };
            match action {
                Action::Quit => return,
                Action::CycleMode => self.cycle_mode(runtime),
                Action::Submit(text, attachments) => {
                    self.remember(&text);
                    if self
                        .submit(runtime, launch_dir, text, attachments)
                        .await
                        .is_break()
                    {
                        return;
                    }
                }
                Action::AttachClipboard => self.attach_clipboard(runtime).await,
                Action::Shell(command) => {
                    self.remember(&format!("!{command}"));
                    if self.shell(runtime, command).await.is_break() {
                        return;
                    }
                }
                Action::EditQueued => self.edit_queued(runtime),
                Action::CancelWakeup => self.cancel_wakeup(runtime),
                Action::RetryDenied(index) => self.retry_denied(runtime, index),
                Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                Action::ResumeSession(id) => {
                    if self.switch_to(runtime, &id).is_break() {
                        return;
                    }
                }
                Action::OpenRewind => self.open_rewind(runtime),
                Action::SelectRewind(turn) => self.offer_rewind(runtime, turn),
                Action::Rewind {
                    plan,
                    what,
                    overwrite_changed,
                } => {
                    self.run_rewind(runtime, plan, what, overwrite_changed)
                        .await
                }
                Action::KillAgent(id) => self.kill_agent(runtime, &id),
                Action::MessageAgent { id, text } => self.message_agent(runtime, id, text),
                Action::KillJob(id) => self.kill_job(runtime, &id),
                Action::PickModel(purpose, reference) => {
                    self.apply_pick(runtime, purpose, &reference).await;
                }
                Action::Providers(request) => self.provider_request(runtime, request).await,
                Action::Config(change) => self.config_change(runtime, change),
                Action::OpenPicker(slot) => self.open_config_picker(runtime, slot).await,
                Action::Server(_)
                | Action::None
                | Action::Interrupt
                | Action::Queue(_)
                | Action::Command { .. }
                | Action::GuardianOff(_)
                | Action::SetupReview(_) => {}
            }
        }
    }

    /// The notice lands where the human is looking: an agent's view, or the main chat.
    fn kill_agent(&mut self, runtime: &AgentRuntime, id: &str) {
        if runtime.state.agents().kill(id) {
            self.app.shown_mut().notice(format!("killed {id}"));
        }
    }

    /// Delivers what the human typed in an agent's view. A running agent reads it with its next
    /// tool result, which the view shows dimmed until then; an ended one starts another run.
    pub(crate) fn message_agent(&mut self, runtime: &AgentRuntime, id: String, text: String) {
        let outcome = runtime.message_agent(&id, text.clone());
        self.delivered(&id, &text, outcome);
    }

    fn delivered(&mut self, id: &str, text: &str, outcome: Result<Delivered, MessageRefusal>) {
        match outcome {
            Ok(Delivered::Queued) => self.remember(text),
            Ok(Delivered::Resumed) => {
                self.remember(text);
                self.app.transcript.notice(format!("you resumed {id}"));
            }
            Ok(Delivered::HandedToResume) => {
                self.remember(text);
                self.app.shown_mut().notice(format!(
                    "{id} ended as your message arrived; it was handed to its resume (a warning follows if that resume was refused)"
                ));
            }
            Err(refusal) => {
                self.app.shown_mut().notice(refusal.notice(id));
                self.app.restore_to_input(text);
            }
        }
    }

    /// A background job killed from the panel; the model is told by the job's own note.
    fn kill_job(&mut self, runtime: &AgentRuntime, id: &str) {
        if runtime.jobs().kill_by_user(id) {
            self.app.transcript.notice(format!("killed {id}"));
        }
    }

    async fn submit(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        text: String,
        attachments: Vec<PendingAttachment>,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        match parse_command(&text) {
            // /loop is never dispatched here or queued: it needs the loop's own
            // first-iteration bookkeeping.
            Some(Command::Loop(argument)) => {
                self.return_attachments(attachments);
                return self
                    .loop_command(runtime, launch_dir, &text, &argument)
                    .await;
            }
            Some(command) => {
                if !attachments.is_empty() {
                    // Commands run, not send; the chips wait for a real message.
                    self.return_attachments(attachments);
                    self.app.transcript.notice(
                        "attachments are sent with a message, not a command; they stay in the input",
                    );
                }
                match self
                    .dispatch_command(runtime, launch_dir, command, &text, false)
                    .await
                {
                    Break(()) => Break(()),
                    // A picker or dialog the command opened is served by the idle loop.
                    Continue(CommandOutcome::Done | CommandOutcome::AwaitingModal) => Continue(()),
                    Continue(CommandOutcome::Turn(submission)) => {
                        self.converse(runtime, launch_dir, VecDeque::from([submission]))
                            .await
                    }
                }
            }
            None => {
                let submission = if attachments.is_empty() {
                    Submission::Message(text)
                } else {
                    Submission::Attached { text, attachments }
                };
                self.converse(runtime, launch_dir, VecDeque::from([submission]))
                    .await
            }
        }
    }

    /// Puts attachments the input sent with a command or a refused send back as chips.
    fn return_attachments(&mut self, attachments: Vec<PendingAttachment>) {
        for pending in attachments {
            self.app.input.attach(pending);
        }
    }

    /// Ctrl+V: read the clipboard image into the session scratch directory and chip it. No
    /// image on the clipboard is a one-line notice, not an error. Blocking clipboard
    /// programs run off the executor's threads.
    async fn attach_clipboard(&mut self, runtime: &AgentRuntime) {
        if self.app.viewing.is_some() {
            self.app
                .shown_mut()
                .notice("attachments can only be sent from the main conversation");
            return;
        }
        let Some(scratch) = runtime.state.scratch.clone() else {
            self.app.attach_notice =
                Some("this session has no scratch directory for attachments".into());
            return;
        };
        let launcher = rig_tools::process::Launcher::default()
            .without_env(runtime.scrub_env().iter().cloned());
        let read = tokio::task::spawn_blocking(move || {
            rig_tools::clipboard::read_image(&launcher, &scratch)
        })
        .await;
        match read {
            Ok(Ok(Some(path))) => {
                let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "clipboard".to_string());
                // `read_image` sniffs before saving and names the file by the type.
                let media = match path.extension().and_then(|ext| ext.to_str()) {
                    Some("jpg" | "jpeg") => {
                        PendingMedia::Image(rig_core::message::ImageMediaType::JPEG)
                    }
                    Some("gif") => PendingMedia::Image(rig_core::message::ImageMediaType::GIF),
                    Some("webp") => PendingMedia::Image(rig_core::message::ImageMediaType::WEBP),
                    _ => PendingMedia::Image(rig_core::message::ImageMediaType::PNG),
                };
                self.app.input.attach(PendingAttachment {
                    label,
                    path,
                    media,
                    size,
                });
                if !self.app.model_accepts_images {
                    self.app.attach_notice = Some("model does not accept images".into());
                }
            }
            Ok(Ok(None)) => self.app.attach_notice = Some("no image on the clipboard".into()),
            Ok(Err(e)) => {
                self.app.attach_notice = Some(format!("the clipboard could not be read: {e}"));
            }
            Err(e) => self.app.attach_notice = Some(format!("the clipboard read failed: {e}")),
        }
    }

    /// Dispatches one parsed command exactly as the idle prompt would, so a queued command
    /// that executes after a turn does what it would have done had it been typed then.
    /// `raw` is the line as typed, recorded for skills, `/goal` and `/init`. `has_pending` is
    /// whether older queued submissions still wait, which refuses session switches. Turn-
    /// starting arms hand the turn back as a submission instead of running it, so no async
    /// recursion forms.
    pub(crate) async fn dispatch_command(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        command: Command,
        raw: &str,
        has_pending: bool,
    ) -> std::ops::ControlFlow<(), CommandOutcome> {
        // In an agent's view the command's notices belong in the view the human is looking at.
        let mark = self.app.notice_mark();
        let outcome = self
            .dispatch_inner(runtime, launch_dir, command, raw, has_pending)
            .await;
        self.app.move_notices_to_shown(mark);
        outcome
    }

    async fn dispatch_inner(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        command: Command,
        raw: &str,
        has_pending: bool,
    ) -> std::ops::ControlFlow<(), CommandOutcome> {
        use std::ops::ControlFlow::{Break, Continue};
        if command.runs_mid_turn() {
            self.run_immediate(runtime, &command);
            return Continue(CommandOutcome::Done);
        }
        match command {
            Command::Exit => Break(()),
            // /loop never reaches here: the idle path keeps its direct arm, and mid-turn it
            // is refused, not queued.
            Command::Loop(_) => Continue(CommandOutcome::Done),
            Command::AddDir(path) => {
                self.add_dir(runtime, launch_dir, &path);
                Continue(CommandOutcome::Done)
            }
            Command::OutputStyle(name) => {
                self.output_style(runtime, &name);
                Continue(CommandOutcome::Done)
            }
            Command::Model(reference) => {
                if reference.is_empty() {
                    self.open_model_picker(runtime, None).await;
                    Continue(CommandOutcome::AwaitingModal)
                } else {
                    self.model(runtime, &reference).await;
                    Continue(CommandOutcome::Done)
                }
            }
            Command::GuardianModel(reference) => {
                if reference.is_empty() {
                    self.open_guardian_picker(runtime).await;
                    Continue(CommandOutcome::AwaitingModal)
                } else {
                    self.guardian_model(runtime, &reference).await;
                    Continue(CommandOutcome::Done)
                }
            }
            Command::GuardianDisable => match self.guardian_disable(runtime).await {
                Break(()) => Break(()),
                Continue(()) => Continue(CommandOutcome::Done),
            },
            Command::AutoModeSetup => match self.auto_mode_setup(runtime).await {
                Break(()) => Break(()),
                Continue(()) => Continue(CommandOutcome::Done),
            },
            Command::GuardianEnable => {
                self.guardian_enable(runtime);
                Continue(CommandOutcome::Done)
            }
            Command::Login(account) => {
                self.login(runtime, &account).await;
                // The dialog stays open after the log-in it started; replay waits for it.
                Continue(CommandOutcome::AwaitingModal)
            }
            Command::Plan(description) => {
                runtime.state.set_mode(PermissionMode::Plan);
                self.app.transcript.notice("plan mode on");
                if description.is_empty() {
                    Continue(CommandOutcome::Done)
                } else {
                    Continue(CommandOutcome::Turn(Submission::Message(description)))
                }
            }
            Command::Goal(argument) => match parse_goal_arg(&argument) {
                GoalArg::Status => {
                    let status = runtime.session.goal().status(runtime.tree_spend());
                    self.app.transcript.notice(status.describe());
                    Continue(CommandOutcome::Done)
                }
                GoalArg::Clear => {
                    match runtime.clear_goal() {
                        Some(cleared) => self
                            .app
                            .transcript
                            .notice(format!("Goal cleared: {}", cleared.condition)),
                        None => self.app.transcript.notice("No goal set"),
                    }
                    Continue(CommandOutcome::Done)
                }
                GoalArg::Set(condition) => {
                    if let Err(e) = runtime.set_goal(&condition) {
                        self.app
                            .transcript
                            .error(fontaine_tools::error_chain::chain(&e));
                        return Continue(CommandOutcome::Done);
                    }
                    self.app
                        .transcript
                        .notice(format!("Goal set: {}", condition.trim()));
                    // The first turn is the user's: the line they typed is the intent the
                    // guardian reads, and later goal turns are harness notes.
                    runtime.append(Record::UserMessage {
                        text: raw.to_string(),
                        attachments: Vec::new(),
                        agent: None,
                    });
                    Continue(CommandOutcome::Turn(Submission::Expanded {
                        text: raw.to_string(),
                        prompt: first_turn_prompt(&condition),
                    }))
                }
            },
            Command::Copy(argument) => {
                self.copy(&argument);
                Continue(CommandOutcome::Done)
            }
            Command::Memory(argument) => {
                self.memory(runtime, &argument);
                Continue(CommandOutcome::Done)
            }
            Command::Init(argument) => {
                if !argument.is_empty() {
                    self.app
                        .transcript
                        .error(format!("/init takes no argument, not `{argument}`"));
                    return Continue(CommandOutcome::Done);
                }
                let (target, existing) = instructions::init_target(&runtime.primary_root());
                // Recorded before the turn so the guardian judging the write has this intent.
                runtime.append(Record::UserMessage {
                    text: format!("/init: create or improve {}", target.display()),
                    attachments: Vec::new(),
                    agent: None,
                });
                Continue(CommandOutcome::Turn(Submission::Expanded {
                    text: "/init".to_string(),
                    prompt: prompt::init_prompt(&target, existing),
                }))
            }
            Command::Resume(argument) => {
                if argument.is_empty() {
                    let others = session::list(&self.data_dir, Some(&self.app.primary_root))
                        .into_iter()
                        .filter(|info| info.id != runtime.session.id)
                        // Every launch writes a start, so a session nothing was said in is only noise.
                        .filter(|info| info.name.is_some() || info.first_prompt.is_some())
                        .collect();
                    self.app.resume = Some(ResumePicker::new(others, SystemTime::now()));
                    Continue(CommandOutcome::AwaitingModal)
                } else if has_pending {
                    self.refuse_switch("/resume");
                    Continue(CommandOutcome::Done)
                } else {
                    match self.switch_to(runtime, &argument) {
                        Break(()) => Break(()),
                        Continue(()) => Continue(CommandOutcome::Done),
                    }
                }
            }
            Command::Rewind(argument) => {
                if !argument.is_empty() {
                    self.app.transcript.error(format!(
                        "/rewind takes no argument, not `{argument}`; it opens a list of your prompts"
                    ));
                    Continue(CommandOutcome::Done)
                } else {
                    self.open_rewind(runtime);
                    Continue(CommandOutcome::AwaitingModal)
                }
            }
            Command::Branch(name) => {
                if has_pending {
                    self.refuse_switch("/branch");
                    Continue(CommandOutcome::Done)
                } else {
                    match self.branch(runtime, &name) {
                        Break(()) => Break(()),
                        Continue(()) => Continue(CommandOutcome::Done),
                    }
                }
            }
            Command::Rename(name) => {
                self.rename(runtime, &name);
                Continue(CommandOutcome::Done)
            }
            Command::Export(path) => {
                self.export(runtime, launch_dir, &path).await;
                Continue(CommandOutcome::Done)
            }
            Command::Clear(name) => {
                self.clear(runtime, &name).await;
                Continue(CommandOutcome::Done)
            }
            Command::Compact(focus) => match self.compact(runtime, focus).await {
                Break(()) => Break(()),
                Continue(()) => Continue(CommandOutcome::Done),
            },
            Command::Context(argument) => {
                self.context(runtime, &argument).await;
                Continue(CommandOutcome::Done)
            }
            Command::Other { name, argument } => {
                let known = runtime
                    .slash_skills()
                    .iter()
                    .any(|(skill, _, _)| *skill == name);
                if !known {
                    self.app.transcript.error(format!(
                        "unknown command /{name}; type / to see the commands and skills"
                    ));
                    return Continue(CommandOutcome::Done);
                }
                match self
                    .expand_skill_command(runtime, raw, &name, &argument)
                    .await
                {
                    Break(()) => Break(()),
                    Continue(SkillRun::Prompt(prompt)) => {
                        Continue(CommandOutcome::Turn(Submission::Expanded {
                            text: raw.to_string(),
                            prompt,
                        }))
                    }
                    Continue(SkillRun::Spent) => Continue(CommandOutcome::Done),
                }
            }
            // The immediate UI set, already handled above by `runs_mid_turn`.
            _ => Continue(CommandOutcome::Done),
        }
    }

    /// Runs one of the immediate UI commands, the set [`Command::runs_mid_turn`] marks:
    /// synchronous handlers that touch only UI state, so both input loops can run them
    /// mid-turn exactly as `submit` would at an idle prompt.
    pub(crate) fn run_immediate(&mut self, runtime: &AgentRuntime, command: &Command) {
        let mark = self.app.notice_mark();
        self.run_immediate_inner(runtime, command);
        self.app.move_notices_to_shown(mark);
    }

    fn run_immediate_inner(&mut self, runtime: &AgentRuntime, command: &Command) {
        match command {
            Command::Help => {
                let (skills, _) = crate::commands::skill_menu(runtime.slash_skills());
                self.app.transcript.notice(reports::help(&skills));
            }
            Command::Status => self.status(runtime),
            Command::Theme(name) => self.theme(name),
            Command::Todos => self.app.todos.toggle(),
            Command::Agents => self.app.agents = Some(AgentsPanel::new()),
            Command::Denied => {
                self.app.denied = Some(DeniedList::new(runtime.state.denied_calls()));
            }
            Command::Permissions => self.permissions(runtime),
            Command::Hooks => {
                let hooks = &runtime.config.hooks;
                self.app.transcript.notice(reports::hooks(
                    &hooks.pre_tool_use,
                    &hooks.post_tool_use,
                    &hooks.permission_denied,
                    runtime.project_executables(),
                ));
            }
            Command::Skills => {
                self.app
                    .transcript
                    .notice(reports::skills(&runtime.extensions().skills));
            }
            Command::Plugins => self.plugins(runtime),
            Command::Mcp(argument) => self.mcp(runtime, argument),
            Command::Expand(argument) => self.expand(argument),
            Command::Config => self.open_config(runtime),
            _ => {}
        }
    }

    /// Carries out what queued up while a turn ran and only the idle loop may do: a picked
    /// model, a session switch, settings changes and provider requests.
    pub(crate) async fn drain_deferred(
        &mut self,
        runtime: &mut AgentRuntime,
    ) -> std::ops::ControlFlow<()> {
        if let Some((purpose, reference)) = self.pending_pick.take() {
            self.apply_pick(runtime, purpose, &reference).await;
        }
        if let Some(id) = self.pending_resume.take() {
            // Switching now would strand undelivered input in this session's steering queue.
            if !runtime.steering.queued().is_empty() {
                self.refuse_switch("/resume");
            } else if self.switch_to(runtime, &id).is_break() {
                return std::ops::ControlFlow::Break(());
            }
        }
        while let Some(change) = self.pending_config.pop_front() {
            match change {
                ConfigKey::OpenPicker(slot) => self.open_config_picker(runtime, slot).await,
                change => self.config_change(runtime, change),
            }
        }
        while let Some(request) = self.pending_provider.pop_front() {
            self.provider_request(runtime, request).await;
        }
        std::ops::ControlFlow::Continue(())
    }

    /// Adds a submitted input to the prompt history; a history file that cannot be written is
    /// reported once.
    fn remember(&mut self, text: &str) {
        if let Some(error) = self.app.history.push(text) {
            self.app.transcript.notice(error);
        }
    }

    /// Up with messages queued: they come back into the input to be edited.
    fn edit_queued(&mut self, runtime: &AgentRuntime) {
        let entries = runtime.steering.retract();
        if entries.is_empty() {
            // The hook delivered them since the last draw, so the display is stale.
            self.app.queued.clear();
            self.app.recall_older();
        } else {
            let raws: Vec<&str> = entries.iter().map(|entry| entry.raw()).collect();
            self.app.restore_to_input(&raws.join("\n\n"));
        }
    }
}
