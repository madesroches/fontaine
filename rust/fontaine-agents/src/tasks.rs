//! The team's shared task board: tasks the lead creates and teammates claim. It is an in-memory
//! list behind one mutex, saved through [`TaskStore`] on every change. The board is state, not
//! authority: every operation on it is an ordinary gated tool call.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::lock::lock;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

pub const MAX_TASKS: usize = 100;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_DESCRIPTION_CHARS: usize = 5_000;
pub const MAX_BLOCKED_BY: usize = 20;

/// How many times in a row a teammate is resumed by the idle hand-off or a `teammate_idle` hook
/// without the board changing in between. Both loops sustain themselves, so they need a bound.
pub const MAX_IDLE_RESUMES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    /// The agent id of whoever claimed it.
    pub owner: Option<String>,
    pub blocked_by: Vec<String>,
    /// The agent id of whoever created it.
    pub created_by: String,
}

/// What is saved: the tasks and the counter that keeps ids from being reused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub next: u64,
    pub tasks: Vec<Task>,
}

/// Where the board is kept. `fontaine-core` implements it over the session directory. A save
/// that fails is the implementation's to report; the board's in-memory state stays the truth.
pub trait TaskStore: Send + Sync {
    fn save(&self, snapshot: &Snapshot);
}

/// Why a board operation was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardError {
    /// The arguments are wrong: a cap, an empty title, an unknown dependency or task.
    Invalid(String),
    /// The arguments are fine but the task is not in a state, or the caller not in a role, that
    /// allows it.
    Refused(String),
}

impl fmt::Display for BoardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoardError::Invalid(why) | BoardError::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for BoardError {}

struct State {
    next: u64,
    tasks: Vec<Task>,
}

impl State {
    fn index(&self, id: &str) -> Result<usize, BoardError> {
        self.tasks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| BoardError::Invalid(format!("there is no task `{id}`")))
    }

    fn unblocked(&self, task: &Task) -> bool {
        task.blocked_by.iter().all(|dep| {
            self.tasks
                .iter()
                .any(|t| t.id == *dep && t.status == TaskStatus::Completed)
        })
    }

    fn claimable(&self, task: &Task) -> bool {
        task.status == TaskStatus::Pending && task.owner.is_none() && self.unblocked(task)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            next: self.next,
            tasks: self.tasks.clone(),
        }
    }
}

pub struct TaskBoard {
    state: Mutex<State>,
    store: Option<Arc<dyn TaskStore>>,
    /// Counts every create, claim, complete and release, so the idle loops can tell whether the
    /// board moved between two resumes.
    mutations: AtomicU64,
}

impl TaskBoard {
    pub fn new(store: Option<Arc<dyn TaskStore>>) -> TaskBoard {
        TaskBoard::restore(store, Snapshot::default())
    }

    /// A board holding what an earlier process of the session saved. Agent ids do not survive a
    /// resume, so a task somebody was working on goes back to `Pending` with no owner.
    pub fn restore(store: Option<Arc<dyn TaskStore>>, snapshot: Snapshot) -> TaskBoard {
        let mut tasks = snapshot.tasks;
        for task in &mut tasks {
            if task.status == TaskStatus::InProgress {
                task.status = TaskStatus::Pending;
            }
            if task.status != TaskStatus::Completed {
                task.owner = None;
            }
        }
        // The counter never falls behind a stored id, whatever the file said.
        let floor = tasks
            .iter()
            .filter_map(|t| t.id.strip_prefix('t')?.parse::<u64>().ok())
            .max()
            .map_or(1, |max| max + 1);
        TaskBoard {
            state: Mutex::new(State {
                next: snapshot.next.max(floor).max(1),
                tasks,
            }),
            store,
            mutations: AtomicU64::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state, "task board")
    }

    fn changed(&self, state: &State) {
        self.mutations.fetch_add(1, Ordering::SeqCst);
        if let Some(store) = &self.store {
            store.save(&state.snapshot());
        }
    }

    pub fn mutations(&self) -> u64 {
        self.mutations.load(Ordering::SeqCst)
    }

    pub fn list(&self) -> Vec<Task> {
        self.state().tasks.clone()
    }

    pub fn get(&self, id: &str) -> Result<Task, BoardError> {
        let state = self.state();
        state.index(id).map(|at| state.tasks[at].clone())
    }

    /// The first task a teammate could claim now: pending, unowned and with every dependency
    /// completed.
    pub fn claimable(&self) -> Option<Task> {
        let state = self.state();
        state.tasks.iter().find(|t| state.claimable(t)).cloned()
    }

    pub fn create(
        &self,
        creator: &str,
        title: &str,
        description: &str,
        blocked_by: &[String],
    ) -> Result<Task, BoardError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(BoardError::Invalid("a task needs a title".to_string()));
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(BoardError::Invalid(format!(
                "a title is at most {MAX_TITLE_CHARS} characters"
            )));
        }
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(BoardError::Invalid(format!(
                "a description is at most {MAX_DESCRIPTION_CHARS} characters"
            )));
        }
        if blocked_by.len() > MAX_BLOCKED_BY {
            return Err(BoardError::Invalid(format!(
                "a task depends on at most {MAX_BLOCKED_BY} others"
            )));
        }
        let mut state = self.state();
        if state.tasks.len() >= MAX_TASKS {
            return Err(BoardError::Refused(format!(
                "the board holds {MAX_TASKS} tasks, the limit"
            )));
        }
        for dep in blocked_by {
            state.index(dep).map_err(|_| {
                BoardError::Invalid(format!("`blocked_by` names `{dep}`, which is not a task"))
            })?;
        }
        let mut deps: Vec<String> = Vec::new();
        for dep in blocked_by {
            if !deps.contains(dep) {
                deps.push(dep.clone());
            }
        }
        let task = Task {
            id: format!("t{}", state.next),
            title: title.to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            owner: None,
            blocked_by: deps,
            created_by: creator.to_string(),
        };
        state.next += 1;
        state.tasks.push(task.clone());
        self.changed(&state);
        Ok(task)
    }

    /// Takes `id` for `agent`. The check and the change share one lock, so of two simultaneous
    /// claims exactly one wins.
    pub fn claim(&self, agent: &str, id: &str) -> Result<Task, BoardError> {
        let mut state = self.state();
        let at = state.index(id)?;
        let task = &state.tasks[at];
        match (task.status, &task.owner) {
            (TaskStatus::Completed, _) => {
                return Err(BoardError::Refused(format!(
                    "task {id} is already completed"
                )));
            }
            (TaskStatus::InProgress, Some(owner)) => {
                return Err(BoardError::Refused(format!(
                    "task {id} is already claimed by {owner}"
                )));
            }
            _ => {}
        }
        if !state.unblocked(task) {
            let open: Vec<&str> = task
                .blocked_by
                .iter()
                .filter(|dep| {
                    !state
                        .tasks
                        .iter()
                        .any(|t| t.id == **dep && t.status == TaskStatus::Completed)
                })
                .map(String::as_str)
                .collect();
            return Err(BoardError::Refused(format!(
                "task {id} is blocked by {}, which are not completed",
                open.join(", ")
            )));
        }
        let task = &mut state.tasks[at];
        task.status = TaskStatus::InProgress;
        task.owner = Some(agent.to_string());
        let task = task.clone();
        self.changed(&state);
        Ok(task)
    }

    /// What `agent` may do to an in-progress task: its owner and the lead.
    fn owned_by(task: &Task, agent: &str, verb: &str) -> Result<(), BoardError> {
        if task.status != TaskStatus::InProgress {
            return Err(BoardError::Refused(format!(
                "task {} is {}, so it cannot be {verb}",
                task.id,
                task.status.as_str()
            )));
        }
        if agent != ROOT_AGENT && task.owner.as_deref() != Some(agent) {
            return Err(BoardError::Refused(format!(
                "only the owner or the lead can change task {} while it is in progress",
                task.id
            )));
        }
        Ok(())
    }

    /// Whether `agent` may complete `id` now, without completing it. The quality hook runs between
    /// this check and [`complete`](Self::complete), outside the board's lock.
    pub fn check_complete(&self, agent: &str, id: &str) -> Result<Task, BoardError> {
        let state = self.state();
        let task = &state.tasks[state.index(id)?];
        Self::owned_by(task, agent, "completed")?;
        Ok(task.clone())
    }

    pub fn complete(&self, agent: &str, id: &str) -> Result<Task, BoardError> {
        let mut state = self.state();
        let at = state.index(id)?;
        Self::owned_by(&state.tasks[at], agent, "completed")?;
        state.tasks[at].status = TaskStatus::Completed;
        let task = state.tasks[at].clone();
        self.changed(&state);
        Ok(task)
    }

    /// Gives an in-progress task back to `Pending`.
    pub fn release(&self, agent: &str, id: &str) -> Result<Task, BoardError> {
        let mut state = self.state();
        let at = state.index(id)?;
        Self::owned_by(&state.tasks[at], agent, "released")?;
        let task = &mut state.tasks[at];
        task.status = TaskStatus::Pending;
        task.owner = None;
        let task = task.clone();
        self.changed(&state);
        Ok(task)
    }

    /// Returns every in-progress task `agent` owns to `Pending`, for an agent that ended without
    /// finishing them. The ids of the tasks released.
    pub fn release_owned_by(&self, agent: &str) -> Vec<String> {
        let mut state = self.state();
        let mut released = Vec::new();
        for task in &mut state.tasks {
            if task.status == TaskStatus::InProgress && task.owner.as_deref() == Some(agent) {
                task.status = TaskStatus::Pending;
                task.owner = None;
                released.push(task.id.clone());
            }
        }
        if !released.is_empty() {
            self.changed(&state);
        }
        released
    }
}

/// The two quality gates, implemented over the user's hooks by `fontaine-core`. A gate can only
/// add work: it blocks or it does not. One that errors or times out reports itself through
/// [`warn`](Self::warn) and answers `None`, because a gate that failed closed forever would wedge
/// the team.
pub trait QualityGate: Send + Sync {
    /// Runs the `task_completed` hooks before `agent` completes `task`; `Some(reason)` blocks.
    fn task_completed<'a>(
        &'a self,
        agent: &'a str,
        task: &'a Task,
    ) -> BoxFuture<'a, Option<String>>;

    /// Runs the `teammate_idle` hooks when `name` ended a turn; `Some(reason)` resumes it with
    /// the reason as feedback.
    fn teammate_idle<'a>(&'a self, agent: &'a str, name: &'a str) -> BoxFuture<'a, Option<String>>;

    /// Tells the human something went wrong that is not the model's to handle.
    fn warn(&self, message: &str);
}

/// What a session with teams on holds: the board, the gates and the resume streaks.
pub struct Teams {
    pub board: Arc<TaskBoard>,
    pub gate: Arc<dyn QualityGate>,
    /// Per teammate: resumes in a row and the board's mutation count at the last of them.
    streaks: Mutex<HashMap<String, (u32, u64)>>,
}

impl Teams {
    pub fn new(board: Arc<TaskBoard>, gate: Arc<dyn QualityGate>) -> Teams {
        Teams {
            board,
            gate,
            streaks: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `agent` may be resumed by the idle hand-off or a hook block now, counting it. The
    /// streak starts over whenever the board has changed since the last such resume.
    pub fn allow_idle_resume(&self, agent: &str) -> bool {
        let mutations = self.board.mutations();
        let mut streaks = lock(&self.streaks, "idle resume streaks");
        let entry = streaks.entry(agent.to_string()).or_insert((0, mutations));
        if entry.1 != mutations {
            *entry = (0, mutations);
        }
        if entry.0 >= MAX_IDLE_RESUMES {
            return false;
        }
        entry.0 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> TaskBoard {
        TaskBoard::new(None)
    }

    fn add(board: &TaskBoard, title: &str, deps: &[&str]) -> String {
        let deps: Vec<String> = deps.iter().map(|d| d.to_string()).collect();
        board.create(ROOT_AGENT, title, "", &deps).unwrap().id
    }

    #[test]
    fn ids_are_minted_in_order_and_never_reused() {
        let board = board();
        assert_eq!(add(&board, "a", &[]), "t1");
        assert_eq!(add(&board, "b", &[]), "t2");
        let restored = TaskBoard::restore(
            None,
            Snapshot {
                next: 1,
                tasks: board.list(),
            },
        );
        assert_eq!(add(&restored, "c", &[]), "t3", "a stale counter is lifted");
    }

    #[test]
    fn creation_enforces_the_caps() {
        let board = board();
        assert!(matches!(
            board.create(ROOT_AGENT, " ", "", &[]),
            Err(BoardError::Invalid(_))
        ));
        let long = "x".repeat(MAX_TITLE_CHARS + 1);
        assert!(matches!(
            board.create(ROOT_AGENT, &long, "", &[]),
            Err(BoardError::Invalid(_))
        ));
        let description = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
        assert!(matches!(
            board.create(ROOT_AGENT, "t", &description, &[]),
            Err(BoardError::Invalid(_))
        ));
        let id = add(&board, "base", &[]);
        let many = vec![id; MAX_BLOCKED_BY + 1];
        assert!(matches!(
            board.create(ROOT_AGENT, "t", "", &many),
            Err(BoardError::Invalid(_))
        ));
        let board = TaskBoard::new(None);
        for n in 0..MAX_TASKS {
            add(&board, &format!("task {n}"), &[]);
        }
        assert!(matches!(
            board.create(ROOT_AGENT, "one more", "", &[]),
            Err(BoardError::Refused(_))
        ));
    }

    #[test]
    fn blocked_by_must_name_an_existing_task() {
        let board = board();
        let error = board
            .create(ROOT_AGENT, "t", "", &["t9".to_string()])
            .unwrap_err();
        assert!(error.to_string().contains("t9"), "{error}");
        assert!(board.list().is_empty());
    }

    #[test]
    fn a_task_is_claimed_only_when_pending_unowned_and_unblocked() {
        let board = board();
        let first = add(&board, "first", &[]);
        let second = add(&board, "second", &[&first]);
        assert!(board.claim("a", &second).is_err(), "blocked");
        board.claim("a", &first).unwrap();
        assert!(board.claim("b", &first).is_err(), "owned");
        board.complete("a", &first).unwrap();
        assert!(board.claim("b", &first).is_err(), "completed");
        let claimed = board.claim("b", &second).unwrap();
        assert_eq!(claimed.owner.as_deref(), Some("b"));
        assert_eq!(claimed.status, TaskStatus::InProgress);
        assert!(board.claim("a", "t99").is_err());
    }

    #[test]
    fn a_simultaneous_claim_has_exactly_one_winner() {
        for _ in 0..20 {
            let board = Arc::new(board());
            let id = add(&board, "contested", &[]);
            let handles: Vec<_> = (0..8)
                .map(|n| {
                    let board = board.clone();
                    let id = id.clone();
                    std::thread::spawn(move || board.claim(&format!("agent-{n}"), &id).is_ok())
                })
                .collect();
            let winners = handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|won| *won)
                .count();
            assert_eq!(winners, 1);
        }
    }

    #[test]
    fn completing_unblocks_dependents() {
        let board = board();
        let first = add(&board, "first", &[]);
        let second = add(&board, "second", &[&first]);
        assert_eq!(board.claimable().unwrap().id, first);
        board.claim("a", &first).unwrap();
        assert!(
            board.claimable().is_none(),
            "the dependent is still blocked"
        );
        board.complete("a", &first).unwrap();
        assert_eq!(board.claimable().unwrap().id, second);
    }

    #[test]
    fn only_the_owner_or_the_lead_updates_an_in_progress_task() {
        let board = board();
        let id = add(&board, "work", &[]);
        board.claim("a", &id).unwrap();
        assert!(board.complete("b", &id).is_err());
        assert!(board.release("b", &id).is_err());
        board.release("a", &id).unwrap();
        board.claim("a", &id).unwrap();
        board.complete(ROOT_AGENT, &id).unwrap();
        assert!(board.complete("a", &id).is_err(), "already completed");
    }

    #[test]
    fn release_returns_a_task_to_pending_without_an_owner() {
        let board = board();
        let id = add(&board, "work", &[]);
        board.claim("a", &id).unwrap();
        let task = board.release("a", &id).unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.owner, None);
        assert!(board.release("a", &id).is_err(), "not in progress");
    }

    #[test]
    fn releasing_by_owner_leaves_other_agents_tasks_and_completed_ones() {
        let board = board();
        let mine = add(&board, "mine", &[]);
        let done = add(&board, "done", &[]);
        let theirs = add(&board, "theirs", &[]);
        board.claim("a", &mine).unwrap();
        board.claim("a", &done).unwrap();
        board.complete("a", &done).unwrap();
        board.claim("b", &theirs).unwrap();
        assert_eq!(board.release_owned_by("a"), vec![mine.clone()]);
        assert_eq!(board.get(&mine).unwrap().status, TaskStatus::Pending);
        assert_eq!(board.get(&done).unwrap().status, TaskStatus::Completed);
        assert_eq!(board.get(&theirs).unwrap().owner.as_deref(), Some("b"));
        assert!(board.release_owned_by("a").is_empty());
    }

    #[test]
    fn a_restored_board_returns_in_progress_tasks_to_pending() {
        let board = board();
        let mine = add(&board, "mine", &[]);
        let done = add(&board, "done", &[]);
        board.claim("a", &mine).unwrap();
        board.claim("a", &done).unwrap();
        board.complete("a", &done).unwrap();
        let restored = TaskBoard::restore(
            None,
            Snapshot {
                next: 3,
                tasks: board.list(),
            },
        );
        let task = restored.get(&mine).unwrap();
        assert_eq!((task.status, task.owner), (TaskStatus::Pending, None));
        assert_eq!(restored.get(&done).unwrap().status, TaskStatus::Completed);
    }

    #[test]
    fn every_change_is_saved_and_counted() {
        #[derive(Default)]
        struct Saved(Mutex<Vec<Snapshot>>);
        impl TaskStore for Saved {
            fn save(&self, snapshot: &Snapshot) {
                self.0.lock().unwrap().push(snapshot.clone());
            }
        }
        let store = Arc::new(Saved::default());
        let board = TaskBoard::new(Some(store.clone()));
        let id = add(&board, "work", &[]);
        board.claim("a", &id).unwrap();
        assert!(board.claim("b", &id).is_err());
        assert_eq!(board.mutations(), 2, "a refusal changes nothing");
        let saved = store.0.lock().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].tasks[0].owner.as_deref(), Some("a"));
        assert_eq!(saved[1].next, 2);
    }

    struct NoGate;
    impl QualityGate for NoGate {
        fn task_completed<'a>(&'a self, _: &'a str, _: &'a Task) -> BoxFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn teammate_idle<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn warn(&self, _: &str) {}
    }

    #[test]
    fn the_idle_resume_streak_is_capped_and_a_board_change_resets_it() {
        let board = Arc::new(board());
        let teams = Teams::new(board.clone(), Arc::new(NoGate));
        for _ in 0..MAX_IDLE_RESUMES {
            assert!(teams.allow_idle_resume("a"));
        }
        assert!(!teams.allow_idle_resume("a"));
        assert!(teams.allow_idle_resume("b"), "streaks are per teammate");
        add(&board, "news", &[]);
        assert!(teams.allow_idle_resume("a"), "the board moved");
    }
}
