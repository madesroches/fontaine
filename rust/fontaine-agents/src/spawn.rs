//! The tools that make agents a model can use: `agent` starts a subagent and `send_message`
//! talks to one, and with teams on `board_show` and `board_update` work the shared task board.
//! `send_message` and `board_update` skip the guardian at the `coordination` tier and `agent`
//! at the `spawn` tier; the checks here only decide whether a request can be served at all.

use std::sync::Arc;

use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use rig_core::tool::{Tool, ToolContext, ToolExecutionError, ToolOutput};
use serde::Deserialize;
use serde_json::json;

use crate::mode::own_mode;
use crate::registry::{AgentTypes, ModelSpec};
use crate::root_ledger::{AgentEvent, RootLedger};
use crate::runner::{ChildHandle, ChildReport, ChildRunner, ChildStatus, SpawnRequest};
use crate::tasks::TaskStatus;
use crate::tree::{AgentTree, DeliverError, Killed, Notification, NotificationKind, Refusal};

const TARGET: &str = "fontaine::telemetry";

/// The longest `send_message` body.
pub const MESSAGE_CAP_CHARS: usize = 50_000;

/// Who is calling. A call that does not say would get the root agent's toolset and depth, so it
/// fails instead.
pub(crate) fn caller_of(context: &ToolContext) -> Result<String, ToolExecutionError> {
    context.require::<AgentId>().map(|id| id.0).map_err(|e| {
        ToolExecutionError::other(format!("the call does not say which agent made it: {e}"))
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    pub agent_type: String,
    pub prompt: String,
    pub model: Option<String>,
    pub name: Option<String>,
    /// Start a teammate: it stays alive, idle, between pieces of work. Only with teams on.
    pub teammate: Option<bool>,
}

#[derive(Clone)]
pub struct AgentTool {
    pub tree: Arc<AgentTree>,
    pub types: Arc<AgentTypes>,
    pub runner: Arc<dyn ChildRunner>,
    pub ledger: Arc<dyn RootLedger>,
}

/// Gives back a reservation if the call is dropped (the turn was interrupted) before the child
/// has a handle, which would otherwise hold a parallel slot forever.
struct ReleaseOnDrop {
    tree: Arc<AgentTree>,
    id: String,
    armed: bool,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.tree.release(&self.id);
        }
    }
}

fn subagent_event(event: &str, id: &str, name: &str, agent_type: &str) {
    tracing::info!(
        target: TARGET,
        event = event,
        agent.id = id,
        agent.name = name,
        agent.type = agent_type,
        "subagent"
    );
}

pub(crate) fn task_event(event: &str, agent: &str, agent_name: &str, task: &str) {
    tracing::info!(
        target: TARGET,
        event = event,
        agent.id = agent,
        agent.name = agent_name,
        task.id = task,
        "task"
    );
}

/// Returns the tasks `agent` owns to the board, for an agent that ended without finishing them.
fn release_tasks(tree: &AgentTree, ledger: &dyn RootLedger, agent: &str) {
    let Some(teams) = tree.teams() else {
        return;
    };
    let name = tree.name_of(agent);
    for task in teams.board.release_owned_by(agent) {
        task_event("released", agent, &name, &task);
        ledger.append_event(AgentEvent::TaskReleased {
            task,
            agent: agent.to_string(),
        });
    }
}

/// Records how a child ended: the tree, the ledger and telemetry.
fn conclude(
    tree: &AgentTree,
    ledger: &dyn RootLedger,
    id: &str,
    agent_type: &str,
    report: &ChildReport,
) {
    // The ledger first: `AgentTree::settled` treats a report in the tree as the end being on record.
    ledger.append_event(AgentEvent::Finished {
        agent: id.to_string(),
        status: report.status.as_str().to_string(),
    });
    // A teammate that finished its turn keeps its tasks: it is idle, not gone. `finish` says
    // whether it ended so, because a kill can land at any point before it.
    if !tree.finish(id, report.clone()) {
        release_tasks(tree, ledger, id);
    }
    let name = tree.name_of(id);
    let event = match report.status {
        ChildStatus::Killed => "killed",
        ChildStatus::BudgetExhausted => "budget_exhausted",
        _ => "complete",
    };
    subagent_event(event, id, &name, agent_type);
}

/// Kills an agent from outside its run. An idle teammate has no watcher to conclude it, so its
/// end, its released tasks and the telemetry are recorded here.
pub fn kill_agent(tree: &AgentTree, ledger: &dyn RootLedger, id: &str) -> bool {
    let Some(found) = tree.kill_outcome(id) else {
        return false;
    };
    if found == Killed::Idle {
        ledger.append_event(AgentEvent::Finished {
            agent: id.to_string(),
            status: ChildStatus::Killed.as_str().to_string(),
        });
        release_tasks(tree, ledger, id);
        let type_name = tree.type_of(id).unwrap_or_default();
        subagent_event("killed", id, &tree.name_of(id), &type_name);
    }
    true
}

/// Waits for the child's current run to end, hands its report to the parent, and concludes the
/// run. The report goes to the parent's inbox, or to the root's when the parent has ended, so an
/// idle root still gets a turn for it. Then messages the human left for the child unread resume
/// it. A teammate whose turn ended `Done` is first offered more work (see
/// [`continue_teammate`]); only when it has none is it concluded, as idle.
pub fn watch_report(
    tree: Arc<AgentTree>,
    ledger: Arc<dyn RootLedger>,
    parent: String,
    id: String,
    type_name: String,
    handle: Arc<dyn ChildHandle>,
) {
    tokio::spawn(async move {
        let report = handle.report().await;
        let teammate = tree.is_teammate(&id);
        let finished_turn = teammate && report.status == ChildStatus::Done && !tree.was_killed(&id);
        // Decided before the node is finished and before the report is queued, so a surface that
        // sees nothing running never misses a resume that was about to happen.
        if finished_turn
            && continue_teammate(&tree, &ledger, &parent, &id, &type_name, &handle).await
        {
            return;
        }
        // Queued before the child is marked finished, so a surface that sees no one
        // running always finds the report already waiting.
        if report.status != ChildStatus::Killed && !tree.was_killed(&id) {
            let name = tree.name_of(&id);
            let outcome = if finished_turn {
                "finished its turn and is idle (send it a message to wake it; idle teammates are retired once every task on the board is completed)".to_string()
            } else {
                format!("finished: {}", report.status.as_str())
            };
            let note = format!(
                "Agent {name} ({id}, {type_name}) {outcome}\n{}",
                report_text(&report)
            );
            if tree.is_running(&parent) {
                tree.notify_report(&parent, &id, note);
            } else {
                tree.post_to_root(&id, NotificationKind::Report, note);
            }
        }
        conclude(&tree, ledger.as_ref(), &id, &type_name, &report);
        resume_missed(&tree, &ledger, &id, &handle);
        if teammate {
            wake_for_inbox(&tree, &ledger, &id);
            retire_finished_team(&tree);
        }
    });
}

/// Gives a teammate whose turn just ended another run when there is a reason to: a message that
/// arrived while it finished, a `teammate_idle` hook that blocks, or a task on the board it could
/// take. Returns whether it did. The node never stops running in between, so nothing waiting for
/// the end of the run can slip past.
async fn continue_teammate(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    parent: &str,
    id: &str,
    type_name: &str,
    handle: &Arc<dyn ChildHandle>,
) -> bool {
    let Some(teams) = tree.teams() else {
        return false;
    };
    let name = tree.name_of(id);
    // A message that arrived while it finished is read at once; the other two reasons to go on
    // are capped, because nothing else bounds them.
    let queued = tree.has_notifications(id);
    let text = if queued {
        None
    } else {
        let reason = teams.gate.teammate_idle(id, &name).await;
        let text = match reason {
            Some(reason) => format!(
                "[Harness note, not the user's words: a teammate_idle hook asks you to keep working.]\n{reason}"
            ),
            None => {
                let Some(task) = teams.board.claimable() else {
                    return false;
                };
                // A type without the board tools could never act on the note.
                if !tree.tools_of(id).iter().any(|tool| tool == "board_update") {
                    return false;
                }
                format!(
                    "[Harness note, not the user's words: task {} is available: use `board_show` and `board_update` to claim it.]",
                    task.id
                )
            }
        };
        Some(text)
    };
    if let Some(why) = tree.continue_blocker(id) {
        if !tree.shutting_down() {
            teams
                .gate
                .warn(&format!("teammate {name} stays idle: {why}"));
        }
        return false;
    }
    // Counted only once the resume can happen, so a full `max_parallel` does not use up the streak.
    if text.is_some() && !teams.allow_idle_resume(id) {
        teams.gate.warn(&format!(
            "teammate {name} was resumed {} times in a row with no change on the board; leaving it idle",
            crate::tasks::MAX_IDLE_RESUMES
        ));
        return false;
    }
    let text = match text {
        Some(text) => text,
        None => {
            let notes = tree.take_notifications(id);
            if notes.is_empty() {
                return false;
            }
            render_notes(&notes)
        }
    };
    subagent_event("resume", id, &name, type_name);
    handle.resume_with_data(text);
    watch_report(
        tree.clone(),
        ledger.clone(),
        parent.to_string(),
        id.to_string(),
        type_name.to_string(),
        handle.clone(),
    );
    true
}

fn render_notes(notes: &[Notification]) -> String {
    notes
        .iter()
        .map(Notification::render)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Ends the idle teammates once the team's work is done: the board holds tasks, all completed,
/// and no teammate is still running. They leave the agent list as `done`; a later task needs a
/// new teammate.
fn retire_finished_team(tree: &AgentTree) {
    let Some(teams) = tree.teams() else {
        return;
    };
    let tasks = teams.board.list();
    if tasks.is_empty() || tasks.iter().any(|t| t.status != TaskStatus::Completed) {
        return;
    }
    if tree.any_teammate_running() {
        return;
    }
    for id in tree.idle_ids() {
        tree.retire_idle(&id);
    }
}

/// Wakes an idle teammate that was sent messages in the moment between its last check and its
/// end, which would otherwise wait for the next message.
fn wake_for_inbox(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, id: &str) {
    if !tree.is_idle(id) || !tree.has_notifications(id) {
        return;
    }
    let notes = tree.take_notifications(id);
    match wake_idle(tree, ledger, id, render_notes(&notes)) {
        Ok(()) => {}
        Err(WakeError::AlreadyRunning) => tree.requeue(id, notes),
        Err(WakeError::Refused(why)) => {
            // The senders were told "queued"; they learn here that nobody read it.
            let name = tree.name_of(id);
            for note in notes.iter().filter(|n| n.kind == NotificationKind::Message) {
                let text = format!(
                    "Your message to {name} was not read: it ended before it could ({why}).\nThe message was: {}",
                    note.text
                );
                if tree.is_running(&note.from) {
                    tree.push(&note.from, id, NotificationKind::Message, text);
                } else {
                    tree.post_to_root(id, NotificationKind::Message, text);
                }
            }
            if let Some(teams) = tree.teams() {
                teams.gate.warn(&format!(
                    "teammate {} could not read a message sent as it went idle: {why}",
                    tree.name_of(id)
                ));
            }
        }
    }
}

/// Why an idle teammate was not woken.
#[derive(Debug, PartialEq, Eq)]
pub enum WakeError {
    /// Someone else woke it first; its run reads what was meant for it.
    AlreadyRunning,
    Refused(String),
}

/// Wakes an idle teammate on `text`, which is not the human's words. A refused wake releases the
/// teammate's tasks: nothing is going to finish them.
pub fn wake_idle(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    text: String,
) -> Result<(), WakeError> {
    let refuse = |why: String| {
        release_tasks(tree, ledger.as_ref(), id);
        Err(WakeError::Refused(why))
    };
    if let Some(why) = tree.wake_blocker(id) {
        return refuse(why);
    }
    let Some(handle) = tree.handle_of(id) else {
        return refuse("it has no conversation to resume".to_string());
    };
    match resume_inner(tree, ledger, id, &handle, text, false) {
        Ok(()) => Ok(()),
        Err(Refusal::AlreadyRunning) => Err(WakeError::AlreadyRunning),
        Err(other) => refuse(describe(&other)),
    }
}

/// Starts another run of an ended child on `text`, the human's words. The run reports back like
/// the first.
pub fn resume_child(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    handle: &Arc<dyn ChildHandle>,
    text: String,
) -> Result<(), Refusal> {
    resume_inner(tree, ledger, id, handle, text, true)
}

fn resume_inner(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    handle: &Arc<dyn ChildHandle>,
    text: String,
    from_human: bool,
) -> Result<(), Refusal> {
    tree.resume(id)?;
    let type_name = tree.type_of(id).unwrap_or_default();
    ledger.append_event(AgentEvent::Resumed {
        agent: id.to_string(),
    });
    subagent_event("resume", id, &tree.name_of(id), &type_name);
    if from_human {
        handle.resume(text);
    } else {
        handle.resume_with_data(text);
    }
    let parent = tree.parent_of(id).unwrap_or_else(|| ROOT_AGENT.to_string());
    watch_report(
        tree.clone(),
        ledger.clone(),
        parent,
        id.to_string(),
        type_name,
        handle.clone(),
    );
    Ok(())
}

/// Resumes a child that ended with the human's messages still unread, as Claude Code does. This
/// runs after the run was concluded, so a late conclusion cannot end the new run.
fn resume_missed(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    handle: &Arc<dyn ChildHandle>,
) {
    let missed = handle.take_missed();
    if missed.is_empty() {
        return;
    }
    let text = missed.join("\n\n");
    if let Some(why) = tree.resume_blocker(id) {
        handle.report_undelivered(&why, &text);
        return;
    }
    if let Err(refusal) = resume_child(tree, ledger, id, handle, text.clone()) {
        handle.report_undelivered(&describe(&refusal), &text);
    }
}

/// A refusal to resume, in words for the human.
pub fn describe(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Parallel(max) => {
            format!("concurrent subagent limit reached (max_parallel = {max})")
        }
        Refusal::Depth(max) => format!("subagents nest only {max} layers deep"),
        Refusal::SessionBudget(why) => format!("the session's agent budget is spent: {why}"),
        Refusal::AlreadyRunning => "it is already running".to_string(),
        Refusal::Killed => "it was killed".to_string(),
        Refusal::Name(why) => why.clone(),
    }
}

/// The text a parent reads for a child's report.
fn report_text(report: &ChildReport) -> String {
    match report.status {
        ChildStatus::BudgetExhausted => format!(
            "{}\n\n[Partial result: the subagent stopped because {}.]",
            report.text,
            report.reason.as_deref().unwrap_or("its budget was spent")
        ),
        ChildStatus::Failed => format!(
            "{}\n\n[The subagent failed: {}.]",
            report.text,
            report
                .reason
                .as_deref()
                .unwrap_or("it ended without an answer")
        ),
        _ => report.text.clone(),
    }
}

impl Tool for AgentTool {
    const NAME: &'static str = "agent";
    type Args = AgentArgs;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Start a subagent: a separate conversation with its own context that works on one \
             task and returns a report. Give it everything it needs in `prompt`: a subagent \
             cannot see this conversation, except a `fork`, which starts from a copy of it and \
             needs only a short task. The call starts the subagent and returns its `agent_id` and \
             `name` at once. When you start several, give each a short `name` that says its role. \
             Subagents run concurrently with you and with each other, and the report \
             arrives as a notification with a later tool result or turn; call `job_output` with \
             the id or the name to wait for one. A subagent that answers while its own subagents run waits \
             for them before it ends. A subagent has a subset of your tools and never more \
             authority than you, and every call it makes passes the same permission checks. \
             Agent types:\n{}",
            self.types.describe()
        )
    }

    fn parameters(&self) -> serde_json::Value {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "agent_type": {
                    "type": "string",
                    "enum": self.types.names(),
                    "description": "Which kind of subagent to start.",
                },
                "prompt": { "type": "string", "description": "The task. A subagent cannot see this conversation, so give it all the context it needs; a `fork` already has the conversation and needs only a short task." },
                "model": {
                    "type": "string",
                    "description": "Optional model for this agent: `$main`, `$guardian_default` or `<account>/<model>`. Defaults to the agent type's model.",
                },
                "name": {
                    "type": "string",
                    "description": "Optional short name for this agent, 1 to 32 characters of a-z, 0-9 and `-`, starting with a letter, such as `survey-tech-debt`. It must be unused in this session, and `send_message` and `job_output` accept it in place of the id. Defaults to the agent type with a number, such as `explore-1`.",
                },
            },
            "required": ["agent_type", "prompt"],
            "additionalProperties": false,
        });
        if self.tree.teams().is_some()
            && let Some(properties) = schema["properties"].as_object_mut()
        {
            properties.insert(
                "teammate".to_string(),
                json!({
                    "type": "boolean",
                    "description": "Start a teammate: when its turn ends it goes idle instead of done and keeps its name, so `send_message` wakes it for more work, and it can claim tasks from the board with `board_show` and `board_update`, if its type has them: `worker` has both, `explore` can only read the board. Only the main conversation can start one.",
                }),
            );
        }
        schema
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: AgentArgs,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let caller = caller_of(context)?;
        let Some(agent_type) = self.types.get(&args.agent_type) else {
            return Err(ToolExecutionError::invalid_args(format!(
                "no agent type `{}`; available:\n{}",
                args.agent_type,
                self.types.describe()
            )));
        };
        if agent_type.inherits_context {
            if caller != ROOT_AGENT {
                return Err(ToolExecutionError::refused(
                    "only the main conversation can fork itself; start a typed subagent and put what it needs in `prompt`",
                ));
            }
            if args.model.is_some() {
                return Err(ToolExecutionError::invalid_args(
                    "a fork runs on the session model; leave `model` out",
                ));
            }
        }
        let teammate = args.teammate.unwrap_or(false);
        if teammate {
            if self.tree.teams().is_none() {
                return Err(ToolExecutionError::refused(
                    "teammates are off: `[agents] teams = false` is set; remove it from the user config to turn them on",
                ));
            }
            if caller != ROOT_AGENT {
                return Err(ToolExecutionError::refused(
                    "only the main conversation can start a teammate; start a plain subagent instead",
                ));
            }
        }
        let model = match args.model.as_deref() {
            Some(text) => {
                if !agent_type.accepts_model_override() {
                    return Err(ToolExecutionError::invalid_args(format!(
                        "the `{}` agent type comes from a project file and runs on the session model; leave `model` out",
                        agent_type.name
                    )));
                }
                ModelSpec::parse(text).map_err(ToolExecutionError::invalid_args)?
            }
            None => agent_type.model.clone().unwrap_or(ModelSpec::Main),
        };

        let limits = *self.tree.limits();
        let mut tools = agent_type.toolset(&self.tree.tools_of(&caller));
        let depth = self.tree.depth_of(&caller) + 1;
        if depth >= limits.max_depth {
            tools.retain(|tool| tool != "agent");
        }
        let mode = own_mode(agent_type.mode, self.tree.parent_posture(&caller));
        let id = self
            .tree
            .reserve_named(
                &caller,
                &agent_type.name,
                &model.as_text(),
                tools.clone(),
                mode,
                args.name.as_deref(),
            )
            .map_err(|refusal| match refusal {
                Refusal::Parallel(max) => ToolExecutionError::refused(format!(
                    "concurrent subagent limit reached (max_parallel = {max}). Do not retry this call: wait for a running subagent to finish, or do the work yourself."
                )),
                Refusal::Depth(max) => ToolExecutionError::refused(format!(
                    "subagents can nest only {max} layers below the main conversation, and you are at the limit. Do the work yourself."
                )),
                Refusal::SessionBudget(why) => ToolExecutionError::refused(format!(
                    "no more subagents can start: {why}. Do not retry this call."
                )),
                Refusal::Name(why) => ToolExecutionError::invalid_args(why),
                // Only a resume can find a node already running; a reservation is always new.
                Refusal::AlreadyRunning | Refusal::Killed => {
                    ToolExecutionError::other("the subagent slot is already taken")
                }
            })?;

        if teammate {
            self.tree.set_teammate(&id);
        }
        let name = self.tree.name_of(&id);
        let mut reservation = ReleaseOnDrop {
            tree: self.tree.clone(),
            id: id.clone(),
            armed: true,
        };
        let board_tools_held = tools.iter().any(|tool| tool == "board_update");
        let request = SpawnRequest {
            id: id.clone(),
            parent: caller.clone(),
            agent_type: agent_type.clone(),
            prompt: args.prompt,
            model,
            tools,
            mode,
            budget: limits.subagent,
            depth,
            ledger: self.ledger.clone(),
        };
        let handle: Arc<dyn ChildHandle> = match self.runner.spawn(request).await {
            Ok(handle) => handle,
            Err(why) => {
                self.tree.release(&id);
                return Err(ToolExecutionError::other(format!(
                    "the subagent could not start: {why}"
                )));
            }
        };
        self.tree.attach(&id, handle.clone());
        reservation.armed = false;
        self.ledger.append_event(AgentEvent::Spawned {
            agent: id.clone(),
            name: name.clone(),
            parent: caller.clone(),
            agent_type: agent_type.name.clone(),
        });
        subagent_event("spawn", &id, &name, &agent_type.name);

        watch_report(
            self.tree.clone(),
            self.ledger.clone(),
            caller,
            id.clone(),
            agent_type.name.clone(),
            handle,
        );
        let mut out = json!({ "agent_id": id, "name": name });
        if teammate && !board_tools_held {
            out["note"] = json!(format!(
                "{} has no `board_update`, so it cannot claim or complete tasks: put its work in the prompt, or start a `worker`.",
                agent_type.name
            ));
        }
        Ok(ToolOutput::json(out))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageArgs {
    pub to: String,
    pub message: String,
}

#[derive(Clone)]
pub struct SendMessageTool {
    pub tree: Arc<AgentTree>,
    pub ledger: Arc<dyn RootLedger>,
}

impl SendMessageTool {
    /// Wakes the idle teammate `to` with the message as its next prompt, marked as another agent's
    /// untrusted words like a queued one. The sender still just sees it queued.
    fn wake(
        &self,
        from: &str,
        to: &str,
        message: String,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let note = Notification {
            from: from.to_string(),
            from_name: self.tree.name_of(from),
            kind: NotificationKind::Message,
            text: message.clone(),
        };
        match wake_idle(&self.tree, &self.ledger, to, note.render()) {
            Ok(()) => {}
            // Another wake won; its run reads the message from the inbox.
            Err(WakeError::AlreadyRunning) => {
                self.tree
                    .deliver(from, to, message)
                    .map_err(|_| ToolExecutionError::other("agent not running"))?;
            }
            Err(WakeError::Refused(why)) => {
                return Err(ToolExecutionError::other(format!(
                    "the teammate cannot be woken: {why}"
                )));
            }
        }
        self.ledger.append_event(AgentEvent::Message {
            from: from.to_string(),
            to: to.to_string(),
        });
        Ok(ToolOutput::json(json!({ "status": "queued" })))
    }
}

impl Tool for SendMessageTool {
    const NAME: &'static str = "send_message";
    type Args = SendMessageArgs;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Send a message to another running agent: your parent, one of your subagents, a sibling \
         (another subagent of your parent) or `main`, the main conversation. The message is \
         queued and the recipient reads it at its next turn, marked with your agent name and id. \
         A teammate that is idle is woken by it. Returns \
         `{\"status\":\"queued\"}`, or an error when the agent is not running."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "An agent name or id, or `main`." },
                "message": { "type": "string", "description": "What to tell it." },
            },
            "required": ["to", "message"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: SendMessageArgs,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let from = caller_of(context)?;
        if args.message.chars().count() > MESSAGE_CAP_CHARS {
            return Err(ToolExecutionError::invalid_args(format!(
                "a message is at most {MESSAGE_CAP_CHARS} characters"
            )));
        }
        match self.tree.deliver(&from, &args.to, args.message.clone()) {
            Ok(to) => {
                self.ledger.append_event(AgentEvent::Message { from, to });
                Ok(ToolOutput::json(json!({ "status": "queued" })))
            }
            Err(DeliverError::Idle(to)) => self.wake(&from, &to, args.message),
            Err(DeliverError::NotRunning) => Err(ToolExecutionError::other("agent not running")),
            Err(DeliverError::NotReachable) => Err(ToolExecutionError::invalid_args(format!(
                "`{}` is not your parent, one of your subagents, a sibling or `main`",
                args.to
            ))),
        }
    }
}
