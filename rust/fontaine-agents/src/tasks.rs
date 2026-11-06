//! The task boards: tasks an agent creates for its children to claim. Each board belongs to the
//! agent that owns it and is an in-memory list behind one mutex; `main`'s is saved through
//! [`TaskStore`] on every change. A board is state, not authority: every operation on it is an
//! ordinary gated tool call.

use std::collections::{HashMap, HashSet};
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

/// How many times in a row an agent is resumed by a `teammate_idle` hook without the board that
/// holds its task changing in between. The loop sustains itself, so it needs a bound.
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
}

pub struct TaskBoard {
    /// The agent whose board this is, who may change any task on it.
    owner: String,
    state: Mutex<State>,
    /// Task ids come from one counter for the whole session, so a task is named the same on
    /// every board.
    ids: Arc<AtomicU64>,
    store: Option<Arc<dyn TaskStore>>,
    /// The saved board, saved again when this one mints an id, so the stored counter never falls
    /// behind an id a subagent's board used.
    saved_root: Option<Arc<TaskBoard>>,
    /// Counts every create, claim, complete and release, so the idle loops and the owner
    /// reminder can tell whether the board moved.
    mutations: AtomicU64,
}

impl TaskBoard {
    /// The board of `main`.
    pub fn new(store: Option<Arc<dyn TaskStore>>) -> TaskBoard {
        TaskBoard::restore(store, Snapshot::default(), &HashSet::new())
    }

    /// A board holding what an earlier process of the session saved. A task somebody was working
    /// on stays in progress when its owner is in `revivable` (the agents the resumed session
    /// brought back); otherwise nobody is working on it and it goes back to `Pending`.
    pub fn restore(
        store: Option<Arc<dyn TaskStore>>,
        snapshot: Snapshot,
        revivable: &HashSet<String>,
    ) -> TaskBoard {
        let mut tasks = snapshot.tasks;
        for task in &mut tasks {
            let kept = task.status == TaskStatus::InProgress
                && task.owner.as_ref().is_some_and(|o| revivable.contains(o));
            if task.status == TaskStatus::InProgress && !kept {
                task.status = TaskStatus::Pending;
            }
            if task.status != TaskStatus::Completed && !kept {
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
            owner: ROOT_AGENT.to_string(),
            state: Mutex::new(State { tasks }),
            ids: Arc::new(AtomicU64::new(snapshot.next.max(floor).max(1))),
            store,
            saved_root: None,
            mutations: AtomicU64::new(0),
        }
    }

    fn child(owner: &str, root: &Arc<TaskBoard>) -> TaskBoard {
        TaskBoard {
            owner: owner.to_string(),
            state: Mutex::new(State { tasks: Vec::new() }),
            ids: root.ids.clone(),
            store: None,
            saved_root: Some(root.clone()),
            mutations: AtomicU64::new(0),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state, "task board")
    }

    fn snapshot(&self, state: &State) -> Snapshot {
        Snapshot {
            next: self.ids.load(Ordering::SeqCst),
            tasks: state.tasks.clone(),
        }
    }

    fn changed(&self, state: &State) {
        self.mutations.fetch_add(1, Ordering::SeqCst);
        if let Some(store) = &self.store {
            store.save(&self.snapshot(state));
        }
    }

    /// Saves `main`'s board again, for the counter a subagent's board just moved.
    fn save_root(&self) {
        if let Some(root) = &self.saved_root
            && let Some(store) = &root.store
        {
            let state = root.state();
            store.save(&root.snapshot(&state));
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

    /// The first task an agent could claim now: pending, unowned and with every dependency
    /// completed.
    pub fn claimable(&self) -> Option<Task> {
        let state = self.state();
        state.tasks.iter().find(|t| state.claimable(t)).cloned()
    }

    /// How many tasks an agent could claim now.
    pub fn claimable_count(&self) -> usize {
        let state = self.state();
        state.tasks.iter().filter(|t| state.claimable(t)).count()
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
            id: format!("t{}", self.ids.fetch_add(1, Ordering::SeqCst)),
            title: title.to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            owner: None,
            blocked_by: deps,
            created_by: creator.to_string(),
        };
        state.tasks.push(task.clone());
        self.changed(&state);
        drop(state);
        self.save_root();
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

    /// What `agent` may do to an in-progress task: its claimer and the board's owner.
    fn owned_by(&self, task: &Task, agent: &str, verb: &str) -> Result<(), BoardError> {
        if task.status != TaskStatus::InProgress {
            return Err(BoardError::Refused(format!(
                "task {} is {}, so it cannot be {verb}",
                task.id,
                task.status.as_str()
            )));
        }
        if agent != self.owner && task.owner.as_deref() != Some(agent) {
            return Err(BoardError::Refused(format!(
                "only the claimer or the board's owner can change task {} while it is in progress",
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
        self.owned_by(task, agent, "completed")?;
        Ok(task.clone())
    }

    pub fn complete(&self, agent: &str, id: &str) -> Result<Task, BoardError> {
        let mut state = self.state();
        let at = state.index(id)?;
        self.owned_by(&state.tasks[at], agent, "completed")?;
        state.tasks[at].status = TaskStatus::Completed;
        let task = state.tasks[at].clone();
        self.changed(&state);
        Ok(task)
    }

    /// Gives an in-progress task back to `Pending`.
    pub fn release(&self, agent: &str, id: &str) -> Result<Task, BoardError> {
        let mut state = self.state();
        let at = state.index(id)?;
        self.owned_by(&state.tasks[at], agent, "released")?;
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

    /// Whether `agent` has claimed a task here that is still in progress.
    pub fn has_in_progress_of(&self, agent: &str) -> bool {
        self.state()
            .tasks
            .iter()
            .any(|t| t.status == TaskStatus::InProgress && t.owner.as_deref() == Some(agent))
    }
}

/// The reminder that tasks wait for agents, and the key it was computed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub text: String,
    key: (u64, u32),
}

/// Every board of a session: `main`'s, which is saved, and one per other agent that created a
/// task and holds the `agent` tool. A task id is unique across all of them.
pub struct Boards {
    root: Arc<TaskBoard>,
    others: Mutex<Vec<(String, Arc<TaskBoard>)>>,
    /// Per owner: the board's mutation count and the free slots at the last reminder.
    reminded: Mutex<HashMap<String, (u64, u32)>>,
}

impl Boards {
    pub fn new(store: Option<Arc<dyn TaskStore>>) -> Boards {
        Boards::with_root(Arc::new(TaskBoard::new(store)))
    }

    /// Boards around `root`, `main`'s own, whose counter they share.
    pub fn with_root(root: Arc<TaskBoard>) -> Boards {
        Boards {
            root,
            others: Mutex::new(Vec::new()),
            reminded: Mutex::new(HashMap::new()),
        }
    }

    pub fn restore(
        store: Option<Arc<dyn TaskStore>>,
        snapshot: Snapshot,
        revivable: &HashSet<String>,
    ) -> Boards {
        Boards::with_root(Arc::new(TaskBoard::restore(store, snapshot, revivable)))
    }

    /// `main`'s board.
    pub fn root(&self) -> &Arc<TaskBoard> {
        &self.root
    }

    fn others(&self) -> std::sync::MutexGuard<'_, Vec<(String, Arc<TaskBoard>)>> {
        lock(&self.others, "task boards")
    }

    /// The board `agent` owns, when it has one.
    pub fn get(&self, agent: &str) -> Option<Arc<TaskBoard>> {
        if agent == ROOT_AGENT {
            return Some(self.root.clone());
        }
        self.others()
            .iter()
            .find(|(owner, _)| owner == agent)
            .map(|(_, board)| board.clone())
    }

    /// The board `agent` owns, created empty the first time.
    pub fn own(&self, agent: &str) -> Arc<TaskBoard> {
        if agent == ROOT_AGENT {
            return self.root.clone();
        }
        let mut others = self.others();
        if let Some((_, board)) = others.iter().find(|(owner, _)| owner == agent) {
            return board.clone();
        }
        let board = Arc::new(TaskBoard::child(agent, &self.root));
        others.push((agent.to_string(), board.clone()));
        board
    }

    /// The board holding `task`, of those `agent` can reach: its own, then its parent's.
    pub fn find(&self, agent: &str, parent: Option<&str>, task: &str) -> Option<Arc<TaskBoard>> {
        std::iter::once(agent)
            .chain(parent)
            .filter_map(|owner| self.get(owner))
            .find(|board| board.get(task).is_ok())
    }

    /// Ends `agent`'s board with its owner. `main`'s lives as long as the session.
    pub fn drop_owner(&self, agent: &str) {
        if agent == ROOT_AGENT {
            return;
        }
        self.others().retain(|(owner, _)| owner != agent);
        lock(&self.reminded, "owner reminders").remove(agent);
    }

    /// Returns every in-progress task `agent` owns, on its own board and on its parent's, to
    /// `Pending`. The ids released.
    pub fn release_owned_by(&self, agent: &str, parent: Option<&str>) -> Vec<String> {
        std::iter::once(agent)
            .chain(parent)
            .filter_map(|owner| self.get(owner))
            .flat_map(|board| board.release_owned_by(agent))
            .collect()
    }

    /// Every board, `main`'s first and the rest in the order they were created, with its tasks.
    pub fn all(&self) -> Vec<(String, Vec<Task>)> {
        let mut all = vec![(ROOT_AGENT.to_string(), self.root.list())];
        all.extend(
            self.others()
                .iter()
                .map(|(owner, board)| (owner.clone(), board.list())),
        );
        all
    }

    /// Whether `agent` holds a task in progress on any board.
    pub fn owns_in_progress(&self, agent: &str) -> bool {
        self.all()
            .iter()
            .any(|(_, tasks)| in_progress_of(tasks, agent))
    }

    /// The agents that hold a task in progress on any board.
    pub fn in_progress_owners(&self) -> HashSet<String> {
        self.all()
            .into_iter()
            .flat_map(|(_, tasks)| tasks)
            .filter(|t| t.status == TaskStatus::InProgress)
            .filter_map(|t| t.owner)
            .collect()
    }

    /// Whether `agent` holds a task in progress on the board `board_owner` owns.
    pub fn owns_in_progress_on(&self, board_owner: &str, agent: &str) -> bool {
        self.get(board_owner)
            .is_some_and(|board| board.has_in_progress_of(agent))
    }

    /// The mutation count of the board `owner` owns, zero when it has none.
    pub fn mutations_of(&self, owner: &str) -> u64 {
        self.get(owner).map_or(0, |board| board.mutations())
    }

    /// The owners whose boards hold a task an agent could claim now, `main` first.
    pub fn claimable_owners(&self) -> Vec<String> {
        let mut owners = Vec::new();
        if self.root.claimable_count() > 0 {
            owners.push(ROOT_AGENT.to_string());
        }
        owners.extend(
            self.others()
                .iter()
                .filter(|(_, board)| board.claimable_count() > 0)
                .map(|(owner, _)| owner.clone()),
        );
        owners
    }

    /// The reminder `owner` is due, if any: its board holds claimable tasks, `free_slots` agents
    /// could start for them, and nothing has changed since it was last reminded. `key_slots` is
    /// the free-slot count that change is measured by, which differs from `free_slots` for an
    /// owner whose own running slot counts as free. Atomically checks and claims the reminder so
    /// two concurrent callers cannot both obtain it; the caller rolls back with
    /// [`forget_reminder`](Self::forget_reminder) if delivery fails.
    pub fn pending_reminder(
        &self,
        owner: &str,
        free_slots: u32,
        key_slots: u32,
    ) -> Option<Reminder> {
        let board = self.get(owner)?;
        let claimable = board.claimable_count();
        if claimable == 0 || free_slots == 0 {
            return None;
        }
        let key = (board.mutations(), key_slots);
        let mut reminded = lock(&self.reminded, "owner reminders");
        if reminded.get(owner) == Some(&key) {
            return None;
        }
        let tasks = if claimable == 1 {
            "1 task on your board is".to_string()
        } else {
            format!("{claimable} tasks on your board are")
        };
        let slots = if free_slots == 1 {
            "1 agent slot is".to_string()
        } else {
            format!("{free_slots} agent slots are")
        };
        let reminder = Reminder {
            text: format!(
                "[Harness note, not the user's words: {tasks} claimable and {slots} free; start agents for them with agent, or do them yourself.]"
            ),
            key,
        };
        reminded.insert(owner.to_string(), key);
        Some(reminder)
    }

    /// Rolls back the claim of `reminder` for `owner`, for a reminder that could not be
    /// delivered. Only the caller that still holds the claimed key rolls back: a newer claim
    /// already replaced it and is left alone.
    pub fn forget_reminder(&self, owner: &str, reminder: &Reminder) {
        let mut reminded = lock(&self.reminded, "owner reminders");
        if reminded.get(owner) == Some(&reminder.key) {
            reminded.remove(owner);
        }
    }

    /// Forgets whatever reminder `owner` was last sent, such as when its queued reminders are
    /// discarded outright.
    pub fn clear_reminder(&self, owner: &str) {
        lock(&self.reminded, "owner reminders").remove(owner);
    }
}

fn in_progress_of(tasks: &[Task], agent: &str) -> bool {
    tasks
        .iter()
        .any(|t| t.status == TaskStatus::InProgress && t.owner.as_deref() == Some(agent))
}

/// The two quality gates, implemented over the user's hooks by `fontaine-core`. A gate can only
/// add work: it blocks or it does not. One that errors or times out reports the failure as a
/// warning through the session's `Approver` and answers `None`, because a gate that failed closed
/// forever would wedge the agents.
pub trait QualityGate: Send + Sync {
    /// Runs the `task_completed` hooks before `agent` completes `task`; `Some(reason)` blocks.
    fn task_completed<'a>(
        &'a self,
        agent: &'a str,
        task: &'a Task,
    ) -> BoxFuture<'a, Option<String>>;

    /// Runs the `teammate_idle` hooks when `name` ended a turn with a task in progress;
    /// `Some(reason)` resumes it with the reason as feedback.
    fn teammate_idle<'a>(&'a self, agent: &'a str, name: &'a str) -> BoxFuture<'a, Option<String>>;
}

/// What a session with teams on holds: the boards, the gates and the resume streaks.
pub struct Teams {
    pub boards: Boards,
    pub gate: Arc<dyn QualityGate>,
    /// A headless run never reminds `main`: a reminder is a root notification, and the run's
    /// answer must stay its own.
    pub headless: bool,
    /// Per agent: resumes in a row and the mutation count of the board that holds its task at the
    /// last of them.
    streaks: Mutex<HashMap<String, (u32, u64)>>,
}

impl Teams {
    pub fn new(boards: Boards, gate: Arc<dyn QualityGate>) -> Teams {
        Teams {
            boards,
            gate,
            headless: false,
            streaks: Mutex::new(HashMap::new()),
        }
    }

    pub fn headless(mut self, headless: bool) -> Teams {
        self.headless = headless;
        self
    }

    /// Whether `agent` may be resumed by a `teammate_idle` block now, counting it. The streak
    /// starts over whenever the board of its `parent`, which holds its task, has changed since the
    /// last such resume.
    pub fn allow_idle_resume(&self, agent: &str, parent: &str) -> bool {
        let mutations = self.boards.mutations_of(parent);
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
            &HashSet::new(),
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
    fn only_the_claimer_or_the_board_owner_updates_an_in_progress_task() {
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
            &HashSet::new(),
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
    }

    #[test]
    fn the_idle_resume_streak_is_capped_and_a_change_of_its_board_resets_it() {
        let board = Arc::new(board());
        let teams = Teams::new(Boards::with_root(board.clone()), Arc::new(NoGate));
        for _ in 0..MAX_IDLE_RESUMES {
            assert!(teams.allow_idle_resume("a", ROOT_AGENT));
        }
        assert!(!teams.allow_idle_resume("a", ROOT_AGENT));
        assert!(
            teams.allow_idle_resume("b", ROOT_AGENT),
            "streaks are per agent"
        );
        add(&board, "news", &[]);
        assert!(
            teams.allow_idle_resume("a", ROOT_AGENT),
            "the board that holds its task moved"
        );
    }

    #[test]
    fn a_board_owner_other_than_main_changes_any_task_on_its_board() {
        let boards = Boards::new(None);
        let board = boards.own("agent-1");
        let id = board.create("agent-1", "work", "", &[]).unwrap().id;
        board.claim("agent-2", &id).unwrap();
        assert!(board.complete("agent-3", &id).is_err());
        assert!(
            board.complete(ROOT_AGENT, &id).is_err(),
            "main is not its owner"
        );
        board.release("agent-1", &id).unwrap();
        board.claim("agent-2", &id).unwrap();
        board.complete("agent-1", &id).unwrap();
    }

    #[test]
    fn ids_are_unique_across_boards_and_only_mains_board_is_saved() {
        #[derive(Default)]
        struct Saved(Mutex<Vec<Snapshot>>);
        impl TaskStore for Saved {
            fn save(&self, snapshot: &Snapshot) {
                self.0.lock().unwrap().push(snapshot.clone());
            }
        }
        let store = Arc::new(Saved::default());
        let boards = Boards::new(Some(store.clone()));
        let a = add(boards.root(), "main's", &[]);
        let b = boards
            .own("agent-1")
            .create("agent-1", "theirs", "", &[])
            .unwrap();
        let c = add(boards.root(), "main's again", &[]);
        assert_eq!((a.as_str(), b.id.as_str(), c.as_str()), ("t1", "t2", "t3"));
        let saved = store.0.lock().unwrap();
        let last = saved.last().unwrap();
        assert_eq!(last.next, 4);
        assert!(
            saved
                .iter()
                .all(|s| s.tasks.iter().all(|t| t.title != "theirs")),
            "a subagent's tasks are never saved"
        );
        assert_eq!(saved[1].next, 3, "its mint saved the counter");
    }

    #[test]
    fn a_task_is_found_on_the_callers_board_or_its_parents_and_nowhere_else() {
        let boards = Boards::new(None);
        let mains = add(boards.root(), "main's", &[]);
        let own = boards
            .own("agent-1")
            .create("agent-1", "own", "", &[])
            .unwrap()
            .id;
        let sibling = boards
            .own("agent-2")
            .create("agent-2", "sibling", "", &[])
            .unwrap()
            .id;
        assert!(boards.find("agent-1", Some(ROOT_AGENT), &mains).is_some());
        assert!(boards.find("agent-1", Some(ROOT_AGENT), &own).is_some());
        assert!(boards.find("agent-1", Some(ROOT_AGENT), &sibling).is_none());
        assert!(boards.find("agent-3", Some("agent-1"), &own).is_some());
        assert!(
            boards.find("agent-3", Some("agent-1"), &mains).is_none(),
            "a grandparent's board is out of reach"
        );
    }

    #[test]
    fn dropping_an_owner_removes_its_board_and_releasing_covers_both_boards() {
        let boards = Boards::new(None);
        let mains = add(boards.root(), "main's", &[]);
        let own = boards
            .own("agent-1")
            .create("agent-1", "own", "", &[])
            .unwrap()
            .id;
        boards.root().claim("agent-1", &mains).unwrap();
        boards
            .get("agent-1")
            .unwrap()
            .claim("agent-1", &own)
            .unwrap();
        let mut released = boards.release_owned_by("agent-1", Some(ROOT_AGENT));
        released.sort();
        assert_eq!(released, vec![mains.clone(), own]);
        assert!(!boards.owns_in_progress("agent-1"));
        boards.drop_owner("agent-1");
        assert!(boards.get("agent-1").is_none());
        boards.drop_owner(ROOT_AGENT);
        assert!(boards.get(ROOT_AGENT).is_some(), "main's board stays");
    }

    #[test]
    fn a_reminder_needs_claimable_tasks_and_free_slots_and_waits_for_a_change() {
        let boards = Boards::new(None);
        assert!(
            boards.pending_reminder(ROOT_AGENT, 2, 2).is_none(),
            "no tasks"
        );
        add(boards.root(), "one", &[]);
        add(boards.root(), "two", &[]);
        assert!(
            boards.pending_reminder(ROOT_AGENT, 0, 0).is_none(),
            "no slots"
        );
        let reminder = boards.pending_reminder(ROOT_AGENT, 2, 2).unwrap();
        assert!(
            reminder
                .text
                .contains("2 tasks on your board are claimable")
        );
        assert!(reminder.text.contains("2 agent slots are free"));
        assert!(
            reminder
                .text
                .starts_with("[Harness note, not the user's words:")
        );
        assert!(
            boards.pending_reminder(ROOT_AGENT, 2, 2).is_none(),
            "the claim is recorded as soon as the reminder is due"
        );
        assert!(
            boards.pending_reminder(ROOT_AGENT, 1, 1).is_some(),
            "a slot went"
        );
        add(boards.root(), "three", &[]);
        assert!(
            boards.pending_reminder(ROOT_AGENT, 2, 2).is_some(),
            "the board moved"
        );
    }

    #[test]
    fn a_rollback_only_forgets_the_key_it_claimed() {
        let boards = Boards::new(None);
        add(boards.root(), "one", &[]);
        let stale = boards.pending_reminder(ROOT_AGENT, 2, 2).unwrap();
        boards.forget_reminder(ROOT_AGENT, &stale);
        let claimed = boards
            .pending_reminder(ROOT_AGENT, 2, 2)
            .expect("a full rollback frees the reminder again");
        add(boards.root(), "two", &[]);
        let newer = boards.pending_reminder(ROOT_AGENT, 2, 2).unwrap();
        assert_ne!(claimed.key, newer.key);
        boards.forget_reminder(ROOT_AGENT, &claimed);
        assert!(
            boards.pending_reminder(ROOT_AGENT, 2, 2).is_none(),
            "the newer claim stands and still dedupes"
        );
    }
}
