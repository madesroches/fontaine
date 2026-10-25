//! The session's scheduled prompts: `cron_create` tasks, `/loop` iterations and the wakeups of a
//! self-paced loop. The scheduler only decides what is due; the surface runs it as a turn the
//! harness started, which is not a user message, so what the human asked for stays the only
//! intent the guardian reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveDateTime, TimeDelta};
use chrono::{TimeZone, Timelike};
use fontaine_tools::error_chain::chain;
use fontaine_tools::lock::{lock, lock_recovering};
use fontaine_tools::schedule::{
    JobInfo, MAX_WAKE_SECONDS, MIN_WAKE_SECONDS, NewJob, ScheduleStore, WakeOutcome, WakeRequest,
};
pub use fontaine_tools::schedule::{MAX_JOBS, MAX_PROMPT_CHARS, ScheduleError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ledger::{Ledger, Record, ScheduleEvent};
use crate::owner_only;
use crate::permissions::Approver;

pub const SCHEDULE_FILE: &str = "schedule.json";

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// How long after an iteration that scheduled nothing a self-paced loop tries once more.
pub const FALLBACK_SECONDS: u64 = 20 * 60;
/// How far ahead a cron expression is searched for a match before it counts as never firing.
const SEARCH_YEARS: i32 = 5;

/// The time source, so tests can move time without sleeping.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(now_ms: i64) -> ManualClock {
        ManualClock(AtomicI64::new(now_ms))
    }

    pub fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, by: Duration) {
        let ms = i64::try_from(by.as_millis()).expect("a test advances by a sane duration");
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn local(ms: i64) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .expect("an instant has exactly one local time")
}

/// A time as the user reads it, in their local zone.
pub fn local_time(ms: i64) -> String {
    local(ms).format("%Y-%m-%d %H:%M").to_string()
}

/// A parsed 5-field cron expression: `*`, values, `a-b`, `*/n`, `a-b/n` and lists, with Sunday
/// as 0 or 7. A day-of-month and a day-of-week that are both restricted match on either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    text: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    days_unrestricted: bool,
    weekdays_unrestricted: bool,
}

struct Field {
    mask: u64,
    unrestricted: bool,
}

fn parse_number(text: &str, name: &str) -> Result<u32, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{text}` is not a number in the {name} field"));
    }
    text.parse()
        .map_err(|_| format!("`{text}` is too large in the {name} field"))
}

fn parse_field(text: &str, min: u32, max: u32, name: &str) -> Result<Field, String> {
    let mut mask = 0u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step = parse_number(step, name)?;
                if step == 0 {
                    return Err(format!("the step in the {name} field must be at least 1"));
                }
                (range, Some(step))
            }
            None => (item, None),
        };
        let (low, high) = if range == "*" {
            (min, if name == "day-of-week" { 6 } else { max })
        } else if let Some((low, high)) = range.split_once('-') {
            (parse_number(low, name)?, parse_number(high, name)?)
        } else {
            let value = parse_number(range, name)?;
            if step.is_some() {
                return Err(format!(
                    "a step in the {name} field needs `*` or a range, not `{range}`"
                ));
            }
            (value, value)
        };
        if low < min || high > max || low > high || step.is_some_and(|step| step > max) {
            return Err(format!(
                "`{item}` is outside {min}-{max} in the {name} field"
            ));
        }
        let mut value = low;
        while value <= high {
            mask |= 1 << value;
            value += step.unwrap_or(1);
        }
    }
    if name == "day-of-week" && mask & (1 << 7) != 0 {
        mask = (mask & !(1 << 7)) | 1;
    }
    Ok(Field {
        mask,
        unrestricted: text.starts_with('*'),
    })
}

impl Cron {
    pub fn parse(text: &str) -> Result<Cron, ScheduleError> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(ScheduleError::BadCron(format!(
                "expected 5 fields (minute hour day-of-month month day-of-week), got {}",
                fields.len()
            )));
        };
        let bad = ScheduleError::BadCron;
        let minutes = parse_field(minute, 0, 59, "minute").map_err(bad)?;
        let hours = parse_field(hour, 0, 23, "hour").map_err(bad)?;
        let days = parse_field(day, 1, 31, "day-of-month").map_err(bad)?;
        let months = parse_field(month, 1, 12, "month").map_err(bad)?;
        let weekdays = parse_field(weekday, 0, 7, "day-of-week").map_err(bad)?;
        Ok(Cron {
            text: fields.join(" "),
            minutes: minutes.mask,
            hours: hours.mask,
            days: days.mask,
            months: months.mask,
            weekdays: weekdays.mask,
            days_unrestricted: days.unrestricted,
            weekdays_unrestricted: weekdays.unrestricted,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn day_matches(&self, date: NaiveDate) -> bool {
        let day = self.days & (1 << date.day()) != 0;
        let weekday = self.weekdays & (1 << date.weekday().num_days_from_sunday()) != 0;
        if !self.days_unrestricted && !self.weekdays_unrestricted {
            day || weekday
        } else {
            day && weekday
        }
    }

    /// The first matching local minute at or after `from`, as a wall-clock time.
    fn first_match(
        &self,
        mut t: NaiveDateTime,
        last_year: i32,
    ) -> Result<NaiveDateTime, ScheduleError> {
        let never = || ScheduleError::NeverFires;
        loop {
            if t.year() > last_year {
                return Err(never());
            }
            if self.months & (1 << t.month()) == 0 {
                let (year, month) = if t.month() == 12 {
                    (t.year() + 1, 1)
                } else {
                    (t.year(), t.month() + 1)
                };
                t = NaiveDate::from_ymd_opt(year, month, 1)
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .ok_or_else(never)?;
            } else if !self.day_matches(t.date()) {
                t = t
                    .date()
                    .succ_opt()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .ok_or_else(never)?;
            } else if self.hours & (1 << t.hour()) == 0 {
                t = t
                    .with_minute(0)
                    .and_then(|t| t.checked_add_signed(TimeDelta::hours(1)))
                    .ok_or_else(never)?;
            } else if self.minutes & (1 << t.minute()) == 0 {
                t = t
                    .checked_add_signed(TimeDelta::minutes(1))
                    .ok_or_else(never)?;
            } else {
                return Ok(t);
            }
        }
    }

    /// The next matching minute after `after`, in `after`'s zone. A local time the zone skips
    /// (a clock change forward) is passed over; one it repeats fires on the first occurrence.
    /// An expression that matches nothing within 5 years is an error.
    pub fn next_after<Tz: TimeZone>(
        &self,
        after: &DateTime<Tz>,
    ) -> Result<DateTime<Tz>, ScheduleError> {
        let zone = after.timezone();
        let wall = after.naive_local();
        let last_year = wall.year() + SEARCH_YEARS;
        let mut t = wall
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .and_then(|t| t.checked_add_signed(TimeDelta::minutes(1)))
            .ok_or(ScheduleError::NeverFires)?;
        loop {
            t = self.first_match(t, last_year)?;
            let found = match zone.from_local_datetime(&t) {
                LocalResult::Single(at) => Some(at).filter(|at| at > after),
                LocalResult::Ambiguous(first, second) => Some(first)
                    .filter(|at| at > after)
                    .or(Some(second).filter(|at| at > after)),
                LocalResult::None => None,
            };
            if let Some(at) = found {
                return Ok(at);
            }
            t = t
                .checked_add_signed(TimeDelta::minutes(1))
                .ok_or(ScheduleError::NeverFires)?;
        }
    }

    /// The cadence in words, for the cadences people write most; the expression otherwise.
    pub fn describe(&self) -> String {
        let fields: Vec<&str> = self.text.split(' ').collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return format!("cron `{}`", self.text);
        };
        let every_day = day == "*" && month == "*" && weekday == "*";
        let plural = |n: &str, unit: &str| {
            if n == "1" {
                format!("every {unit}")
            } else {
                format!("every {n} {unit}s")
            }
        };
        if every_day && hour == "*" {
            if minute == "*" {
                return "every minute".to_string();
            }
            if let Some(n) = minute
                .strip_prefix("*/")
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            {
                return plural(n, "minute");
            }
            if minute == "0" {
                return "every hour".to_string();
            }
        }
        if every_day
            && minute == "0"
            && let Some(n) = hour
                .strip_prefix("*/")
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        {
            return plural(n, "hour");
        }
        if minute == "0"
            && hour == "0"
            && month == "*"
            && weekday == "*"
            && let Some(n) = day
                .strip_prefix("*/")
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        {
            return plural(n, "day");
        }
        if every_day && let (Ok(m), Ok(h)) = (minute.parse::<u32>(), hour.parse::<u32>()) {
            return format!("daily at {h:02}:{m:02}");
        }
        format!("cron `{}`", self.text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The user typed it (`/loop`).
    User,
    /// The model scheduled it (`cron_create`).
    Model,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::User => "user",
            Origin::Model => "model",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Cron,
    /// The next iteration of a self-paced loop.
    Wakeup {
        loop_id: String,
        fallback: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// Five fields in local time; for a wakeup, the minute it was pinned to.
    pub cron: String,
    pub prompt: String,
    pub recurring: bool,
    pub origin: Origin,
    pub kind: Kind,
    pub created_ms: i64,
    pub next_ms: i64,
}

impl Job {
    /// The cadence in words.
    pub fn cadence(&self) -> String {
        match (&self.kind, self.recurring) {
            (Kind::Wakeup { .. }, _) => "self-paced loop".to_string(),
            (Kind::Cron, false) => format!("once at {}", local_time(self.next_ms)),
            (Kind::Cron, true) => Cron::parse(&self.cron)
                .map_or_else(|_| format!("cron `{}`", self.cron), |cron| cron.describe()),
        }
    }

    fn info(&self) -> JobInfo {
        JobInfo {
            id: self.id.clone(),
            cadence: self.cadence(),
            origin: match self.origin {
                Origin::User => "you",
                Origin::Model => "model",
            },
            next_run: local_time(self.next_ms),
            prompt: self.prompt.clone(),
        }
    }
}

/// What to schedule.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub cron: String,
    pub prompt: String,
    pub recurring: bool,
    pub origin: Origin,
}

/// A job that came due, as it was before it was re-armed or removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fired {
    pub job: Job,
    /// The recurring job reached its 7 days with this run and is gone.
    pub last_run: bool,
}

/// Tells the model it is in a self-paced loop, which only `schedule_wakeup` can keep going.
pub const SELF_PACED_NOTE: &str = "[Harness note: this is an iteration of a self-paced loop. When it is done, call `schedule_wakeup` with the delay before the next iteration, or with `stop: true` if the work is finished.]";

/// The self-paced loop the user started, which `schedule_wakeup` paces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveLoop {
    pub id: String,
    pub prompt: String,
}

/// What the end of a self-paced iteration led to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopStep {
    NoLoop,
    /// The model scheduled the next iteration.
    Continues,
    /// It did not, so one more is armed after [`FALLBACK_SECONDS`].
    FallbackArmed,
    /// It did not twice in a row, so the loop is over.
    Ended,
}

struct LoopState {
    active: ActiveLoop,
    last_was_fallback: bool,
}

#[derive(Default)]
struct State {
    jobs: Vec<Job>,
    active: Option<LoopState>,
}

pub struct Scheduler {
    path: PathBuf,
    ledger: Arc<Ledger>,
    clock: Arc<dyn Clock>,
    // Orders concurrent changes with their saves, so the file never lags a newer list.
    state: Mutex<State>,
    warning: Mutex<Option<String>>,
    expired: Mutex<Vec<Job>>,
}

fn validate_prompt(prompt: &str) -> Result<(), ScheduleError> {
    if prompt.trim().is_empty() {
        return Err(ScheduleError::EmptyPrompt);
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(ScheduleError::PromptTooLong);
    }
    Ok(())
}

fn fresh_id(taken: &[Job]) -> String {
    loop {
        let id = Uuid::new_v4().simple().to_string()[..8].to_string();
        if taken.iter().all(|job| job.id != id) {
            return id;
        }
    }
}

/// The minute a wakeup fires at, as the five fields of a cron expression.
fn pinned_minute(ms: i64) -> String {
    let at = local(ms);
    format!(
        "{} {} {} {} *",
        at.minute(),
        at.hour(),
        at.day(),
        at.month()
    )
}

impl Scheduler {
    /// A scheduler with no tasks.
    pub fn new(session_dir: &Path, ledger: Arc<Ledger>, clock: Arc<dyn Clock>) -> Scheduler {
        Scheduler {
            path: session_dir.join(SCHEDULE_FILE),
            ledger,
            clock,
            state: Mutex::default(),
            warning: Mutex::new(None),
            expired: Mutex::new(Vec::new()),
        }
    }

    /// The saved tasks that are still worth running. Wakeups of a self-paced loop, recurring
    /// tasks past their 7 days and one-shots whose time passed are dropped (see
    /// [`settle_expired`](Self::settle_expired)), and the rest are re-armed from now, so nothing replays what was missed. An unreadable file leaves an empty
    /// scheduler and a warning for [`take_warning`](Self::take_warning).
    pub fn load(session_dir: &Path, ledger: Arc<Ledger>, clock: Arc<dyn Clock>) -> Scheduler {
        let scheduler = Scheduler::new(session_dir, ledger, clock);
        let saved = match std::fs::read(&scheduler.path) {
            Ok(bytes) => serde_json::from_slice::<Vec<Job>>(&bytes).map_err(|e| chain(&e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return scheduler,
            Err(e) => Err(chain(&e)),
        };
        let saved = match saved {
            Ok(saved) => saved,
            Err(reason) => {
                scheduler.warn(format!(
                    "the saved schedule {} was not loaded: {reason}",
                    scheduler.path.display()
                ));
                return scheduler;
            }
        };
        let now = scheduler.clock.now_ms();
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for mut job in saved {
            let alive = match job.kind {
                Kind::Wakeup { .. } => false,
                Kind::Cron if !job.recurring => job.next_ms > now,
                Kind::Cron => {
                    now < job.created_ms + WEEK_MS
                        && Cron::parse(&job.cron)
                            .and_then(|cron| cron.next_after(&local(now)))
                            .map(|next| job.next_ms = next.timestamp_millis())
                            .is_ok()
                }
            };
            if alive {
                kept.push(job);
            } else {
                dropped.push(job);
            }
        }
        // Held back until the session knows scheduling is on: a session with it off leaves
        // what was saved as it was, and says nothing about it.
        lock(&scheduler.state, "schedule").jobs = kept;
        *lock(&scheduler.expired, "schedule") = dropped;
        scheduler
    }

    /// Makes the drops of [`load`](Self::load) final: the file is rewritten without them, each is
    /// recorded, and one warning counts them. A later resume then finds nothing to repeat.
    pub fn settle_expired(&self) {
        let dropped = std::mem::take(&mut *lock(&self.expired, "schedule"));
        if dropped.is_empty() {
            return;
        }
        let saved = {
            let state = lock(&self.state, "schedule");
            self.save(&state.jobs)
        };
        if let Err(e) = saved {
            self.warn(chain(&e));
        }
        for job in &dropped {
            self.record(ScheduleEvent::Expired, job);
        }
        let n = dropped.len();
        self.warn(format!(
            "{n} scheduled task{} expired while the session was closed",
            if n == 1 { "" } else { "s" }
        ));
    }

    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    fn warn(&self, message: String) {
        // Advisory text shown to the human once; a half-written string is harmless.
        let mut slot = lock_recovering(&self.warning);
        match slot.as_mut() {
            Some(existing) => {
                existing.push('\n');
                existing.push_str(&message);
            }
            None => *slot = Some(message),
        }
    }

    /// What went wrong without a caller to tell, once.
    pub fn take_warning(&self) -> Option<String> {
        lock_recovering(&self.warning).take()
    }

    fn save(&self, jobs: &[Job]) -> Result<(), ScheduleError> {
        let text = serde_json::to_vec(jobs).expect("a list of plain jobs serializes");
        owner_only::write_atomic(&self.path, &text).map_err(|source| ScheduleError::Persist {
            path: self.path.clone(),
            source,
        })
    }

    fn record(&self, event: ScheduleEvent, job: &Job) {
        let result = self.ledger.append(Record::Scheduled {
            event,
            id: job.id.clone(),
            origin: job.origin.as_str().to_string(),
            cron: job.cron.clone(),
            recurring: job.recurring,
        });
        if let Err(e) = result {
            self.warn(format!(
                "the ledger could not record a scheduled task: {}",
                chain(&e)
            ));
        }
    }

    pub fn create(&self, spec: JobSpec) -> Result<Job, ScheduleError> {
        validate_prompt(&spec.prompt)?;
        let cron = Cron::parse(&spec.cron)?;
        let now = self.clock.now_ms();
        let next = cron.next_after(&local(now))?.timestamp_millis();
        let job = {
            let mut state = lock(&self.state, "schedule");
            self.insert(
                &mut state,
                spec.origin,
                Kind::Cron,
                cron.text().to_string(),
                spec.prompt,
                spec.recurring,
                now,
                next,
            )?
        };
        self.record(ScheduleEvent::Created, &job);
        Ok(job)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        state: &mut State,
        origin: Origin,
        kind: Kind,
        cron: String,
        prompt: String,
        recurring: bool,
        now: i64,
        next: i64,
    ) -> Result<Job, ScheduleError> {
        if state.jobs.len() >= MAX_JOBS {
            return Err(ScheduleError::TooMany);
        }
        let job = Job {
            id: fresh_id(&state.jobs),
            cron,
            prompt,
            recurring,
            origin,
            kind,
            created_ms: now,
            next_ms: next,
        };
        state.jobs.push(job.clone());
        if let Err(e) = self.save(&state.jobs) {
            state.jobs.pop();
            return Err(e);
        }
        Ok(job)
    }

    /// Every task, soonest first.
    pub fn list(&self) -> Vec<Job> {
        let mut jobs = lock(&self.state, "schedule").jobs.clone();
        jobs.sort_by_key(|job| job.next_ms);
        jobs
    }

    pub fn count(&self) -> usize {
        lock(&self.state, "schedule").jobs.len()
    }

    /// Cancels a task; false when there is none with this id.
    pub fn delete(&self, id: &str) -> Result<bool, ScheduleError> {
        let removed = {
            let mut state = lock(&self.state, "schedule");
            let Some(index) = state.jobs.iter().position(|job| job.id == id) else {
                return Ok(false);
            };
            let job = state.jobs.remove(index);
            // A loop whose only wakeup is gone would stay active yet never fire.
            let ended_loop = match (&job.kind, &state.active) {
                (Kind::Wakeup { loop_id, .. }, Some(active)) if *loop_id == active.active.id => {
                    state.active.take()
                }
                _ => None,
            };
            if let Err(e) = self.save(&state.jobs) {
                state.jobs.insert(index, job);
                state.active = state.active.take().or(ended_loop);
                return Err(e);
            }
            job
        };
        self.record(ScheduleEvent::Deleted, &removed);
        Ok(true)
    }

    /// Cancels every task and ends a self-paced loop. Returns how many tasks there were.
    pub fn clear(&self) -> Result<usize, ScheduleError> {
        let removed = {
            let mut state = lock(&self.state, "schedule");
            let previous = std::mem::take(&mut state.jobs);
            if let Err(e) = self.save(&[]) {
                state.jobs = previous;
                return Err(e);
            }
            state.active = None;
            previous
        };
        for job in &removed {
            self.record(ScheduleEvent::Deleted, job);
        }
        Ok(removed.len())
    }

    /// Takes the earliest task that is due. A recurring one is re-armed from now, so one that
    /// fell due while the model was busy runs once; a one-shot is removed, and so is a recurring
    /// one that has run for 7 days.
    pub fn due(&self) -> Option<Fired> {
        let now = self.clock.now_ms();
        let fired = {
            let mut state = lock(&self.state, "schedule");
            let index = state
                .jobs
                .iter()
                .enumerate()
                .filter(|(_, job)| job.next_ms <= now)
                .min_by_key(|(_, job)| job.next_ms)
                .map(|(index, _)| index)?;
            let job = state.jobs[index].clone();
            let last_run = job.recurring && now >= job.created_ms + WEEK_MS;
            let rearmed = (job.recurring && !last_run)
                .then(|| {
                    Cron::parse(&job.cron)
                        .and_then(|cron| cron.next_after(&local(now)))
                        .map(|next| next.timestamp_millis())
                        .ok()
                })
                .flatten();
            match rearmed {
                Some(next) => state.jobs[index].next_ms = next,
                None => {
                    state.jobs.remove(index);
                }
            }
            if let Err(e) = self.save(&state.jobs) {
                self.warn(format!("the schedule was not saved: {}", chain(&e)));
            }
            Fired { job, last_run }
        };
        self.record(ScheduleEvent::Fired, &fired.job);
        if fired.last_run {
            self.record(ScheduleEvent::Expired, &fired.job);
        }
        Some(fired)
    }

    /// When the next task is due, in epoch milliseconds.
    pub fn next_wake(&self) -> Option<i64> {
        lock(&self.state, "schedule")
            .jobs
            .iter()
            .map(|job| job.next_ms)
            .min()
    }

    /// How long until the next task is due; zero when one already is.
    pub fn until_next_wake(&self) -> Option<Duration> {
        let wake = self.next_wake()?;
        let ms = u64::try_from(wake - self.clock.now_ms()).unwrap_or(0);
        Some(Duration::from_millis(ms))
    }

    /// Starts a self-paced loop over `prompt`, which the user typed. Only one runs at a time.
    pub fn begin_loop(&self, prompt: &str) -> Result<ActiveLoop, ScheduleError> {
        validate_prompt(prompt)?;
        let mut state = lock(&self.state, "schedule");
        if state.active.is_some() {
            return Err(ScheduleError::LoopActive);
        }
        let active = ActiveLoop {
            id: fresh_id(&state.jobs),
            prompt: prompt.to_string(),
        };
        state.active = Some(LoopState {
            active: active.clone(),
            last_was_fallback: false,
        });
        Ok(active)
    }

    pub fn active_loop(&self) -> Option<ActiveLoop> {
        lock(&self.state, "schedule")
            .active
            .as_ref()
            .map(|state| state.active.clone())
    }

    /// Whether the self-paced loop has an iteration scheduled.
    pub fn wakeup_pending(&self) -> bool {
        let state = lock(&self.state, "schedule");
        state
            .active
            .as_ref()
            .is_some_and(|active| Self::pending_wakeup(&state.jobs, &active.active.id).is_some())
    }

    fn pending_wakeup<'a>(jobs: &'a [Job], loop_id: &str) -> Option<&'a Job> {
        jobs.iter()
            .find(|job| matches!(&job.kind, Kind::Wakeup { loop_id: id, .. } if id == loop_id))
    }

    /// Ends the self-paced loop and cancels its pending wakeup. False when none was running.
    pub fn end_loop(&self) -> Result<bool, ScheduleError> {
        let removed = {
            let mut state = lock(&self.state, "schedule");
            let Some(active) = state.active.take() else {
                return Ok(false);
            };
            match self.drop_wakeups(&mut state, &active.active.id) {
                Ok(removed) => removed,
                Err(e) => {
                    state.active = Some(active);
                    return Err(e);
                }
            }
        };
        for job in &removed {
            self.record(ScheduleEvent::Deleted, job);
        }
        Ok(true)
    }

    fn drop_wakeups(&self, state: &mut State, loop_id: &str) -> Result<Vec<Job>, ScheduleError> {
        let (removed, kept): (Vec<Job>, Vec<Job>) =
            std::mem::take(&mut state.jobs).into_iter().partition(
                |job| matches!(&job.kind, Kind::Wakeup { loop_id: id, .. } if id == loop_id),
            );
        state.jobs = kept;
        if !removed.is_empty()
            && let Err(e) = self.save(&state.jobs)
        {
            state.jobs.extend(removed);
            return Err(e);
        }
        Ok(removed)
    }

    /// Arms the next iteration of the self-paced loop `seconds` from now, replacing one that is
    /// pending. The loop's own prompt runs; nothing the caller supplies does.
    pub fn wake(&self, seconds: u64, fallback: bool) -> Result<Job, ScheduleError> {
        if !fallback && !(MIN_WAKE_SECONDS..=MAX_WAKE_SECONDS).contains(&seconds) {
            return Err(ScheduleError::BadDelay);
        }
        let (job, replaced) = {
            let mut state = lock(&self.state, "schedule");
            self.arm(&mut state, seconds, fallback)?
        };
        if let Some(old) = &replaced {
            self.record(ScheduleEvent::Deleted, old);
        }
        self.record(ScheduleEvent::Created, &job);
        Ok(job)
    }

    fn arm(
        &self,
        state: &mut State,
        seconds: u64,
        fallback: bool,
    ) -> Result<(Job, Option<Job>), ScheduleError> {
        let active = state
            .active
            .as_ref()
            .ok_or(ScheduleError::NoLoop)?
            .active
            .clone();
        let before = state.jobs.clone();
        let replaced = self.drop_wakeups_in_memory(state, &active.id);
        let now = self.clock.now_ms();
        let next = now + i64::try_from(seconds).unwrap_or(i64::MAX / 2) * 1000;
        let result = self.insert(
            state,
            Origin::User,
            Kind::Wakeup {
                loop_id: active.id,
                fallback,
            },
            pinned_minute(next),
            active.prompt,
            false,
            now,
            next,
        );
        match result {
            Ok(job) => Ok((job, replaced)),
            Err(e) => {
                state.jobs = before;
                Err(e)
            }
        }
    }

    fn drop_wakeups_in_memory(&self, state: &mut State, loop_id: &str) -> Option<Job> {
        let index = state.jobs.iter().position(
            |job| matches!(&job.kind, Kind::Wakeup { loop_id: id, .. } if id == loop_id),
        )?;
        Some(state.jobs.remove(index))
    }

    /// Called when an iteration of the self-paced loop has returned: sees whether the model
    /// scheduled the next one, and if not arms a fallback once, then ends the loop.
    pub fn iteration_ended(&self) -> Result<LoopStep, ScheduleError> {
        let mut ended = Vec::new();
        let mut armed = None;
        let step = {
            let mut state = lock(&self.state, "schedule");
            let Some(active) = state.active.as_ref() else {
                return Ok(LoopStep::NoLoop);
            };
            let id = active.active.id.clone();
            let was_fallback = active.last_was_fallback;
            if Self::pending_wakeup(&state.jobs, &id).is_some() {
                state
                    .active
                    .as_mut()
                    .expect("checked above")
                    .last_was_fallback = false;
                LoopStep::Continues
            } else if !was_fallback {
                armed = Some(self.arm(&mut state, FALLBACK_SECONDS, true)?.0);
                state
                    .active
                    .as_mut()
                    .expect("checked above")
                    .last_was_fallback = true;
                LoopStep::FallbackArmed
            } else {
                let previous = state.active.take();
                match self.drop_wakeups(&mut state, &id) {
                    Ok(removed) => ended = removed,
                    Err(e) => {
                        state.active = previous;
                        return Err(e);
                    }
                }
                LoopStep::Ended
            }
        };
        if let Some(job) = &armed {
            self.record(ScheduleEvent::Created, job);
        }
        for job in &ended {
            self.record(ScheduleEvent::Deleted, job);
        }
        Ok(step)
    }
}

impl ScheduleStore for Scheduler {
    fn create(&self, job: NewJob) -> Result<JobInfo, ScheduleError> {
        self.create(JobSpec {
            cron: job.cron,
            prompt: job.prompt,
            recurring: job.recurring,
            origin: Origin::Model,
        })
        .map(|job| job.info())
    }

    fn list(&self) -> Vec<JobInfo> {
        Scheduler::list(self).iter().map(Job::info).collect()
    }

    fn delete(&self, id: &str) -> Result<bool, ScheduleError> {
        Scheduler::delete(self, id)
    }

    fn wake(&self, request: WakeRequest) -> Result<WakeOutcome, ScheduleError> {
        match request {
            WakeRequest::Stop => {
                if self.end_loop()? {
                    Ok(WakeOutcome::Stopped)
                } else {
                    Err(ScheduleError::NoLoop)
                }
            }
            WakeRequest::After { seconds, .. } => self
                .wake(seconds, false)
                .map(|job| WakeOutcome::Armed(job.info())),
        }
    }
}

/// The store the tools are given: a failed save is also told to the human, not only to the model.
pub struct ReportedSchedule {
    pub scheduler: Arc<Scheduler>,
    pub approver: Arc<dyn Approver>,
}

impl ReportedSchedule {
    fn reported<T>(&self, result: Result<T, ScheduleError>) -> Result<T, ScheduleError> {
        if let Err(e @ ScheduleError::Persist { .. }) = &result {
            self.approver
                .warn(&format!("the schedule was not saved: {}", chain(e)));
        }
        result
    }
}

impl ScheduleStore for ReportedSchedule {
    fn create(&self, job: NewJob) -> Result<JobInfo, ScheduleError> {
        self.reported(ScheduleStore::create(self.scheduler.as_ref(), job))
    }

    fn list(&self) -> Vec<JobInfo> {
        ScheduleStore::list(self.scheduler.as_ref())
    }

    fn delete(&self, id: &str) -> Result<bool, ScheduleError> {
        self.reported(Scheduler::delete(&self.scheduler, id))
    }

    fn wake(&self, request: WakeRequest) -> Result<WakeOutcome, ScheduleError> {
        self.reported(ScheduleStore::wake(self.scheduler.as_ref(), request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<FixedOffset> {
        utc().with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn next(
        expr: &str,
        after: DateTime<FixedOffset>,
    ) -> Result<DateTime<FixedOffset>, ScheduleError> {
        Cron::parse(expr)?.next_after(&after)
    }

    fn assert_next(expr: &str, after: DateTime<FixedOffset>, expected: DateTime<FixedOffset>) {
        assert_eq!(next(expr, after).unwrap(), expected, "{expr} after {after}");
    }

    #[test]
    fn every_field_form_matches() {
        let start = at(2026, 1, 1, 10, 0);
        assert_next("* * * * *", start, at(2026, 1, 1, 10, 1));
        assert_next("5 * * * *", start, at(2026, 1, 1, 10, 5));
        assert_next(
            "*/15 * * * *",
            at(2026, 1, 1, 10, 15),
            at(2026, 1, 1, 10, 30),
        );
        assert_next("10-20/5 * * * *", start, at(2026, 1, 1, 10, 10));
        assert_next(
            "10-20/5 * * * *",
            at(2026, 1, 1, 10, 10),
            at(2026, 1, 1, 10, 15),
        );
        assert_next(
            "1,30,45 * * * *",
            at(2026, 1, 1, 10, 1),
            at(2026, 1, 1, 10, 30),
        );
        assert_next("0 9 * * *", start, at(2026, 1, 2, 9, 0));
        assert_next("0 */6 * * *", start, at(2026, 1, 1, 12, 0));
        assert_next("30 14 15 * *", start, at(2026, 1, 15, 14, 30));
        assert_next("0 0 1 3 *", start, at(2026, 3, 1, 0, 0));
        assert_next("0 0 1 1,7 *", start, at(2026, 7, 1, 0, 0));
    }

    #[test]
    fn the_next_minute_is_strictly_after() {
        let exact = at(2026, 1, 1, 10, 5);
        assert_next("5 10 * * *", exact, at(2026, 1, 2, 10, 5));
        let mid_minute = utc().with_ymd_and_hms(2026, 1, 1, 10, 4, 30).unwrap();
        assert_next("5 10 * * *", mid_minute, exact);
    }

    #[test]
    fn sunday_is_zero_or_seven() {
        // 2026-01-01 is a Thursday; the next Sunday is the 4th.
        let start = at(2026, 1, 1, 0, 0);
        assert_next("0 0 * * 0", start, at(2026, 1, 4, 0, 0));
        assert_next("0 0 * * 7", start, at(2026, 1, 4, 0, 0));
        assert_next("0 0 * * 5-7", start, at(2026, 1, 2, 0, 0));
        assert_next("0 0 * * 1-5", start, at(2026, 1, 2, 0, 0));
        assert_next("0 0 * * */2", start, at(2026, 1, 3, 0, 0));
    }

    #[test]
    fn day_of_month_and_day_of_week_match_on_either_when_both_are_restricted() {
        let start = at(2026, 1, 1, 0, 0);
        // The 15th, or a Monday.
        assert_next("0 0 15 * 1", start, at(2026, 1, 5, 0, 0));
        assert_next("0 0 15 * 1", at(2026, 1, 12, 0, 0), at(2026, 1, 15, 0, 0));
        // A star on one side leaves the other to decide.
        assert_next("0 0 15 * *", start, at(2026, 1, 15, 0, 0));
        assert_next("0 0 * * 1", start, at(2026, 1, 5, 0, 0));
    }

    #[test]
    fn the_search_rolls_over_months_and_years() {
        assert_next("0 0 1 1 *", at(2026, 1, 1, 0, 0), at(2027, 1, 1, 0, 0));
        assert_next(
            "59 23 31 12 *",
            at(2026, 1, 1, 0, 0),
            at(2026, 12, 31, 23, 59),
        );
        assert_next("0 0 * * *", at(2026, 12, 31, 12, 0), at(2027, 1, 1, 0, 0));
        assert_next("0 0 31 * *", at(2026, 1, 31, 0, 0), at(2026, 3, 31, 0, 0));
    }

    #[test]
    fn february_the_twenty_ninth_waits_for_a_leap_year() {
        assert_next("0 0 29 2 *", at(2026, 1, 1, 0, 0), at(2028, 2, 29, 0, 0));
        assert_next("0 0 29 2 *", at(2028, 3, 1, 0, 0), at(2032, 2, 29, 0, 0));
    }

    #[test]
    fn syntax_outside_the_five_numeric_fields_is_rejected() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "L * * * *",
            "0 0 L * *",
            "0 0 15W * *",
            "0 0 ? * *",
            "0 0 * JAN *",
            "0 0 * * MON",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * 32 * *",
            "* * * 0 *",
            "* * * 13 *",
            "* * * * 8",
            "5-1 * * * *",
            "*/0 * * * *",
            "5/10 * * * *",
            "-5 * * * *",
            "1,,2 * * * *",
            "*/x * * * *",
        ] {
            assert!(
                matches!(Cron::parse(bad), Err(ScheduleError::BadCron(_))),
                "`{bad}` should be rejected"
            );
        }
        assert!(Cron::parse("  0   9  *  * 1-5 ").is_ok());
    }

    #[test]
    fn an_expression_that_never_matches_is_an_error() {
        assert!(matches!(
            next("0 0 31 2 *", at(2026, 1, 1, 0, 0)),
            Err(ScheduleError::NeverFires)
        ));
        assert!(matches!(
            next("0 0 30 2 *", at(2026, 1, 1, 0, 0)),
            Err(ScheduleError::NeverFires)
        ));
    }

    /// Clocks run an hour ahead from 2026-03-29 02:00 UTC, so the local times 02:00-02:59 on that
    /// day do not exist.
    #[derive(Clone, Copy, Debug)]
    struct Spring;

    #[derive(Clone, Copy, Debug)]
    struct SpringOffset(i32);

    impl chrono::Offset for SpringOffset {
        fn fix(&self) -> FixedOffset {
            FixedOffset::east_opt(self.0).unwrap()
        }
    }

    impl Spring {
        fn gap() -> (NaiveDateTime, NaiveDateTime) {
            let day = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
            (
                day.and_hms_opt(2, 0, 0).unwrap(),
                day.and_hms_opt(3, 0, 0).unwrap(),
            )
        }
    }

    impl TimeZone for Spring {
        type Offset = SpringOffset;

        fn from_offset(_: &SpringOffset) -> Spring {
            Spring
        }

        fn offset_from_local_date(&self, date: &NaiveDate) -> LocalResult<SpringOffset> {
            self.offset_from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        }

        fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<SpringOffset> {
            let (start, end) = Spring::gap();
            if *local >= start && *local < end {
                LocalResult::None
            } else if *local < start {
                LocalResult::Single(SpringOffset(0))
            } else {
                LocalResult::Single(SpringOffset(3600))
            }
        }

        fn offset_from_utc_date(&self, date: &NaiveDate) -> SpringOffset {
            self.offset_from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> SpringOffset {
            let (start, _) = Spring::gap();
            SpringOffset(if *utc < start { 0 } else { 3600 })
        }
    }

    #[test]
    fn a_local_time_the_clock_skips_is_passed_over() {
        let before = Spring.with_ymd_and_hms(2026, 3, 28, 12, 0, 0).unwrap();
        let cron = Cron::parse("30 2 * * *").unwrap();
        let fired = cron.next_after(&before).unwrap();
        // 02:30 does not exist on the 29th, so the run is the next day's.
        assert_eq!(
            fired.naive_local(),
            NaiveDate::from_ymd_opt(2026, 3, 30)
                .unwrap()
                .and_hms_opt(2, 30, 0)
                .unwrap()
        );
        // A time either side of the gap is unaffected.
        let after_gap = Cron::parse("30 3 * * *")
            .unwrap()
            .next_after(&before)
            .unwrap();
        assert_eq!(after_gap.naive_local().day(), 29);
    }

    #[test]
    fn describe_words_the_common_cadences() {
        let d = |expr: &str| Cron::parse(expr).unwrap().describe();
        assert_eq!(d("* * * * *"), "every minute");
        assert_eq!(d("*/5 * * * *"), "every 5 minutes");
        assert_eq!(d("*/1 * * * *"), "every minute");
        assert_eq!(d("0 * * * *"), "every hour");
        assert_eq!(d("0 */2 * * *"), "every 2 hours");
        assert_eq!(d("30 9 * * *"), "daily at 09:30");
        assert_eq!(d("0 0 */3 * *"), "every 3 days");
        assert_eq!(d("0 9 * * 1-5"), "cron `0 9 * * 1-5`");
    }

    // ---- Scheduler ----

    struct Rig {
        dir: tempfile::TempDir,
        clock: Arc<ManualClock>,
        ledger: Arc<Ledger>,
        scheduler: Scheduler,
    }

    // 2026-01-15 12:00:00 UTC; local minutes are aligned in every zone whose offset is a whole
    // number of quarter hours.
    const START_MS: i64 = 1_768_478_400_000;

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(START_MS));
        let ledger = Arc::new(Ledger::create(dir.path()).unwrap());
        let scheduler = Scheduler::new(dir.path(), ledger.clone(), clock.clone());
        Rig {
            dir,
            clock,
            ledger,
            scheduler,
        }
    }

    impl Rig {
        fn reopen(&self) -> Scheduler {
            Scheduler::load(self.dir.path(), self.ledger.clone(), self.clock.clone())
        }

        fn events(&self) -> Vec<(ScheduleEvent, String)> {
            self.ledger
                .read()
                .unwrap()
                .into_iter()
                .filter_map(|entry| match entry.record {
                    Record::Scheduled { event, id, .. } => Some((event, id)),
                    _ => None,
                })
                .collect()
        }
    }

    fn spec(cron: &str, recurring: bool) -> JobSpec {
        JobSpec {
            cron: cron.to_string(),
            prompt: "check the build".to_string(),
            recurring,
            origin: Origin::Model,
        }
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn create_list_and_delete() {
        let rig = rig();
        let a = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        let b = rig.scheduler.create(spec("* * * * *", true)).unwrap();
        assert_eq!(a.id.len(), 8);
        assert!(
            a.id.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(rig.scheduler.count(), 2);
        let listed = rig.scheduler.list();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].next_ms <= listed[1].next_ms);
        assert!(rig.scheduler.delete(&a.id).unwrap());
        assert!(!rig.scheduler.delete(&a.id).unwrap());
        assert_eq!(rig.scheduler.list()[0].id, b.id);
        assert_eq!(
            rig.events(),
            vec![
                (ScheduleEvent::Created, a.id.clone()),
                (ScheduleEvent::Created, b.id.clone()),
                (ScheduleEvent::Deleted, a.id),
            ]
        );
    }

    #[test]
    fn limits_and_bad_input_are_refused_without_changing_anything() {
        let rig = rig();
        for n in 0..MAX_JOBS {
            rig.scheduler
                .create(spec("* * * * *", true))
                .unwrap_or_else(|e| panic!("{n}: {e}"));
        }
        assert!(matches!(
            rig.scheduler.create(spec("* * * * *", true)),
            Err(ScheduleError::TooMany)
        ));
        assert_eq!(rig.scheduler.count(), MAX_JOBS);

        let rig = self::rig();
        let mut long = spec("* * * * *", true);
        long.prompt = "x".repeat(MAX_PROMPT_CHARS + 1);
        assert!(matches!(
            rig.scheduler.create(long),
            Err(ScheduleError::PromptTooLong)
        ));
        let mut exact = spec("* * * * *", true);
        exact.prompt = "x".repeat(MAX_PROMPT_CHARS);
        assert!(rig.scheduler.create(exact).is_ok());
        let mut blank = spec("* * * * *", true);
        blank.prompt = "  \n".to_string();
        assert!(matches!(
            rig.scheduler.create(blank),
            Err(ScheduleError::EmptyPrompt)
        ));
        assert!(matches!(
            rig.scheduler.create(spec("0 0 31 2 *", false)),
            Err(ScheduleError::NeverFires)
        ));
        assert!(matches!(
            rig.scheduler.create(spec("nope", true)),
            Err(ScheduleError::BadCron(_))
        ));
        assert_eq!(rig.scheduler.count(), 1);
    }

    #[test]
    fn a_job_is_due_only_once_its_time_has_come() {
        let rig = rig();
        let job = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        assert!(rig.scheduler.due().is_none());
        assert_eq!(rig.scheduler.next_wake(), Some(job.next_ms));
        rig.clock.set(job.next_ms - 1);
        assert!(rig.scheduler.due().is_none());
        rig.clock.set(job.next_ms);
        let fired = rig.scheduler.due().expect("due");
        assert_eq!(fired.job.id, job.id);
        assert!(!fired.last_run);
        assert!(rig.scheduler.due().is_none(), "re-armed past now");
    }

    #[test]
    fn a_one_shot_is_removed_when_it_fires() {
        let rig = rig();
        let job = rig.scheduler.create(spec("* * * * *", false)).unwrap();
        rig.clock.set(job.next_ms);
        assert_eq!(rig.scheduler.due().unwrap().job.id, job.id);
        assert_eq!(rig.scheduler.count(), 0);
        assert_eq!(rig.scheduler.next_wake(), None);
        assert!(rig.scheduler.due().is_none());
        assert_eq!(rig.events().last().unwrap().0, ScheduleEvent::Fired);
    }

    #[test]
    fn a_recurring_job_that_fell_due_while_busy_fires_once_without_catching_up() {
        let rig = rig();
        let job = rig.scheduler.create(spec("* * * * *", true)).unwrap();
        rig.clock.set(job.next_ms + 3 * 60 * 60 * 1000 + 17_000);
        assert!(rig.scheduler.due().is_some());
        assert!(rig.scheduler.due().is_none(), "no missed runs are replayed");
        let rearmed = rig.scheduler.list()[0].next_ms;
        let now = rig.clock.now_ms();
        assert!(
            rearmed > now && rearmed - now <= 60_000,
            "{rearmed} vs {now}"
        );
        let fired_events = rig
            .events()
            .into_iter()
            .filter(|(event, _)| *event == ScheduleEvent::Fired)
            .count();
        assert_eq!(fired_events, 1);
    }

    #[test]
    fn the_earliest_due_job_goes_first() {
        let rig = rig();
        let every_five = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        let every_minute = rig.scheduler.create(spec("* * * * *", true)).unwrap();
        rig.clock.set(every_five.next_ms.max(every_minute.next_ms));
        let first = rig.scheduler.due().unwrap().job;
        assert_eq!(
            first.id,
            if every_five.next_ms <= every_minute.next_ms {
                every_five.id
            } else {
                every_minute.id
            }
        );
        assert!(rig.scheduler.due().is_some());
        assert!(rig.scheduler.due().is_none());
    }

    #[test]
    fn a_recurring_job_fires_one_last_time_after_seven_days_and_is_removed() {
        let rig = rig();
        let job = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        rig.clock.set(job.created_ms + WEEK_MS - 1);
        let near = rig.scheduler.due();
        assert!(near.is_some_and(|f| !f.last_run));
        assert_eq!(rig.scheduler.count(), 1);
        rig.clock.set(job.created_ms + WEEK_MS + 10 * 60 * 1000);
        let last = rig.scheduler.due().expect("final run");
        assert!(last.last_run);
        assert_eq!(rig.scheduler.count(), 0);
        let events = rig.events();
        assert_eq!(events[events.len() - 1].0, ScheduleEvent::Expired);
    }

    #[test]
    fn tasks_survive_a_reopen_and_the_file_is_owner_only() {
        let rig = rig();
        let job = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        let text = std::fs::read_to_string(rig.dir.path().join(SCHEDULE_FILE)).unwrap();
        assert!(text.contains(&job.id));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(rig.dir.path().join(SCHEDULE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
        rig.clock.advance(MINUTE);
        let reopened = rig.reopen();
        assert_eq!(reopened.take_warning(), None);
        let listed = reopened.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, job.id);
        assert_eq!(listed[0].prompt, job.prompt);
        assert!(listed[0].next_ms > rig.clock.now_ms());
    }

    #[test]
    fn restoring_drops_what_expired_what_passed_and_wakeups_and_says_so_once() {
        let rig = rig();
        let recurring = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        let soon = rig.scheduler.create(spec("* * * * *", false)).unwrap();
        let far = rig.scheduler.create(spec("0 0 1 1 *", false)).unwrap();
        let active = rig.scheduler.begin_loop("watch").unwrap();
        let wakeup = rig.scheduler.wake(120, false).unwrap();
        assert!(matches!(wakeup.kind, Kind::Wakeup { ref loop_id, .. } if *loop_id == active.id));

        // Eight minutes later the one-shot has passed and the wakeup is stale.
        rig.clock.advance(MINUTE * 8);
        let reopened = rig.reopen();
        reopened.settle_expired();
        let ids: Vec<String> = reopened.list().into_iter().map(|j| j.id).collect();
        assert!(ids.contains(&recurring.id) && ids.contains(&far.id));
        assert!(!ids.contains(&soon.id) && !ids.contains(&wakeup.id));
        let warning = reopened.take_warning().expect("a warning");
        assert_eq!(
            warning,
            "2 scheduled tasks expired while the session was closed"
        );
        assert_eq!(reopened.take_warning(), None);
        assert!(reopened.active_loop().is_none(), "a loop is not restored");
        let on_disk = std::fs::read_to_string(rig.dir.path().join(SCHEDULE_FILE)).unwrap();
        assert!(
            !on_disk.contains(&wakeup.id) && on_disk.contains(&recurring.id),
            "settling rewrites the file with the kept jobs"
        );
        assert!(
            rig.events()
                .contains(&(ScheduleEvent::Expired, soon.id.clone()))
        );

        // A week later the recurring job has run out too.
        rig.clock.advance(Duration::from_millis(WEEK_MS as u64));
        let later = rig.reopen();
        later.settle_expired();
        assert!(later.list().iter().all(|j| j.id != recurring.id));
        assert_eq!(later.list().len(), 1, "only the far one-shot is left");
        // The earlier drops were already settled, so only the recurring job counts now.
        assert_eq!(
            later.take_warning().unwrap(),
            "1 scheduled task expired while the session was closed"
        );

        let disabled = rig.reopen();
        assert_eq!(disabled.take_warning(), None, "loading alone says nothing");
    }

    #[test]
    fn a_restored_recurring_job_does_not_replay_missed_runs() {
        let rig = rig();
        rig.scheduler.create(spec("* * * * *", true)).unwrap();
        rig.clock.advance(MINUTE * 90);
        let reopened = rig.reopen();
        assert!(reopened.due().is_none());
    }

    #[test]
    fn an_unparseable_file_is_an_empty_scheduler_with_one_warning() {
        let rig = rig();
        std::fs::write(rig.dir.path().join(SCHEDULE_FILE), "{not json").unwrap();
        let reopened = rig.reopen();
        assert_eq!(reopened.count(), 0);
        let warning = reopened.take_warning().expect("a warning");
        assert!(warning.contains("schedule.json"), "{warning}");
        assert_eq!(reopened.take_warning(), None);
    }

    #[test]
    fn a_missing_file_is_empty_without_a_warning() {
        let rig = rig();
        let reopened = rig.reopen();
        assert_eq!(reopened.count(), 0);
        assert_eq!(reopened.take_warning(), None);
    }

    #[test]
    fn clear_deletes_every_job_records_each_and_empties_the_file() {
        let rig = rig();
        let a = rig.scheduler.create(spec("* * * * *", true)).unwrap();
        let b = rig.scheduler.create(spec("*/5 * * * *", false)).unwrap();
        rig.scheduler.begin_loop("watch").unwrap();
        assert_eq!(rig.scheduler.clear().unwrap(), 2);
        assert_eq!(rig.scheduler.count(), 0);
        assert!(rig.scheduler.active_loop().is_none());
        let events = rig.events();
        assert!(events.contains(&(ScheduleEvent::Deleted, a.id)));
        assert!(events.contains(&(ScheduleEvent::Deleted, b.id)));
        assert_eq!(rig.reopen().count(), 0);
        let text = std::fs::read_to_string(rig.dir.path().join(SCHEDULE_FILE)).unwrap();
        assert_eq!(text.trim(), "[]");
    }

    #[test]
    fn a_failed_save_leaves_the_schedule_as_it_was() {
        let rig = rig();
        let missing = rig.dir.path().join("gone");
        let scheduler = Scheduler::new(&missing, rig.ledger.clone(), rig.clock.clone());
        let error = scheduler.create(spec("* * * * *", true)).unwrap_err();
        assert!(matches!(error, ScheduleError::Persist { .. }));
        assert_eq!(scheduler.count(), 0);
    }

    #[test]
    fn a_failed_save_is_also_told_to_the_human() {
        use std::sync::Mutex as StdMutex;
        #[derive(Default)]
        struct Warnings(StdMutex<Vec<String>>);
        impl Approver for Warnings {
            fn approve<'a>(
                &'a self,
                _: crate::permissions::ApprovalRequest,
            ) -> futures::future::BoxFuture<'a, crate::permissions::Answer> {
                Box::pin(async { crate::permissions::Answer::Deny { feedback: None } })
            }
            fn warn(&self, message: &str) {
                self.0.lock().unwrap().push(message.to_string());
            }
        }
        let rig = rig();
        let missing = rig.dir.path().join("gone");
        let approver = Arc::new(Warnings::default());
        let store = ReportedSchedule {
            scheduler: Arc::new(Scheduler::new(
                &missing,
                rig.ledger.clone(),
                rig.clock.clone(),
            )),
            approver: approver.clone(),
        };
        let job = NewJob {
            cron: "* * * * *".into(),
            prompt: "p".into(),
            recurring: true,
        };
        assert!(ScheduleStore::create(&store, job).is_err());
        assert_eq!(approver.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_store_marks_what_the_model_schedules_as_the_models() {
        let rig = rig();
        let info = ScheduleStore::create(
            &rig.scheduler,
            NewJob {
                cron: "*/5 * * * *".into(),
                prompt: "p".into(),
                recurring: true,
            },
        )
        .unwrap();
        assert_eq!(info.origin, "model");
        assert_eq!(info.cadence, "every 5 minutes");
        assert_eq!(rig.scheduler.list()[0].origin, Origin::Model);
    }

    // ---- the self-paced loop ----

    #[test]
    fn deleting_the_wakeup_ends_the_loop() {
        let rig = rig();
        rig.scheduler.begin_loop("watch").unwrap();
        let wake = rig.scheduler.wake(120, false).unwrap();
        assert!(rig.scheduler.delete(&wake.id).unwrap());
        assert!(rig.scheduler.active_loop().is_none());
        rig.scheduler.begin_loop("again").unwrap();
    }

    #[test]
    fn a_huge_step_is_rejected_not_overflowed() {
        assert!(matches!(
            Cron::parse("1-59/4294967295 * * * *"),
            Err(ScheduleError::BadCron(_))
        ));
    }

    #[test]
    fn a_wakeup_needs_a_loop_and_rearms_the_loops_own_prompt() {
        let rig = rig();
        assert!(matches!(
            rig.scheduler.wake(120, false),
            Err(ScheduleError::NoLoop)
        ));
        let active = rig.scheduler.begin_loop("keep watching CI").unwrap();
        assert!(matches!(
            rig.scheduler.begin_loop("again"),
            Err(ScheduleError::LoopActive)
        ));
        assert!(!rig.scheduler.wakeup_pending());
        let first = rig.scheduler.wake(120, false).unwrap();
        assert_eq!(first.prompt, "keep watching CI");
        assert!(!first.recurring);
        assert_eq!(first.origin, Origin::User);
        assert_eq!(first.next_ms, START_MS + 120_000);
        assert_eq!(first.cron.split(' ').count(), 5);
        assert!(rig.scheduler.wakeup_pending());
        // A later wakeup replaces the pending one.
        let second = rig.scheduler.wake(300, false).unwrap();
        assert_eq!(rig.scheduler.count(), 1);
        assert_eq!(rig.scheduler.list()[0].id, second.id);
        assert!(matches!(
            rig.scheduler.wake(59, false),
            Err(ScheduleError::BadDelay)
        ));
        assert!(matches!(
            rig.scheduler.wake(3601, false),
            Err(ScheduleError::BadDelay)
        ));
        assert_eq!(rig.scheduler.active_loop().unwrap(), active);
    }

    #[test]
    fn an_iteration_that_schedules_nothing_arms_one_fallback_then_ends_the_loop() {
        let rig = rig();
        rig.scheduler.begin_loop("watch").unwrap();
        assert_eq!(
            rig.scheduler.iteration_ended().unwrap(),
            LoopStep::FallbackArmed
        );
        let fallback = rig.scheduler.list().remove(0);
        assert!(matches!(fallback.kind, Kind::Wakeup { fallback: true, .. }));
        assert_eq!(
            fallback.next_ms,
            START_MS + (FALLBACK_SECONDS as i64) * 1000
        );
        // The fallback fires and the iteration schedules nothing again.
        rig.clock.set(fallback.next_ms);
        assert!(rig.scheduler.due().is_some());
        assert_eq!(rig.scheduler.iteration_ended().unwrap(), LoopStep::Ended);
        assert!(rig.scheduler.active_loop().is_none());
        assert_eq!(rig.scheduler.iteration_ended().unwrap(), LoopStep::NoLoop);
    }

    #[test]
    fn scheduling_a_wakeup_resets_the_fallback_allowance() {
        let rig = rig();
        rig.scheduler.begin_loop("watch").unwrap();
        assert_eq!(
            rig.scheduler.iteration_ended().unwrap(),
            LoopStep::FallbackArmed
        );
        let job = rig.scheduler.list().remove(0);
        rig.clock.set(job.next_ms);
        rig.scheduler.due().unwrap();
        rig.scheduler.wake(600, false).unwrap();
        assert_eq!(
            rig.scheduler.iteration_ended().unwrap(),
            LoopStep::Continues
        );
        let job = rig.scheduler.list().remove(0);
        rig.clock.set(job.next_ms);
        rig.scheduler.due().unwrap();
        assert_eq!(
            rig.scheduler.iteration_ended().unwrap(),
            LoopStep::FallbackArmed
        );
    }

    #[test]
    fn stopping_a_loop_cancels_its_wakeup() {
        let rig = rig();
        assert!(!rig.scheduler.end_loop().unwrap());
        rig.scheduler.begin_loop("watch").unwrap();
        rig.scheduler.wake(120, false).unwrap();
        assert!(rig.scheduler.end_loop().unwrap());
        assert_eq!(rig.scheduler.count(), 0);
        assert!(!rig.scheduler.wakeup_pending());
        assert_eq!(rig.scheduler.iteration_ended().unwrap(), LoopStep::NoLoop);
        // The store's stop says so when there is no loop.
        assert!(matches!(
            ScheduleStore::wake(&rig.scheduler, WakeRequest::Stop),
            Err(ScheduleError::NoLoop)
        ));
    }

    #[test]
    fn until_next_wake_counts_down_to_zero() {
        let rig = rig();
        assert_eq!(rig.scheduler.until_next_wake(), None);
        let job = rig.scheduler.create(spec("*/5 * * * *", true)).unwrap();
        let wait = rig.scheduler.until_next_wake().unwrap();
        assert_eq!(wait.as_millis() as i64, job.next_ms - START_MS);
        rig.clock.set(job.next_ms + 5000);
        assert_eq!(rig.scheduler.until_next_wake(), Some(Duration::ZERO));
    }
}
