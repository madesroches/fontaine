//! The tools that make agents a model can use: `agent` starts a subagent and `send_message`
//! talks to one, and with teams on `board_show` and `board_update` work the task boards.
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
use crate::tasks::Reminder;
use crate::tree::{
    AgentTree, Attach, DeliverError, Killed, Notification, NotificationKind, Refusal, Retired,
    RevivalEnd, WakeBlocker,
};

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
}

#[derive(Clone)]
pub struct AgentTool {
    pub tree: Arc<AgentTree>,
    pub types: Arc<AgentTypes>,
    pub runner: Arc<dyn ChildRunner>,
    pub ledger: Arc<dyn RootLedger>,
}

/// Gives back a reservation if the call is dropped (the turn was interrupted) before the child
/// has a handle, which would otherwise hold a parallel slot forever. The release frees the
/// slot for everything that waited on one, such as reports and reminders requeued for it.
struct ReleaseOnDrop {
    tree: Arc<AgentTree>,
    ledger: Arc<dyn RootLedger>,
    id: String,
    armed: bool,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.tree.release(&self.id);
            slot_freed(&self.tree, &self.ledger, Some(&self.id));
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

/// Returns the tasks `agent` holds on its own board and its parent's to `Pending`, for an agent
/// that ended without finishing them, and tells the owners of boards that now have claimable work
/// and free slots.
fn release_tasks(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, agent: &str) {
    release_tasks_of(
        tree,
        ledger,
        agent,
        &tree.name_of(agent),
        tree.parent_of(agent).as_deref(),
        true,
    );
}

fn release_tasks_of(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    agent: &str,
    name: &str,
    parent: Option<&str>,
    remind: bool,
) {
    let Some(teams) = tree.teams() else {
        return;
    };
    let released = teams.boards.release_owned_by(agent, parent);
    for task in &released {
        task_event("released", agent, name, task);
        ledger.append_event(AgentEvent::TaskReleased {
            task: task.clone(),
            agent: agent.to_string(),
        });
    }
    if remind && !released.is_empty() {
        remind_owners(tree, ledger, Some(agent));
    }
}

/// Gives mail an agent will never read back: a message to its sender, who was told it was
/// queued, and a report to the root, which `watch_report` would have sent there had the agent
/// been final when it arrived. A sender that went idle before the bounce is woken through the
/// inbox-wake path, so it learns its mail was not read; only a final or missing sender's
/// bounce goes to the root. `wake` is `false` where no agent may be started, such as during
/// an AFK stop.
fn bounce(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    notes: Vec<Notification>,
    why: &str,
    wake: bool,
) {
    bounce_to(tree, ledger, id, notes, why, wake, false);
}

/// `to_root` sends every message note to the root: with the session budget spent no sender can
/// be woken, and a running one reaches its next turn end through the same refusal, so a note
/// in a sender's inbox would never be read.
fn bounce_to(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    notes: Vec<Notification>,
    why: &str,
    wake: bool,
    to_root: bool,
) {
    let name = tree.name_of(id);
    for note in notes {
        match note.kind {
            NotificationKind::Message => {
                let text = format!(
                    "Your message to {name} was not read: {why}.\nThe message was: {}",
                    note.text
                );
                if !to_root && (tree.is_running(&note.from) || tree.is_idle(&note.from)) {
                    tree.push(&note.from, id, NotificationKind::Message, text);
                    if wake {
                        wake_for_inbox(tree, ledger, &note.from);
                    }
                } else {
                    tree.post_to_root(id, NotificationKind::Message, text);
                }
            }
            NotificationKind::Report => {
                tree.post_to_root(&note.from, NotificationKind::Report, note.text);
            }
            NotificationKind::Job | NotificationKind::Reminder => {}
        }
    }
}

/// Does what an agent that stopped being resumable without a kill of its own needs: its mail goes
/// back, its tasks are released, its board is dropped and the ledger marks it retired, unless it
/// ended some other way, which the ledger keeps. Set `remind` to `false` when the caller will
/// discard any queued reminders itself, to avoid waking unrelated agents mid-sequence.
pub fn settle_retired(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    retired: Vec<Retired>,
    remind: bool,
) {
    for gone in retired {
        bounce(tree, ledger, &gone.id, gone.inbox, "it was retired", remind);
        release_tasks_of(
            tree,
            ledger,
            &gone.id,
            &gone.name,
            gone.parent.as_deref(),
            remind,
        );
        if let Some(teams) = tree.teams() {
            teams.boards.drop_owner(&gone.id);
        }
        if gone.was_done {
            ledger.append_event(AgentEvent::Finished {
                agent: gone.id.clone(),
                status: RETIRED.to_string(),
            });
            subagent_event("retired", &gone.id, &gone.name, &gone.agent_type);
        }
    }
}

/// The ledger's `finished` status of an agent that is final without having failed or been
/// killed.
pub const RETIRED: &str = "retired";

/// Ends the idle agent `id` for good, and the idle agents below it. Returns every id retired.
pub fn retire(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    why: &str,
) -> Vec<String> {
    let retired = tree.retire(id, why);
    let ids = retired.iter().map(|r| r.id.clone()).collect();
    settle_retired(tree, ledger, retired, true);
    ids
}

/// Retires every idle agent, such as when a rewind drops the conversation they were judged
/// against or an AFK stop ends the session's activity. Returns every id retired.
pub fn retire_all_idle(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    why: &str,
) -> Vec<String> {
    let retired = tree.retire_all_idle(why);
    let ids = retired.iter().map(|r| r.id.clone()).collect();
    settle_retired(tree, ledger, retired, true);
    ids
}

/// Like [`retire_all_idle`] but suppresses owner reminders; use during an AFK stop, where the
/// caller discards queued reminders after.
pub fn retire_all_idle_no_remind(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    why: &str,
) -> Vec<String> {
    let retired = tree.retire_all_idle(why);
    let ids = retired.iter().map(|r| r.id.clone()).collect();
    settle_retired(tree, ledger, retired, false);
    ids
}

/// Records how a child ended: the tree, the ledger and telemetry.
fn conclude(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    agent_type: &str,
    report: &ChildReport,
) {
    // The ledger first: `AgentTree::settled` treats a report in the tree as the end being on record.
    ledger.append_event(AgentEvent::Finished {
        agent: id.to_string(),
        status: report.status.as_str().to_string(),
    });
    let parent = tree.parent_of(id);
    // An agent that finished its turn keeps its tasks: it is idle, not gone. `finish` says
    // whether it ended so, because a kill can land at any point before it.
    let finish = tree.finish(id, report.clone());
    // The report was read before a kill that can land later, so the tree's status wins; the
    // ledger keeps the last `finished` record.
    if finish.status == Some(ChildStatus::Killed) && report.status != ChildStatus::Killed {
        ledger.append_event(AgentEvent::Finished {
            agent: id.to_string(),
            status: ChildStatus::Killed.as_str().to_string(),
        });
    }
    if !finish.keeps_tasks {
        // No reminder here: `after_end` sends it once `resume_missed` has had the freed slot, so
        // an idle owner cannot take it first.
        release_tasks_of(
            tree,
            ledger,
            id,
            &tree.name_of(id),
            parent.as_deref(),
            false,
        );
    }
    if finish.final_end
        && let Some(teams) = tree.teams()
    {
        teams.boards.drop_owner(id);
    }
    if finish.final_end {
        // Mail queued while it ran and still unread will never be read.
        let why = match finish.status {
            Some(ChildStatus::Killed) => "it was killed",
            _ => "its budget ran out",
        };
        bounce(tree, ledger, id, tree.take_notifications(id), why, true);
    }
    settle_retired(tree, ledger, finish.retired, true);
    let name = tree.name_of(id);
    // The report was read before the kill that the tree's status records, so the tree wins.
    let event = match finish.status.unwrap_or(report.status) {
        ChildStatus::Killed => "killed",
        ChildStatus::BudgetExhausted => "budget_exhausted",
        _ => "complete",
    };
    subagent_event(event, id, &name, agent_type);
}

/// Kills an agent from outside its run. An idle agent has no watcher to conclude it, so its
/// end, its released tasks and the telemetry are recorded here, and so is the retirement of the
/// idle agents below any target.
pub fn kill_agent(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, id: &str) -> bool {
    kill_agent_inner(tree, ledger, id, true)
}

/// Like [`kill_agent`] but suppresses owner reminders; use during an AFK stop, where the
/// caller discards queued reminders after.
pub fn kill_agent_no_remind(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, id: &str) -> bool {
    kill_agent_inner(tree, ledger, id, false)
}

fn kill_agent_inner(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    remind: bool,
) -> bool {
    let parent = tree.parent_of(id);
    let Some(outcome) = tree.kill_outcome(id) else {
        return false;
    };
    if outcome.killed == Killed::Idle {
        ledger.append_event(AgentEvent::Finished {
            agent: id.to_string(),
            status: ChildStatus::Killed.as_str().to_string(),
        });
        bounce(tree, ledger, id, outcome.inbox, "it was killed", remind);
        release_tasks_of(
            tree,
            ledger,
            id,
            &tree.name_of(id),
            parent.as_deref(),
            remind,
        );
        if let Some(teams) = tree.teams() {
            teams.boards.drop_owner(id);
        }
        let type_name = tree.type_of(id).unwrap_or_default();
        subagent_event("killed", id, &tree.name_of(id), &type_name);
    }
    settle_retired(tree, ledger, outcome.retired, remind);
    true
}

/// What follows any agent giving up a slot or a task: idle agents with mail they could not be
/// woken for are woken, and owners of claimable work are reminded. `except` is the agent whose
/// own end this is.
pub fn slot_freed(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, except: Option<&str>) {
    for id in tree.idle_ids() {
        if Some(id.as_str()) != except && tree.has_notifications(&id) {
            wake_for_inbox(tree, ledger, &id);
        }
    }
    remind_owners(tree, ledger, except);
}

/// Tells the owners of boards with claimable tasks that agents could be started for them: at
/// most as many owners as there are free slots, the earliest spawned first, and only those whose
/// board or free slots changed since they were last told. `except` is the agent whose own action
/// this follows. `main` is not told in a headless run.
pub fn remind_owners(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, except: Option<&str>) {
    let Some(teams) = tree.teams() else {
        return;
    };
    if tree.shutting_down() || tree.wakes_held() {
        return;
    }
    let free = tree.free_slots();
    let mut slots_left = free;
    for owner in tree.by_spawn_order(&teams.boards.claimable_owners()) {
        if slots_left == 0 {
            break;
        }
        if Some(owner.as_str()) == except || (owner == ROOT_AGENT && teams.headless) {
            continue;
        }
        let running = tree.is_running(&owner);
        if !running && !tree.is_idle(&owner) {
            continue;
        }
        // A running subagent's own slot is one of the used ones, and it is the same slot that
        // frees when it goes idle: the key does not change between its turn end and its idle end.
        let key_slots = free + u32::from(running && owner != ROOT_AGENT);
        let Some(reminder) = teams.boards.pending_reminder(&owner, free, key_slots) else {
            continue;
        };
        slots_left -= 1;
        if owner == ROOT_AGENT {
            tree.post_to_root(ROOT_AGENT, NotificationKind::Reminder, reminder.text);
        } else if running {
            tree.push(&owner, &owner, NotificationKind::Reminder, reminder.text);
            // The owner can end its run in the moment between the push and its own next
            // check; the wake is a no-op while it runs and closes that transition.
            wake_for_inbox(tree, ledger, &owner);
        } else if wake_idle(
            tree,
            ledger,
            &owner,
            reminder.text.clone(),
            Vec::new(),
            Some(reminder.clone()),
        )
        .is_err()
        {
            teams.boards.forget_reminder(&owner, &reminder);
        }
    }
}

/// Tells `main` at the end of its turn that tasks on its board wait for agents.
pub fn remind_main(tree: &AgentTree) {
    let Some(teams) = tree.teams() else {
        return;
    };
    if teams.headless || tree.shutting_down() || tree.wakes_held() {
        return;
    }
    let free = tree.free_slots();
    if let Some(reminder) = teams.boards.pending_reminder(ROOT_AGENT, free, free) {
        tree.post_to_root(ROOT_AGENT, NotificationKind::Reminder, reminder.text);
    }
}

/// Waits for the child's current run to end, hands its report to the parent, and concludes the
/// run. The report goes to the parent's inbox, or to the root's when the parent has ended for
/// good, so an idle root still gets a turn for it; an idle parent is woken for it. Then messages
/// the human left for the child unread resume it. A run that ended `Done` is first offered more
/// work (see `continue_agent`); only when it has none is it concluded, as idle.
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
        let finished_turn = report.status == ChildStatus::Done && !tree.was_killed(&id);
        // Decided before the node is finished and before the report is queued, so a surface that
        // sees nothing running never misses a resume that was about to happen.
        if finished_turn && continue_agent(&tree, &ledger, &parent, &id, &type_name, &handle).await
        {
            return;
        }
        finish_run(
            &tree,
            &ledger,
            &parent,
            &id,
            &type_name,
            &report,
            finished_turn,
        );
        resume_missed(&tree, &ledger, &id, &handle);
        after_end(&tree, &ledger, &parent, &id);
    });
}

/// Retries the wakes an end may have left pending, then hands its slot on.
fn after_end(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, parent: &str, id: &str) {
    wake_for_inbox(tree, ledger, id);
    wake_for_inbox(tree, ledger, parent);
    // After the child's own mail is resumed, so a freed slot is not taken first by another
    // idle agent and leaves the human's unread messages undelivered. The just-idled owner is
    // not excluded from the reminder pass: with every slot taken its turn-end check saw none
    // free, and the reminder key, unchanged between its turn end and its idle end, keeps a
    // reminder it did get from repeating.
    slot_freed(tree, ledger, None);
}

/// Queues a run's report for its parent and concludes the run.
fn finish_run(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    parent: &str,
    id: &str,
    type_name: &str,
    report: &ChildReport,
    finished_turn: bool,
) {
    // Queued before the child is marked finished, so a surface that sees no one
    // running always finds the report already waiting.
    if report.status != ChildStatus::Killed && !tree.was_killed(id) {
        let name = tree.name_of(id);
        let outcome = if finished_turn {
            "finished its turn and is idle; send_message wakes it with its conversation intact."
                .to_string()
        } else {
            format!("finished: {}", report.status.as_str())
        };
        let note = format!(
            "Agent {name} ({id}, {type_name}) {outcome}\n{}",
            report_text(report)
        );
        if tree.is_running(parent) || tree.is_idle(parent) {
            tree.notify_report(parent, id, note);
        } else {
            tree.post_to_root(id, NotificationKind::Report, note);
        }
    }
    conclude(tree, ledger, id, type_name, report);
}

/// What gives an agent whose turn just ended another run.
enum Reason {
    Mail,
    Hook(String),
    Reminder(Reminder),
}

/// Gives an agent whose turn just ended `Done` another run when there is a reason to: a message
/// that arrived while it finished, a `teammate_idle` hook that blocks while it holds a task on its
/// parent's board, or tasks on its own board waiting for agents. Returns whether it did. The node
/// never stops running in between, so nothing waiting for the end of the run can slip past.
async fn continue_agent(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    parent: &str,
    id: &str,
    type_name: &str,
    handle: &Arc<dyn ChildHandle>,
) -> bool {
    let name = tree.name_of(id);
    let teams = tree.teams();
    // A message that arrived while it finished is read at once; the other two reasons to go on
    // are bounded, because nothing else bounds them.
    let reason = if tree.has_notifications(id) {
        Reason::Mail
    } else if let Some(teams) = &teams {
        let hook = if teams.boards.owns_in_progress_on(parent, id) {
            teams.gate.teammate_idle(id, &name).await
        } else {
            None
        };
        if let Some(reason) = hook {
            Reason::Hook(reason)
        } else {
            let free = tree.free_slots();
            // Its own slot is still counted as used.
            match teams.boards.pending_reminder(id, free, free + 1) {
                Some(reminder) => Reason::Reminder(reminder),
                None => return false,
            }
        }
    } else {
        return false;
    };
    if let Some(why) = tree.continue_blocker(id) {
        if let (Some(teams), Reason::Reminder(reminder)) = (&teams, &reason) {
            teams.boards.forget_reminder(id, reminder);
        }
        if !tree.shutting_down() {
            tree.warn(&format!("agent {name} stays idle: {why}"));
        }
        return false;
    }
    let text = match reason {
        Reason::Mail => {
            let notes = tree.take_notifications(id);
            if notes.is_empty() {
                return false;
            }
            render_notes(&notes)
        }
        Reason::Hook(reason) => {
            let teams = teams.expect("a hook reason comes from teams");
            // Counted only once the resume can happen, so a full `max_parallel` does not use up the streak.
            if !teams.allow_idle_resume(id, parent) {
                tree.warn(&format!(
                    "agent {name} was resumed {} times in a row with no change on the board that holds its task; leaving it idle",
                    crate::tasks::MAX_IDLE_RESUMES
                ));
                return false;
            }
            format!(
                "[Harness note, not the user's words: a teammate_idle hook asks you to keep working.]\n{reason}"
            )
        }
        Reason::Reminder(reminder) => reminder.text,
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

/// Wakes an idle agent that was sent messages or a report in the moment between its last check
/// and its end, or while no slot was free, which would otherwise wait for the next message.
fn wake_for_inbox(tree: &Arc<AgentTree>, ledger: &Arc<dyn RootLedger>, id: &str) {
    if !tree.is_idle(id) || !tree.has_notifications(id) {
        return;
    }
    let notes = tree.take_notifications(id);
    match wake_idle(tree, ledger, id, render_notes(&notes), notes.clone(), None) {
        Ok(()) => {}
        // Someone woke it first, or no slot is free yet: the notes wait for its next end or the
        // next slot to free.
        Err(WakeError::AlreadyRunning | WakeError::Transient(_)) => tree.requeue(id, notes),
        // The spent budget never clears, so the mail would stay queued forever; the agent keeps
        // its tasks.
        Err(WakeError::SessionBudget(why)) => {
            bounce_to(
                tree,
                ledger,
                id,
                notes,
                &format!("the session's budget is spent ({why})"),
                false,
                true,
            );
        }
        Err(WakeError::Refused(why)) => {
            // The senders were told "queued"; they learn here that nobody read it.
            bounce(
                tree,
                ledger,
                id,
                notes,
                &format!("it ended before it could ({why})"),
                true,
            );
            if !tree.shutting_down() {
                tree.warn(&format!(
                    "agent {} could not read a message sent as it went idle: {why}",
                    tree.name_of(id)
                ));
            }
        }
    }
}

/// Why an idle agent was not woken.
#[derive(Debug, PartialEq, Eq)]
pub enum WakeError {
    /// Someone else woke it first; its run reads what was meant for it.
    AlreadyRunning,
    /// A limit or a session operation stops it for now (the concurrent-agent limit, a held
    /// wake); it stays idle and keeps its tasks.
    Transient(String),
    /// The session's budget is spent and only grows: nothing queued for the agent will be read.
    /// It keeps its tasks.
    SessionBudget(String),
    /// Its end is final; its tasks were released.
    Refused(String),
}

/// Wakes an idle agent on `text`, which is not the human's words. A final refusal releases the
/// agent's tasks: nothing is going to finish them. `waking` is the mail `text` carries; if the
/// agent's conversation cannot be rebuilt after `Ok`, it goes back to its senders.
pub fn wake_idle(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    text: String,
    waking: Vec<Notification>,
    reminder: Option<Reminder>,
) -> Result<(), WakeError> {
    let refuse = |why: String| {
        release_tasks(tree, ledger, id);
        Err(WakeError::Refused(why))
    };
    match tree.wake_blocker(id) {
        // A hold stops the wake for now: it retries once it lifts, and the agent keeps its
        // tasks. The session's end is final.
        Some(WakeBlocker::Transient(why)) => return Err(WakeError::Transient(why)),
        Some(WakeBlocker::Final(why)) => return refuse(why),
        None => {}
    }
    match resume_inner(tree, ledger, id, text, false, waking, reminder) {
        Ok(()) => Ok(()),
        Err(Refusal::AlreadyRunning) => Err(WakeError::AlreadyRunning),
        Err(transient @ Refusal::Parallel(_)) => Err(WakeError::Transient(describe(&transient))),
        // A held wake (a session operation is sweeping the tree) retries once it ends.
        Err(held @ Refusal::Held) => Err(WakeError::Transient(describe(&held))),
        Err(spent @ Refusal::SessionBudget(_)) => Err(WakeError::SessionBudget(describe(&spent))),
        Err(other) => refuse(describe(&other)),
    }
}

/// Starts another run of an ended child on `text`, the human's words. The run reports back like
/// the first.
pub fn resume_child(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    text: String,
) -> Result<(), Refusal> {
    resume_inner(tree, ledger, id, text, true, Vec::new(), None)
}

fn resume_inner(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    text: String,
    from_human: bool,
    waking: Vec<Notification>,
    reminder: Option<Reminder>,
) -> Result<(), Refusal> {
    // A node a resumed session rebuilt has no conversation handle yet: the runner builds one,
    // after the node is claimed.
    let revival = match tree.handle_of(id) {
        Some(handle) => Err(handle),
        None => match (tree.revival_request(id, ledger.clone()), tree.runner()) {
            (Some(request), Some(runner)) => Ok((request, runner)),
            _ => return Err(Refusal::NoConversation),
        },
    };
    tree.resume(id)?;
    let type_name = tree.type_of(id).unwrap_or_default();
    ledger.append_event(AgentEvent::Resumed {
        agent: id.to_string(),
    });
    subagent_event("resume", id, &tree.name_of(id), &type_name);
    let parent = tree.parent_of(id).unwrap_or_else(|| ROOT_AGENT.to_string());
    match revival {
        Err(handle) => start_run(tree, ledger, id, &handle, text, from_human),
        Ok((request, runner)) => {
            let (tree, ledger, id) = (tree.clone(), ledger.clone(), id.to_string());
            tokio::spawn(async move {
                match runner.revive(request).await {
                    Ok(handle) => match tree.attach(&id, handle.clone()) {
                        Attach::Attached => {
                            start_run(&tree, &ledger, &id, &handle, text, from_human);
                        }
                        // A kill or a clear landed while the runner rebuilt the conversation.
                        // No watcher exists for this revival, so its end is concluded here;
                        // the run never started, so the human's words are reported
                        // undelivered, never recorded as read.
                        Attach::Killed | Attach::Missing => {
                            runner.discard_unread(&id);
                            killed_revival(
                                &tree, &ledger, &parent, &id, &type_name, waking, text, from_human,
                            );
                        }
                    },
                    Err(why) => {
                        runner.discard_unread(&id);
                        revive_failed(
                            &tree, &ledger, &parent, &id, &type_name, &why, waking, reminder, text,
                            from_human,
                        );
                    }
                }
            });
        }
    }
    Ok(())
}

fn start_run(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    id: &str,
    handle: &Arc<dyn ChildHandle>,
    text: String,
    from_human: bool,
) {
    if from_human {
        handle.resume(text);
    } else {
        handle.resume_with_data(text);
    }
    let parent = tree.parent_of(id).unwrap_or_else(|| ROOT_AGENT.to_string());
    let type_name = tree.type_of(id).unwrap_or_default();
    watch_report(
        tree.clone(),
        ledger.clone(),
        parent,
        id.to_string(),
        type_name,
        handle.clone(),
    );
}

/// Ends the run of an agent whose conversation could not be rebuilt as failed, reporting it as
/// any failed run is reported. Mail queued while it started is returned to its senders. A kill
/// or a clear that landed while the runner rebuilt is decided under one lock: it concludes the
/// revival as killed, with no report reaching the cleared conversation.
#[allow(clippy::too_many_arguments)]
fn revive_failed(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    parent: &str,
    id: &str,
    type_name: &str,
    why: &str,
    waking: Vec<Notification>,
    reminder: Option<Reminder>,
    text: String,
    from_human: bool,
) {
    match tree.revival_end(id) {
        RevivalEnd::Killed | RevivalEnd::Gone => {
            killed_revival(
                tree, ledger, parent, id, type_name, waking, text, from_human,
            );
        }
        RevivalEnd::Live => {
            if from_human {
                // The run never started, so the words reached no model and the human would
                // otherwise believe the agent was resumed.
                tree.warn(&format!(
                    "{id} could not be revived from its saved conversation ({why}); your message was not delivered: {text}"
                ));
            }
            let mut dropped = waking;
            dropped.extend(tree.take_notifications(id));
            bounce(
                tree,
                ledger,
                id,
                dropped,
                &format!("it could not be revived from its saved conversation ({why})"),
                true,
            );
            let report = ChildReport {
                status: ChildStatus::Failed,
                text: String::new(),
                reason: Some(format!(
                    "it could not be revived from its saved conversation: {why}; messages sent while it started were dropped"
                )),
            };
            finish_run(tree, ledger, parent, id, type_name, &report, false);
            after_end(tree, ledger, parent, id);
            // The revival the reminder's key was claimed for failed; free the key only after
            // this end's own reminder pass, so that pass cannot re-claim it in a tight loop.
            // A later board or slot check retries the reminder.
            if let (Some(reminder), Some(teams)) = (reminder, tree.teams()) {
                teams.boards.forget_reminder(id, &reminder);
            }
        }
    }
}

/// Concludes a handle-less revival whose node was killed or pruned before the handle could
/// attach. The run never started: no report is queued, the human's words are reported
/// undelivered rather than recorded as read, and exactly one killed conclusion lands in the
/// ledger with the telemetry event.
#[allow(clippy::too_many_arguments)]
fn killed_revival(
    tree: &Arc<AgentTree>,
    ledger: &Arc<dyn RootLedger>,
    parent: &str,
    id: &str,
    type_name: &str,
    waking: Vec<Notification>,
    text: String,
    from_human: bool,
) {
    if from_human {
        // The words never reached a model, so they are not recorded as the human's; the human
        // learns they were not delivered.
        tree.warn(&format!(
            "{id} could not read your message (it was killed); it was not delivered: {text}"
        ));
    }
    let mut dropped = waking;
    dropped.extend(tree.take_notifications(id));
    bounce(tree, ledger, id, dropped, "it was killed", true);
    conclude(tree, ledger, id, type_name, &killed_revival_report());
    // The pending revival held a running slot; free it like any other end so
    // requeued waiters and owner reminders get their normal retry.
    after_end(tree, ledger, parent, id);
}

/// The report a revival that never started records: no run, no words.
fn killed_revival_report() -> ChildReport {
    ChildReport {
        status: ChildStatus::Killed,
        text: String::new(),
        reason: Some("it was killed before its saved conversation could be rebuilt".to_string()),
    }
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
    if let Err(refusal) = resume_child(tree, ledger, id, text.clone()) {
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
        Refusal::Held => "wakes are held while the session settles; try again shortly".to_string(),
        Refusal::Retired(why) => format!("it was retired ({why})"),
        Refusal::NoConversation => "it has no conversation to resume".to_string(),
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
             the id or the name to wait for one. When its turn ends it goes idle and keeps its \
             conversation: `send_message` wakes it for follow-up on the same work. Start a new \
             agent for new work. To end one before it finishes, `job_cancel` it; to redirect \
             it, `send_message`. A subagent that answers while its own subagents run waits \
             for them before it ends. A subagent has a subset of your tools and never more \
             authority than you, and every call it makes passes the same permission checks. \
             Agent types:\n{}",
            self.types.describe()
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
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
        })
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
                Refusal::AlreadyRunning
                | Refusal::Held
                | Refusal::Killed
                | Refusal::Retired(_)
                | Refusal::NoConversation => {
                    ToolExecutionError::other("the subagent slot is already taken")
                }
            })?;

        let name = self.tree.name_of(&id);
        let mut reservation = ReleaseOnDrop {
            tree: self.tree.clone(),
            ledger: self.ledger.clone(),
            id: id.clone(),
            armed: true,
        };
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
            // The reservation drops with the error: the slot is released and what waited on
            // one is retried.
            Err(why) => {
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
            model: self.tree.model_of(&id),
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
        Ok(ToolOutput::json(json!({ "agent_id": id, "name": name })))
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
    /// Wakes the idle agent `to` with the message as its next prompt, marked as another agent's
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
        match wake_idle(
            &self.tree,
            &self.ledger,
            to,
            note.render(),
            vec![note],
            None,
        ) {
            Ok(()) => {}
            // Another wake won; its run reads the message from the inbox.
            Err(WakeError::AlreadyRunning) => {
                self.tree
                    .deliver(from, to, message)
                    .map_err(|_| ToolExecutionError::other("agent not running"))?;
            }
            Err(
                WakeError::Transient(why) | WakeError::SessionBudget(why) | WakeError::Refused(why),
            ) => {
                return Err(ToolExecutionError::other(format!(
                    "{} cannot be woken: {why}",
                    self.tree.name_of(to)
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
        "Send a message to another agent: your parent, one of your subagents, a sibling \
         (another subagent of your parent) or `main`, the main conversation. The message is \
         queued and the recipient reads it at its next turn, marked with your agent name and id. \
         An agent whose turn has ended is woken by it with its conversation intact, unless it was \
         killed, ran out of budget or was retired. It cannot stop an agent; `job_cancel` does. Returns \
         `{\"status\":\"queued\"}`, or an error when the agent cannot be reached or woken."
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
            Err(DeliverError::NotRunning(Some(why))) => Err(ToolExecutionError::other(format!(
                "{} cannot be woken: {why}; start a new agent with the context it needs",
                args.to
            ))),
            Err(DeliverError::NotRunning(None)) => {
                Err(ToolExecutionError::other("agent not running"))
            }
            Err(DeliverError::NotReachable) => Err(ToolExecutionError::invalid_args(format!(
                "`{}` is not your parent, one of your subagents, a sibling or `main`",
                args.to
            ))),
        }
    }
}

/// How `job_cancel` stops a subagent: the tree's ownership rule, then the same kill as the
/// agents panel.
pub struct TreeKills {
    pub tree: Arc<AgentTree>,
    pub ledger: Arc<dyn RootLedger>,
}

impl fontaine_tools::jobs::AgentKills for TreeKills {
    fn kill(
        &self,
        caller: &str,
        id: &str,
    ) -> Result<fontaine_tools::jobs::AgentKilled, fontaine_tools::jobs::AgentWaitError> {
        let id = self.tree.owned_child(caller, id)?;
        let stopped = kill_agent(&self.tree, &self.ledger, &id);
        Ok(fontaine_tools::jobs::AgentKilled {
            name: self.tree.name_of(&id),
            id,
            stopped,
        })
    }
}
