//! The session-wide pause for a spent usage window. Every provider call waits at a gate while
//! it is in force, so the main agent, the guardian and every subagent stop together and nothing
//! pending is decided in the meantime. Fail-closed: the pause always ends, by the reset, a
//! probe, the human, or a deadline, and a call that waited out a failed pause is denied.
//!
//! The state lives in a `watch` channel and the clock in a task the pause itself owns, because
//! callers' futures are dropped (Esc, a killed subagent, headless `kill_all`) and a waiter
//! must never be what keeps the others waiting.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::quota::{Quota, QuotaSource, UsageRole};
use fontaine_tools::lock::lock;

/// Past the reset time before the first probe, because a window rarely resets to the second.
pub const RESET_MARGIN: Duration = Duration::from_secs(30);
/// The first probe interval when no reset time is known; it doubles from here.
pub const FIRST_PROBE_INTERVAL: Duration = Duration::from_secs(60);
pub const MAX_PROBE_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// How long a caller that was let through may hold the probe without an answer before another
/// is chosen.
pub const PROBE_LEASE: Duration = Duration::from_secs(2 * 60);
/// A quota fetch that has not answered by now says nothing.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// A pause for the same account that starts this soon after one ended continues its backoff
/// instead of starting over, so a probe that proved nothing does not repeat at once.
const BACKOFF_MEMORY: Duration = Duration::from_secs(15 * 60);

/// Why a pause ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseEnd {
    /// The reset time passed, or the account's own usage figures show room again.
    Reset,
    /// A request let through to try the account succeeded.
    Probed,
    /// The human chose to go on.
    Resumed,
    Cancelled,
    /// The deadline passed.
    CapReached,
}

impl PauseEnd {
    /// Whether the run must treat the end as a failure rather than carry on.
    pub fn is_failure(self) -> bool {
        matches!(self, PauseEnd::Cancelled | PauseEnd::CapReached)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PauseEnd::Reset => "reset",
            PauseEnd::Probed => "probed",
            PauseEnd::Resumed => "resumed",
            PauseEnd::Cancelled => "cancelled",
            PauseEnd::CapReached => "cap_reached",
        }
    }
}

/// What is paused, and until when.
#[derive(Clone, Debug)]
pub struct PauseInfo {
    pub role: UsageRole,
    pub account: String,
    pub reset_at: Option<SystemTime>,
    pub since: Instant,
    /// `None` waits for as long as it takes; only an interactive session, where the human can
    /// cancel, leaves it so.
    pub deadline: Option<Instant>,
    /// Where the account's own usage figures are read, to learn the reset time and to check it
    /// without spending a request.
    pub source: Option<Arc<dyn QuotaSource>>,
}

impl PauseInfo {
    pub fn new(role: UsageRole, account: impl Into<String>, reset_at: Option<SystemTime>) -> Self {
        PauseInfo {
            role,
            account: account.into(),
            reset_at,
            since: Instant::now(),
            deadline: None,
            source: None,
        }
    }

    pub fn with_deadline(mut self, wait: Option<Duration>) -> Self {
        self.deadline = wait.and_then(|wait| self.since.checked_add(wait));
        self
    }

    pub fn with_source(mut self, source: Option<Arc<dyn QuotaSource>>) -> Self {
        self.source = source;
        self
    }
}

/// What a surface is told as the pause begins and ends.
#[derive(Clone, Debug)]
pub enum PauseEvent {
    Paused(PauseInfo),
    Resumed {
        end: PauseEnd,
        info: PauseInfo,
        waited: Duration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    /// Not yet due.
    Idle,
    /// Due: the next caller to ask is let through.
    Open,
    /// A caller is trying the account.
    Claimed(Instant),
}

#[derive(Debug)]
struct Recent {
    account: String,
    ended: Instant,
    attempts: u32,
}

#[derive(Debug)]
struct State {
    epoch: u64,
    /// Bumped when the reset time or the schedule changes, so the driver replans.
    rev: u64,
    active: Option<PauseInfo>,
    probe: Probe,
    attempts: u32,
    closed: Duration,
    last_end: Option<PauseEnd>,
    terminal: Option<(PauseEnd, PauseInfo)>,
    recent: Option<Recent>,
    /// When the driver next looks at the account, for a surface to count down to. Written
    /// without waking anyone: it only informs a display.
    next_check: Option<Instant>,
}

type Observer = dyn Fn(&PauseEvent) + Send + Sync;

struct Shared {
    tx: watch::Sender<State>,
    observers: Mutex<Vec<Arc<Observer>>>,
}

impl Shared {
    fn announce(&self, event: PauseEvent) {
        let observers = lock(&self.observers, "usage pause observers").clone();
        for observer in observers {
            observer(&event);
        }
    }

    /// Ends the pause of generation `epoch` (any when `None`).
    fn end(&self, epoch: Option<u64>, end: PauseEnd) {
        let mut ended = None;
        self.tx.send_if_modified(|s| {
            if epoch.is_some_and(|g| g != s.epoch) {
                return false;
            }
            let Some(info) = s.active.take() else {
                return false;
            };
            let waited = info.since.elapsed();
            s.closed += waited;
            s.probe = Probe::Idle;
            s.last_end = Some(end);
            if end.is_failure() {
                s.terminal = Some((end, info.clone()));
            }
            s.recent = Some(Recent {
                account: info.account.clone(),
                ended: Instant::now(),
                attempts: s.attempts,
            });
            ended = Some((info, waited));
            // Nobody is woken yet: the observers hear of the end first, so a record or a stream
            // event of it precedes anything the released callers then do.
            false
        });
        if let Some((info, waited)) = ended {
            self.announce(PauseEvent::Resumed { end, info, waited });
            self.tx.send_modify(|_| {});
        }
    }
}

/// The session's usage pause. Cheap to clone; every clone is the same pause.
#[derive(Clone)]
pub struct UsagePause {
    shared: Arc<Shared>,
}

impl Default for UsagePause {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for UsagePause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsagePause")
            .field("paused", &self.info().is_some())
            .finish()
    }
}

/// The caller let through to try a paused account. If it is dropped without an answer, because
/// the run was stopped, the pause chooses another caller.
pub struct ProbeToken {
    shared: Arc<Shared>,
    epoch: u64,
    settled: bool,
}

impl ProbeToken {
    /// The request went through: the window is open again.
    pub fn succeeded(mut self) {
        self.settled = true;
        self.shared.end(Some(self.epoch), PauseEnd::Probed);
    }

    /// Leaves the answer to whatever notes the next success or limit, for a caller that cannot
    /// hold the token across its request. The probe lapses on its own if nothing answers.
    pub fn detach(mut self) {
        self.settled = true;
    }
}

impl Drop for ProbeToken {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let epoch = self.epoch;
        self.shared.tx.send_if_modified(|s| {
            if s.epoch == epoch && matches!(s.probe, Probe::Claimed(_)) {
                s.probe = Probe::Idle;
                s.rev += 1;
                true
            } else {
                false
            }
        });
    }
}

/// What the gate says to a caller.
pub struct Admission {
    /// `None` when nothing was paused.
    pub end: Option<PauseEnd>,
    /// Set when this caller was let through to try the paused account.
    pub probe: Option<ProbeToken>,
}

impl UsagePause {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(State {
            epoch: 0,
            rev: 0,
            active: None,
            probe: Probe::Idle,
            attempts: 0,
            closed: Duration::ZERO,
            last_end: None,
            terminal: None,
            recent: None,
            next_check: None,
        });
        UsagePause {
            shared: Arc::new(Shared {
                tx,
                observers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Registers a synchronous observer of each pause beginning and ending: the session's
    /// durable record, and a surface that streams them. It runs where the event happens, so
    /// events reach it in order and before anything the pause then releases.
    pub fn observe(&self, observer: Arc<Observer>) {
        lock(&self.shared.observers, "usage pause observers").push(observer);
    }

    /// The pause in force, if any.
    pub fn info(&self) -> Option<PauseInfo> {
        self.shared.tx.borrow().active.clone()
    }

    /// When the pause next probes the account, when it is waiting for that.
    pub fn next_check(&self) -> Option<Instant> {
        let state = self.shared.tx.borrow();
        state.active.as_ref().and(state.next_check)
    }

    pub fn is_paused(&self) -> bool {
        self.shared.tx.borrow().active.is_some()
    }

    /// A receiver that fires when the pause begins, changes or ends.
    pub fn watch(&self) -> watch::Receiver<()> {
        let mut rx = self.shared.tx.subscribe();
        let (tx, out) = watch::channel(());
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
        out
    }

    /// Puts the whole run on hold. Idempotent: the first engager wins. A request that failed
    /// while a probe was out is that probe's answer: the pause goes on with the reset time it
    /// brought.
    /// Returns whether this call began the pause: a failed probe that re-engages it is not a
    /// new pause, so it does not reopen a prompt the human may be answering.
    pub fn engage(&self, info: PauseInfo) -> bool {
        let mut started = None;
        self.shared.tx.send_if_modified(|s| {
            if let Some(active) = &mut s.active {
                if matches!(s.probe, Probe::Claimed(_)) {
                    if info.reset_at.is_some() {
                        active.reset_at = info.reset_at;
                    }
                    s.probe = Probe::Idle;
                    s.attempts += 1;
                    s.rev += 1;
                    return true;
                }
                return false;
            }
            s.epoch += 1;
            s.rev += 1;
            s.probe = Probe::Idle;
            s.attempts = match &s.recent {
                Some(recent)
                    if recent.account == info.account
                        && recent.ended.elapsed() < BACKOFF_MEMORY =>
                {
                    recent.attempts + 1
                }
                _ => 0,
            };
            s.terminal = None;
            s.last_end = None;
            s.active = Some(info.clone());
            started = Some(s.epoch);
            true
        });
        if let Some(epoch) = started {
            self.shared.announce(PauseEvent::Paused(info));
            tokio::spawn(drive(self.shared.clone(), epoch));
        }
        started.is_some()
    }

    /// Resolves when no pause is in force. Unlike [`wait_open`](Self::wait_open) it is never let
    /// through as a probe, so a surface can watch for the end without taking part.
    pub async fn closed(&self) {
        let mut rx = self.shared.tx.subscribe();
        while rx.borrow_and_update().active.is_some() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Waits while a pause is in force. `None` when none was. The caller treats a
    /// [`PauseEnd::is_failure`] end as the run failing, and one that ended so earlier and was
    /// not cleared since still counts.
    pub async fn wait_open(&self) -> Option<PauseEnd> {
        let admission = self.admit().await;
        if let Some(token) = admission.probe {
            token.detach();
        }
        admission.end
    }

    /// [`wait_open`](Self::wait_open) for a caller that can hold the probe across its request.
    pub async fn admit(&self) -> Admission {
        self.admit_with(true).await
    }

    /// Waits like [`admit`](Self::admit) but leaves an open probe for the caller's own retry to
    /// claim, so the request that follows is not itself held at the gate for the lease.
    pub async fn wait_ready(&self) -> Option<PauseEnd> {
        self.admit_with(false).await.end
    }

    async fn admit_with(&self, claim: bool) -> Admission {
        let mut rx = self.shared.tx.subscribe();
        {
            // A pause that ended in failure still stops the run: calls after it fail the same
            // way until a new turn clears it, rather than slipping through the gap.
            let state = rx.borrow();
            if state.active.is_none() {
                return Admission {
                    end: state.terminal.as_ref().map(|(end, _)| *end),
                    probe: None,
                };
            }
        }
        loop {
            if !claim && self.shared.tx.borrow().probe == Probe::Open {
                return Admission {
                    end: Some(PauseEnd::Probed),
                    probe: None,
                };
            }
            let mut claimed = None;
            self.shared.tx.send_if_modified(|s| {
                if claim && s.active.is_some() && s.probe == Probe::Open {
                    s.probe = Probe::Claimed(Instant::now());
                    claimed = Some(s.epoch);
                    true
                } else {
                    false
                }
            });
            if let Some(epoch) = claimed {
                return Admission {
                    end: Some(PauseEnd::Probed),
                    probe: Some(ProbeToken {
                        shared: self.shared.clone(),
                        epoch,
                        settled: false,
                    }),
                };
            }
            {
                let state = rx.borrow_and_update();
                if state.active.is_none() {
                    return Admission {
                        end: state.last_end.or(Some(PauseEnd::Reset)),
                        probe: None,
                    };
                }
            }
            if rx.changed().await.is_err() {
                return Admission {
                    end: Some(PauseEnd::Cancelled),
                    probe: None,
                };
            }
        }
    }

    /// A request to the provider succeeded. If a probe was out, the window is open.
    pub fn note_success(&self) {
        let (claimed, epoch) = {
            let s = self.shared.tx.borrow();
            (
                s.active.is_some() && matches!(s.probe, Probe::Claimed(_)),
                s.epoch,
            )
        };
        if claimed {
            self.shared.end(Some(epoch), PauseEnd::Probed);
        }
    }

    /// Bounds a pause that had no deadline, `wait` from now. An overflowing wait leaves it as it
    /// was.
    pub fn set_deadline(&self, wait: Duration) {
        self.shared.tx.send_if_modified(|s| {
            let Some(info) = s.active.as_mut().filter(|i| i.deadline.is_none()) else {
                return false;
            };
            let Some(at) = Instant::now().checked_add(wait) else {
                return false;
            };
            info.deadline = Some(at);
            s.rev += 1;
            true
        });
    }

    /// The human chose to go on now. Waiting callers resume; the first to meet the limit again
    /// pauses the run anew.
    pub fn resume(&self) {
        self.shared.end(None, PauseEnd::Resumed);
    }

    pub fn cancel(&self) {
        self.shared.end(None, PauseEnd::Cancelled);
    }

    /// How the most recent pause ended when it ended in failure, until the next one begins.
    /// A run that was stopped for it reads this to say why.
    pub fn failure(&self) -> Option<(PauseEnd, PauseInfo)> {
        self.shared.tx.borrow().terminal.clone()
    }

    /// Clears [`failure`](Self::failure), as a new turn starting does.
    pub fn clear_failure(&self) {
        self.shared
            .tx
            .send_if_modified(|s| s.terminal.take().is_some());
    }

    /// Time spent paused so far, the one in force included. Wall-clock clocks subtract it.
    pub fn total_paused(&self) -> Duration {
        let s = self.shared.tx.borrow();
        s.closed
            + s.active
                .as_ref()
                .map_or(Duration::ZERO, |i| i.since.elapsed())
    }
}

/// `2h 14m`, `4m`, `30s`: a wait for a person to read, rounded up to the unit shown.
pub fn format_wait(wait: Duration) -> String {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    if secs < 60 {
        return format!("{secs}s");
    }
    let minutes = secs.div_ceil(60);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// When the pause says the window resets, as a clock time and the wait to it: `resets 14:30 (in
/// 2h 14m)`, or that the time is unknown.
pub fn describe_reset(reset_at: Option<SystemTime>, now: SystemTime) -> String {
    match reset_at {
        Some(at) => {
            let clock = chrono::DateTime::<chrono::Local>::from(at).format("%H:%M");
            match at.duration_since(now) {
                Ok(wait) if !wait.is_zero() => {
                    format!("resets {clock} (in {})", format_wait(wait))
                }
                _ => format!("resets {clock} (due now)"),
            }
        }
        None => "reset time unknown".to_string(),
    }
}

/// The line a surface shows while paused: `paused — work usage window spent, resets 14:30 (in
/// 2h 14m)`, or, with no reset time, when the account is next checked.
pub fn status_line(info: &PauseInfo, next_check: Option<Instant>, now: SystemTime) -> String {
    let when = match (info.reset_at, next_check) {
        (None, Some(next)) => format!(
            "reset time unknown, next check in {}",
            format_wait(next.saturating_duration_since(Instant::now()))
        ),
        (reset, _) => describe_reset(reset, now),
    };
    format!("paused — {} usage window spent, {when}", info.account)
}

/// The soonest reset among the windows that are spent.
pub fn spent_reset(quota: &Quota) -> Option<SystemTime> {
    quota
        .windows
        .iter()
        .filter(|w| w.used_ratio >= 1.0)
        .filter_map(|w| w.reset_at)
        .min()
}

fn backoff(attempts: u32) -> Duration {
    (FIRST_PROBE_INTERVAL * 2u32.saturating_pow(attempts.min(10))).min(MAX_PROBE_INTERVAL)
}

/// When the next probe is due for a pause that has failed `attempts` probes already.
fn schedule(info: &PauseInfo, attempts: u32) -> Instant {
    let now = Instant::now();
    let paced = now + backoff(attempts.saturating_sub(1));
    match info.reset_at {
        Some(at) => {
            let wait = at.duration_since(SystemTime::now()).unwrap_or_default();
            let due = now + wait + RESET_MARGIN;
            if attempts == 0 { due } else { due.max(paced) }
        }
        None => now + backoff(attempts),
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// What the driver reads off the state each round.
struct Round {
    info: PauseInfo,
    probe: Probe,
    rev: u64,
    attempts: u32,
}

fn round(shared: &Shared, epoch: u64) -> Option<Round> {
    let s = shared.tx.borrow();
    if s.epoch != epoch {
        return None;
    }
    Some(Round {
        info: s.active.clone()?,
        probe: s.probe,
        rev: s.rev,
        attempts: s.attempts,
    })
}

async fn fetch(source: &Arc<dyn QuotaSource>) -> Option<Quota> {
    tokio::time::timeout(FETCH_TIMEOUT, source.fetch())
        .await
        .ok()?
        .ok()
}

/// The clock of one pause. Owned by the pause, not by any waiter.
async fn drive(shared: Arc<Shared>, epoch: u64) {
    let mut rx = shared.tx.subscribe();
    let mut seen_rev = 0;
    let mut due = None;
    let mut learned = false;
    loop {
        let Some(now) = round(&shared, epoch) else {
            return;
        };
        if !learned {
            learned = true;
            if now.info.reset_at.is_none()
                && let Some(source) = &now.info.source
                && let Some(quota) = fetch(source).await
                && let Some(at) = spent_reset(&quota)
            {
                shared.tx.send_if_modified(|s| {
                    if s.epoch != epoch {
                        return false;
                    }
                    match &mut s.active {
                        Some(info) => {
                            info.reset_at = Some(at);
                            s.rev += 1;
                            true
                        }
                        None => false,
                    }
                });
                continue;
            }
        }
        if now.rev != seen_rev {
            seen_rev = now.rev;
            due = Some(schedule(&now.info, now.attempts));
        }
        let shown = (now.probe == Probe::Idle).then_some(due).flatten();
        shared.tx.send_if_modified(|s| {
            if s.epoch == epoch {
                s.next_check = shown;
            }
            false
        });
        let deadline = now.info.deadline;
        if deadline.is_some_and(|d| d <= Instant::now()) {
            shared.end(Some(epoch), PauseEnd::CapReached);
            return;
        }
        let wake = match now.probe {
            Probe::Claimed(at) => Some(at + PROBE_LEASE),
            Probe::Open => None,
            Probe::Idle => due,
        };
        let woke = tokio::select! {
            _ = sleep_until_opt(wake) => true,
            _ = sleep_until_opt(deadline) => {
                shared.end(Some(epoch), PauseEnd::CapReached);
                return;
            }
            changed = rx.changed() => {
                if changed.is_err() {
                    return;
                }
                false
            }
        };
        if !woke {
            continue;
        }
        match now.probe {
            Probe::Claimed(_) => {
                // Nobody answered in time: the lease lapses and the next round probes again.
                shared.tx.send_if_modified(|s| {
                    if s.epoch == epoch && matches!(s.probe, Probe::Claimed(_)) {
                        s.probe = Probe::Idle;
                        s.attempts += 1;
                        s.rev += 1;
                        true
                    } else {
                        false
                    }
                });
            }
            Probe::Idle => check_source(&shared, epoch, &now.info).await,
            Probe::Open => {}
        }
    }
}

/// The probe is due. Reads the account's own figures when it has them, which costs no request;
/// otherwise opens the gate so the next real call is the probe.
async fn check_source(shared: &Shared, epoch: u64, info: &PauseInfo) {
    let quota = match &info.source {
        Some(source) => fetch(source).await,
        None => None,
    };
    match quota {
        Some(quota) if !quota.windows.is_empty() => {
            if !quota.windows.iter().any(|w| w.used_ratio >= 1.0) {
                shared.end(Some(epoch), PauseEnd::Reset);
                return;
            }
            let reset = spent_reset(&quota);
            shared.tx.send_if_modified(|s| {
                if s.epoch != epoch || s.active.is_none() {
                    return false;
                }
                s.attempts += 1;
                s.rev += 1;
                if let (Some(info), Some(at)) = (&mut s.active, reset) {
                    info.reset_at = Some(at);
                }
                true
            });
        }
        _ => {
            shared.tx.send_if_modified(|s| {
                if s.epoch == epoch && s.active.is_some() && s.probe == Probe::Idle {
                    s.probe = Probe::Open;
                    true
                } else {
                    false
                }
            });
        }
    }
}

#[cfg(test)]
mod tests;
