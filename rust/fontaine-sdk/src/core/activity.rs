//! The session's activity: one snapshot of whether the instance is working, stuck on a person or
//! idle, derived from the runtime's own turn, agent, job, notification and human-wait state.
//!
//! The snapshot is read, never event-sourced: [`compute`] looks at the authoritative stores, so
//! every way a job, agent or note can end (natural exit, model cancel, panel kill, `/clear`,
//! teardown) shows up without a per-path call. [`ActivityMonitor`] watches the sources, publishes
//! each change once and delivers the `activity_changed` hook.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::{Value, json};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::core::gate::SessionState;
use crate::core::steering::Steering;
use crate::core::user_hooks::LifecycleHooks;
use crate::tools::jobs::Jobs;
use crate::tools::lock::lock_recovering;

/// What the instance is doing as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityState {
    /// Something runs or will run without a human.
    Active,
    /// Some agent cannot progress until a human answers.
    Waiting,
    Idle,
}

impl ActivityState {
    pub fn as_str(self) -> &'static str {
        match self {
            ActivityState::Active => "active",
            ActivityState::Waiting => "waiting",
            ActivityState::Idle => "idle",
        }
    }
}

/// Why the snapshot changed, as the hook's `reason` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityReason {
    WaitingChanged,
    TurnStarted,
    TurnEnded,
    AgentStarted,
    AgentEnded,
    JobStarted,
    JobEnded,
    NotificationQueued,
    NotificationDelivered,
    QueueChanged,
    /// The conversation was cleared or rewound.
    Clear,
    /// The session is ending.
    Shutdown,
}

impl ActivityReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ActivityReason::WaitingChanged => "waiting_changed",
            ActivityReason::TurnStarted => "turn_started",
            ActivityReason::TurnEnded => "turn_ended",
            ActivityReason::AgentStarted => "agent_started",
            ActivityReason::AgentEnded => "agent_ended",
            ActivityReason::JobStarted => "job_started",
            ActivityReason::JobEnded => "job_ended",
            ActivityReason::NotificationQueued => "notification_queued",
            ActivityReason::NotificationDelivered => "notification_delivered",
            ActivityReason::QueueChanged => "queue_changed",
            ActivityReason::Clear => "clear",
            ActivityReason::Shutdown => "shutdown",
        }
    }
}

/// The complete snapshot, which the hook and the status line both carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionActivity {
    pub session_id: String,
    /// Increments on every published change.
    pub seq: u64,
    pub state: ActivityState,
    /// A root turn, its end-of-turn work, or a claimed notification turn is under way.
    pub turn_running: bool,
    /// The root is blocked on a prompt, question or elicitation, or a running subagent on a
    /// permission prompt.
    pub waiting_on_human: bool,
    /// Subagents whose status is running, those waiting on a permission prompt included.
    pub running_agents: usize,
    /// Background shells and monitors still running.
    pub running_background_jobs: usize,
    /// The monitors among `running_background_jobs`.
    pub running_monitors: usize,
    /// Notes some surface will deliver and none has yet.
    pub pending_notifications: usize,
    /// Human messages queued behind the running turn.
    pub queued_prompts: usize,
}

impl SessionActivity {
    /// The aggregate state of the other fields: `waiting` beats `active` even while jobs run.
    pub fn aggregate(&self) -> ActivityState {
        if self.waiting_on_human {
            ActivityState::Waiting
        } else if self.turn_running
            || self.running_agents > 0
            || self.running_background_jobs > 0
            || self.pending_notifications > 0
            || self.queued_prompts > 0
        {
            ActivityState::Active
        } else {
            ActivityState::Idle
        }
    }

    /// The object the hook and the status line both receive.
    pub fn to_json(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "seq": self.seq,
            "state": self.state.as_str(),
            "turn_running": self.turn_running,
            "waiting_on_human": self.waiting_on_human,
            "running_agents": self.running_agents,
            "running_background_jobs": self.running_background_jobs,
            "running_monitors": self.running_monitors,
            "pending_notifications": self.pending_notifications,
            "queued_prompts": self.queued_prompts,
        })
    }

    fn same_as(&self, other: &SessionActivity) -> bool {
        let mut other = other.clone();
        other.seq = self.seq;
        *self == other
    }

    /// What changed from `previous`, by a fixed priority.
    pub fn reason_after(&self, previous: &SessionActivity) -> ActivityReason {
        use std::cmp::Ordering::{Greater, Less};
        if self.waiting_on_human != previous.waiting_on_human {
            ActivityReason::WaitingChanged
        } else if self.turn_running != previous.turn_running {
            if self.turn_running {
                ActivityReason::TurnStarted
            } else {
                ActivityReason::TurnEnded
            }
        } else {
            match self.running_agents.cmp(&previous.running_agents) {
                Greater => return ActivityReason::AgentStarted,
                Less => return ActivityReason::AgentEnded,
                _ => {}
            }
            let jobs = (self.running_background_jobs, self.running_monitors);
            let was = (previous.running_background_jobs, previous.running_monitors);
            match jobs.cmp(&was) {
                Greater => return ActivityReason::JobStarted,
                Less => return ActivityReason::JobEnded,
                _ => {}
            }
            match self
                .pending_notifications
                .cmp(&previous.pending_notifications)
            {
                Greater => return ActivityReason::NotificationQueued,
                Less => return ActivityReason::NotificationDelivered,
                _ => {}
            }
            ActivityReason::QueueChanged
        }
    }
}

/// Counts the turns in flight and carries the wake signal every source of the snapshot shares.
///
/// Held by the session state; surfaces hold [`TurnScope`]s around their turn loops and the
/// notification path hands out a [`TurnClaim`].
pub struct ActivityTracker {
    turns: AtomicUsize,
    /// Human inputs a host queued itself, behind the steering queue.
    queued_inputs: AtomicUsize,
    /// Whether the host starts a turn for a note on its own, between host messages.
    delivers_between_messages: AtomicBool,
    /// Whether a root turn must run under a scope, which debug builds assert.
    scopes_required: AtomicBool,
    /// The reason an operation in progress gives every transition, such as a clear.
    reason_override: Mutex<Option<ActivityReason>>,
    /// Set when an override ended: the next compute publishes even without a difference.
    forced: Mutex<Option<ActivityReason>>,
    notify: Arc<Notify>,
}

impl ActivityTracker {
    pub fn new() -> Arc<ActivityTracker> {
        Arc::new(ActivityTracker {
            turns: AtomicUsize::new(0),
            queued_inputs: AtomicUsize::new(0),
            delivers_between_messages: AtomicBool::new(false),
            scopes_required: AtomicBool::new(false),
            reason_override: Mutex::new(None),
            forced: Mutex::new(None),
            notify: Arc::new(Notify::new()),
        })
    }

    /// The wake signal; a poke stores a permit, so one made before the wait is not lost.
    pub fn notify(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    pub fn poke(&self) {
        self.notify.notify_one();
    }

    /// How many scopes and claims are held now.
    pub fn turns(&self) -> usize {
        self.turns.load(Ordering::SeqCst)
    }

    /// Marks the start of a turn, its end-of-turn work or a wait that precedes one. It ends when
    /// the scope drops, however the turn ends.
    pub fn turn_scope(self: &Arc<Self>) -> TurnScope {
        TurnScope(Hold::new(self))
    }

    /// Marks notes taken from the inbox that are not yet a turn.
    pub fn turn_claim(self: &Arc<Self>) -> TurnClaim {
        TurnClaim(Hold::new(self))
    }

    /// Whether a surface that runs root turns holds a scope for each, which debug builds assert.
    pub fn require_scopes(&self) {
        self.scopes_required.store(true, Ordering::SeqCst);
    }

    /// Debug builds only, and only for a surface that called `require_scopes`: a root turn,
    /// compaction or model request running with no scope held would show the session idle.
    pub fn assert_scoped(&self, what: &str) {
        debug_assert!(
            !self.scopes_required.load(Ordering::SeqCst) || self.turns() > 0,
            "{what} ran with no turn scope held, so the session would show idle"
        );
    }

    pub fn set_delivers_between_messages(&self, delivers: bool) {
        self.delivers_between_messages
            .store(delivers, Ordering::SeqCst);
        self.poke();
    }

    /// The inputs the host holds in a queue of its own, as last published.
    pub fn queued_inputs(&self) -> usize {
        self.queued_inputs.load(Ordering::SeqCst)
    }

    /// Publishes how many inputs the host holds in a queue of its own.
    pub fn set_queued_inputs(&self, count: usize) {
        if self.queued_inputs.swap(count, Ordering::SeqCst) != count {
            self.poke();
        }
    }

    /// Gives every transition `reason` until the guard drops, then forces one publish so the
    /// reason is not lost to deduplication.
    pub fn override_reason(self: &Arc<Self>, reason: ActivityReason) -> ReasonOverride {
        *lock_recovering(&self.reason_override) = Some(reason);
        self.poke();
        ReasonOverride {
            tracker: self.clone(),
            reason,
        }
    }

    fn current_override(&self) -> Option<ActivityReason> {
        // A plain value replaced whole; a panic cannot leave it half-updated.
        *lock_recovering(&self.reason_override)
    }

    fn take_forced(&self) -> Option<ActivityReason> {
        // A plain value replaced whole; a panic cannot leave it half-updated.
        lock_recovering(&self.forced).take()
    }
}

struct Hold(Arc<ActivityTracker>);

impl Hold {
    fn new(tracker: &Arc<ActivityTracker>) -> Hold {
        tracker.turns.fetch_add(1, Ordering::SeqCst);
        tracker.poke();
        Hold(tracker.clone())
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.turns.fetch_sub(1, Ordering::SeqCst);
        self.0.poke();
    }
}

/// A root turn's span; see [`ActivityTracker::turn_scope`].
#[must_use]
pub struct TurnScope(#[allow(dead_code)] Hold);

/// Notes taken from the inbox on their way to a turn; see [`ActivityTracker::turn_claim`]. It is
/// released when the turn starts under its own scope, or when the surface drops it.
#[must_use]
pub struct TurnClaim(#[allow(dead_code)] Hold);

/// Ends a reason override when dropped; see [`ActivityTracker::override_reason`].
#[must_use]
pub struct ReasonOverride {
    tracker: Arc<ActivityTracker>,
    reason: ActivityReason,
}

impl Drop for ReasonOverride {
    fn drop(&mut self) {
        // A plain value replaced whole; a panic cannot leave it half-updated.
        *lock_recovering(&self.tracker.reason_override) = None;
        *lock_recovering(&self.tracker.forced) = Some(self.reason);
        self.tracker.poke();
    }
}

/// What the snapshot is read from.
pub struct Sources<'a> {
    pub state: &'a SessionState,
    pub jobs: Option<&'a Jobs>,
    pub steering: &'a Steering,
}

/// Reads the snapshot (with `seq` 0).
///
/// The read order closes the hand-offs that would otherwise show a false idle, and each is pinned
/// by a test. A job's end and its note are recorded under one lock, so jobs are read before the
/// inbox. A subagent's report is queued before it leaves running, so its count is read before the
/// inbox. A claim is taken before the inbox is drained, so the counter is read after the inbox,
/// and a goal reminder or a held waiter's note is posted under a scope, so it is read before
/// too. An idle subagent's notes move from its inbox into a run, so the running count is read
/// again last, after the in-flight count that bridges the move.
pub fn compute(sources: &Sources<'_>) -> SessionActivity {
    compute_reading(sources, false)
}

/// `closing` is the session's last reading: the exit's own turn scopes and the root inbox, which
/// nothing will read any more, are ignored so the final snapshot says idle.
fn compute_reading(sources: &Sources<'_>, closing: bool) -> SessionActivity {
    let state = sources.state;
    let agents = state.agents();
    let tracker = state.activity();
    let (background, monitors) = sources.jobs.map_or((0, 0), |jobs| {
        let running = jobs.running_background();
        (
            running.len(),
            running.iter().filter(|job| job.monitor).count(),
        )
    });
    let agents_before = agents.running_count();
    let turns_before = tracker.turns();
    let pending = agents.pending_notes();
    let turns_after = tracker.turns();
    let agents_after = agents.running_count();
    let turn_running = !closing && (turns_before > 0 || turns_after > 0);
    let held = state.unattended_blocked().is_some();
    let delivers = tracker.delivers_between_messages.load(Ordering::SeqCst);
    let pending_notifications = if held {
        0
    } else {
        let root = if !closing && (delivers || turn_running) {
            pending.root
        } else {
            0
        };
        root + pending.subagents
    };
    let mut activity = SessionActivity {
        session_id: state.session_id.clone(),
        seq: 0,
        state: ActivityState::Idle,
        turn_running,
        waiting_on_human: state.any_waiting(),
        running_agents: agents_before.max(agents_after),
        running_background_jobs: background,
        running_monitors: monitors,
        pending_notifications,
        queued_prompts: sources.steering.queued_len()
            + tracker.queued_inputs.load(Ordering::SeqCst),
    };
    activity.state = activity.aggregate();
    activity
}

type Delivery = Pin<Box<dyn Future<Output = ()> + Send>>;

/// What the monitor's task leaves when it stops.
pub struct Quiesced {
    pub last_published: SessionActivity,
    /// The state of the last snapshot the hook was given.
    pub last_delivered_state: ActivityState,
}

/// Watches the sources, publishes each change once and delivers it to the `activity_changed`
/// hooks.
///
/// Delivery is serial and latest-wins: one hook run at a time, and while it runs only the newest
/// snapshot waits, so a slow hook never reorders or backs up and the final state is always
/// delivered. `previous_state` and `reason` are relative to the last snapshot delivered; a forced
/// reason such as `clear` survives a skip.
pub struct ActivityMonitor {
    tx: watch::Sender<SessionActivity>,
    stop: Arc<Notify>,
    task: Mutex<Option<JoinHandle<Quiesced>>>,
    // Kept apart from `task` because `quiesce` takes the handle out and awaits it, and a timeout
    // there drops that future with the task still running.
    abort: Option<tokio::task::AbortHandle>,
    ended: AtomicBool,
}

impl ActivityMonitor {
    /// Starts the monitor on the current tokio runtime; without one the snapshot stays at its
    /// first reading.
    pub fn spawn(
        state: &Arc<SessionState>,
        jobs: &Arc<Jobs>,
        steering: &Steering,
        hooks: LifecycleHooks,
    ) -> ActivityMonitor {
        let mut first = compute(&Sources {
            state,
            jobs: Some(jobs),
            steering,
        });
        first.seq = 0;
        let (tx, _) = watch::channel(first);
        let stop = Arc::new(Notify::new());
        let task = tokio::runtime::Handle::try_current().ok().map(|runtime| {
            runtime.spawn(run(
                Arc::downgrade(state),
                Arc::downgrade(jobs),
                steering.clone(),
                hooks,
                tx.clone(),
                stop.clone(),
            ))
        });
        ActivityMonitor {
            tx,
            stop,
            abort: task.as_ref().map(JoinHandle::abort_handle),
            task: Mutex::new(task),
            ended: AtomicBool::new(false),
        }
    }

    /// The latest published snapshot.
    pub fn subscribe(&self) -> watch::Receiver<SessionActivity> {
        self.tx.subscribe()
    }

    /// Stops the monitor after any delivery in flight and returns where it stood. `None` when it
    /// never ran or already stopped.
    pub async fn quiesce(&self) -> Option<Quiesced> {
        // Taken out, so a second caller finds nothing to wait for.
        let handle = lock_recovering(&self.task).take()?;
        self.stop.notify_one();
        let _abort_on_cancel = self.abort.clone().map(AbortOnDrop);
        handle.await.ok()
    }

    /// The session's last transition, `shutdown`, once: the monitor is stopped first so it is
    /// never concurrent with one of its own deliveries, and the snapshot is delivered even when
    /// nothing differs from the last. `sources` is read after the monitor stopped.
    pub async fn shutdown_transition(&self, sources: &Sources<'_>, hooks: &LifecycleHooks) {
        if self.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        let stopped = self.quiesce().await;
        let (seq, previous) = match &stopped {
            Some(stopped) => (stopped.last_published.seq, stopped.last_delivered_state),
            None => {
                let last = self.tx.borrow();
                (last.seq, last.state)
            }
        };
        let closing = sources.state.agents().shutting_down();
        let mut now = compute_reading(sources, closing);
        now.seq = seq + 1;
        self.tx.send_replace(now.clone());
        hooks
            .activity_changed(
                now.state.as_str(),
                ActivityReason::Shutdown.as_str(),
                previous.as_str(),
                now.to_json(),
            )
            .await;
    }
}

impl Drop for ActivityMonitor {
    fn drop(&mut self) {
        if let Some(abort) = &self.abort {
            abort.abort();
        }
    }
}

/// Aborts the monitor's task when dropped (a no-op once it finished), so a cancelled `quiesce`
/// does not leave it running.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One snapshot waiting for its turn at the hook.
struct Pending {
    activity: SessionActivity,
    /// A reason an operation forced; it outranks the diff and survives a skip.
    forced: Option<ActivityReason>,
}

async fn run(
    state: Weak<SessionState>,
    jobs: Weak<Jobs>,
    steering: Steering,
    hooks: LifecycleHooks,
    tx: watch::Sender<SessionActivity>,
    stop: Arc<Notify>,
) -> Quiesced {
    let Some(state) = state.upgrade() else {
        let last = tx.borrow().clone();
        return Quiesced {
            last_delivered_state: last.state,
            last_published: last,
        };
    };
    let tracker = state.activity().clone();
    let mut jobs_changed = jobs.upgrade().map(|jobs| jobs.subscribe());
    let mut last = tx.borrow().clone();
    let mut delivered = last.clone();
    let mut pending: Option<Pending> = None;
    let mut in_flight: Option<Delivery> = None;
    loop {
        // Every wake source is registered before the read, or a change between the read and the
        // wait is missed: the tree wakes with `notify_waiters` and stores no permit.
        let tree_changed = state.agents().changed().notified();
        let tree_display = state.agents().display_changed().notified();
        let tracker_poked = tracker.notify.notified();
        tokio::pin!(tree_changed, tree_display, tracker_poked);
        tree_changed.as_mut().enable();
        tree_display.as_mut().enable();
        if let Some(rx) = jobs_changed.as_mut() {
            rx.borrow_and_update();
        }
        let forced = tracker.take_forced();
        let strong = jobs.upgrade();
        let mut next = compute(&Sources {
            state: &state,
            jobs: strong.as_deref(),
            steering: &steering,
        });
        drop(strong);
        if forced.is_some() || !next.same_as(&last) {
            next.seq = last.seq + 1;
            last = next;
            tx.send_replace(last.clone());
            let forced = forced.or_else(|| tracker.current_override());
            let carried = pending.take().and_then(|p| p.forced);
            pending = Some(Pending {
                activity: last.clone(),
                forced: forced.or(carried),
            });
        }
        if in_flight.is_none()
            && let Some(next) = pending.take()
        {
            let unchanged = next.forced.is_none() && next.activity.same_as(&delivered);
            if !unchanged && hooks.has(crate::core::config::HookEvent::ActivityChanged) {
                let reason = next
                    .forced
                    .unwrap_or_else(|| next.activity.reason_after(&delivered));
                let (hooks, activity, previous) =
                    (hooks.clone(), next.activity.clone(), delivered.state);
                in_flight = Some(Box::pin(async move {
                    hooks
                        .activity_changed(
                            activity.state.as_str(),
                            reason.as_str(),
                            previous.as_str(),
                            activity.to_json(),
                        )
                        .await;
                }));
            }
            if !unchanged {
                delivered = next.activity;
            }
        }
        let jobs_wake = async {
            match jobs_changed.as_mut() {
                Some(rx) => {
                    if rx.changed().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
                None => std::future::pending::<()>().await,
            }
        };
        let delivering = async {
            match in_flight.as_mut() {
                Some(delivery) => delivery.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = stop.notified() => {
                if let Some(delivery) = in_flight.take() {
                    delivery.await;
                }
                return Quiesced {
                    last_published: last,
                    last_delivered_state: delivered.state,
                };
            }
            () = delivering => in_flight = None,
            () = &mut tree_changed => {}
            () = &mut tree_display => {}
            () = &mut tracker_poked => {}
            () = jobs_wake => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> SessionActivity {
        SessionActivity {
            session_id: "s".into(),
            seq: 0,
            state: ActivityState::Idle,
            turn_running: false,
            waiting_on_human: false,
            running_agents: 0,
            running_background_jobs: 0,
            running_monitors: 0,
            pending_notifications: 0,
            queued_prompts: 0,
        }
    }

    #[test]
    fn every_combination_of_the_fields_maps_to_one_state() {
        for bits in 0u32..128 {
            let activity = SessionActivity {
                turn_running: bits & 1 != 0,
                waiting_on_human: bits & 2 != 0,
                running_agents: usize::from(bits & 4 != 0),
                running_background_jobs: usize::from(bits & 8 != 0),
                pending_notifications: usize::from(bits & 16 != 0),
                queued_prompts: usize::from(bits & 32 != 0),
                running_monitors: usize::from(bits & 64 != 0),
                ..quiet()
            };
            let busy = bits & (1 | 4 | 8 | 16 | 32) != 0;
            let expected = if bits & 2 != 0 {
                ActivityState::Waiting
            } else if busy {
                ActivityState::Active
            } else {
                ActivityState::Idle
            };
            assert_eq!(activity.aggregate(), expected, "bits {bits:#b}");
        }
    }

    #[test]
    fn waiting_beats_active_even_while_jobs_run() {
        let activity = SessionActivity {
            waiting_on_human: true,
            running_background_jobs: 2,
            turn_running: true,
            ..quiet()
        };
        assert_eq!(activity.aggregate(), ActivityState::Waiting);
    }

    #[test]
    fn the_json_shape_is_pinned() {
        let activity = SessionActivity {
            seq: 7,
            state: ActivityState::Active,
            running_agents: 1,
            running_background_jobs: 2,
            running_monitors: 1,
            pending_notifications: 3,
            queued_prompts: 4,
            turn_running: true,
            ..quiet()
        };
        assert_eq!(
            activity.to_json(),
            json!({
                "session_id": "s",
                "seq": 7,
                "state": "active",
                "turn_running": true,
                "waiting_on_human": false,
                "running_agents": 1,
                "running_background_jobs": 2,
                "running_monitors": 1,
                "pending_notifications": 3,
                "queued_prompts": 4,
            })
        );
    }

    #[test]
    fn the_reason_follows_the_fixed_priority() {
        let before = quiet();
        let both = SessionActivity {
            waiting_on_human: true,
            turn_running: true,
            ..quiet()
        };
        assert_eq!(both.reason_after(&before), ActivityReason::WaitingChanged);
        let turn = SessionActivity {
            turn_running: true,
            running_agents: 1,
            ..quiet()
        };
        assert_eq!(turn.reason_after(&before), ActivityReason::TurnStarted);
        assert_eq!(before.reason_after(&turn), ActivityReason::TurnEnded);
        let agent = SessionActivity {
            running_agents: 1,
            running_background_jobs: 1,
            ..quiet()
        };
        assert_eq!(agent.reason_after(&before), ActivityReason::AgentStarted);
        assert_eq!(before.reason_after(&agent), ActivityReason::AgentEnded);
        let job = SessionActivity {
            running_background_jobs: 1,
            pending_notifications: 1,
            ..quiet()
        };
        assert_eq!(job.reason_after(&before), ActivityReason::JobStarted);
        assert_eq!(before.reason_after(&job), ActivityReason::JobEnded);
        let note = SessionActivity {
            pending_notifications: 1,
            ..quiet()
        };
        assert_eq!(
            note.reason_after(&before),
            ActivityReason::NotificationQueued
        );
        assert_eq!(
            before.reason_after(&note),
            ActivityReason::NotificationDelivered
        );
        let queued = SessionActivity {
            queued_prompts: 1,
            ..quiet()
        };
        assert_eq!(queued.reason_after(&before), ActivityReason::QueueChanged);
    }

    #[test]
    fn wire_strings_are_stable() {
        let all = [
            (ActivityReason::WaitingChanged, "waiting_changed"),
            (ActivityReason::TurnStarted, "turn_started"),
            (ActivityReason::TurnEnded, "turn_ended"),
            (ActivityReason::AgentStarted, "agent_started"),
            (ActivityReason::AgentEnded, "agent_ended"),
            (ActivityReason::JobStarted, "job_started"),
            (ActivityReason::JobEnded, "job_ended"),
            (ActivityReason::NotificationQueued, "notification_queued"),
            (
                ActivityReason::NotificationDelivered,
                "notification_delivered",
            ),
            (ActivityReason::QueueChanged, "queue_changed"),
            (ActivityReason::Clear, "clear"),
            (ActivityReason::Shutdown, "shutdown"),
        ];
        for (reason, wire) in all {
            assert_eq!(reason.as_str(), wire);
        }
    }

    #[test]
    fn scopes_and_claims_nest_and_a_claim_hands_over_to_a_scope() {
        let tracker = ActivityTracker::new();
        assert_eq!(tracker.turns(), 0);
        let claim = tracker.turn_claim();
        let outer = tracker.turn_scope();
        let inner = tracker.turn_scope();
        assert_eq!(tracker.turns(), 3);
        drop(claim);
        drop(inner);
        assert_eq!(tracker.turns(), 1, "the scope outlives the claim");
        drop(outer);
        assert_eq!(tracker.turns(), 0);
    }

    #[test]
    fn a_dropped_override_forces_one_publish_with_its_reason() {
        let tracker = ActivityTracker::new();
        let guard = tracker.override_reason(ActivityReason::Clear);
        assert_eq!(tracker.current_override(), Some(ActivityReason::Clear));
        assert_eq!(tracker.take_forced(), None);
        drop(guard);
        assert_eq!(tracker.current_override(), None);
        assert_eq!(tracker.take_forced(), Some(ActivityReason::Clear));
        assert_eq!(tracker.take_forced(), None);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "no turn scope")]
    fn an_unscoped_turn_trips_the_assertion_once_scopes_are_required() {
        let tracker = ActivityTracker::new();
        tracker.assert_scoped("a turn");
        tracker.require_scopes();
        tracker.assert_scoped("a turn");
    }

    #[test]
    fn a_scoped_turn_passes_the_assertion() {
        let tracker = ActivityTracker::new();
        tracker.require_scopes();
        let _scope = tracker.turn_scope();
        tracker.assert_scoped("a turn");
    }
}
