//! A session's two stores, plus its task list, schedule and goal files. rig-memory holds the model context, which compaction
//! may rewrite; the ledger beside it is append-only. Both live in `<data dir>/sessions/<session id>/`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use fontaine_tools::error_chain::chain;
use rig_core::DynModel;
use rig_core::catalog::ModelSpec;
use rig_core::completion::Usage;
use rig_core::conversation::Author;
use rig_core::operation::Completion;
pub use rig_memory::CompactReason;
use rig_memory::{
    CompactionPolicy, DirStore, Summarizer, apply_summary, estimate_tokens, load_async, save_async,
    summary_state,
};

use crate::audit::AuditLog;
use crate::checkpoint::CheckpointStore;
use crate::goal::SessionGoal;
use crate::ledger::Ledger;
use crate::owner_only;
use crate::permissions::Approver;
use crate::schedule::{Clock, Scheduler, SystemClock};
use crate::todos::SessionTodos;

pub const SESSIONS_DIR: &str = "sessions";

/// The log, within the session's store, that holds the conversation.
pub const CONVERSATION_LOG: &str = "main";

#[derive(Debug)]
pub enum SessionError {
    /// The session id names a directory that already exists.
    InUse(String),
    /// There is no session with this id to resume or fork.
    Missing(String),
    Io {
        op: SessionOp,
        path: PathBuf,
        source: io::Error,
    },
    Store(String),
}

/// What was being done to the session's files when the io error happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOp {
    Create,
    Open,
}

impl fmt::Display for SessionOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SessionOp::Create => "create",
            SessionOp::Open => "open",
        })
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::InUse(id) => write!(f, "session ID {id} is already in use"),
            SessionError::Missing(id) => write!(f, "no session with ID {id} to resume"),
            SessionError::Io { op, path, source } => {
                write!(f, "cannot {op} {}: {source}", path.display())
            }
            SessionError::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl SessionError {
    /// A reused or unknown session id is a usage error; anything else is a failure of the run.
    pub fn exit_code(&self) -> u8 {
        match self {
            SessionError::InUse(_) | SessionError::Missing(_) => crate::config::CONFIG_EXIT_CODE,
            _ => 1,
        }
    }
}

/// What a compaction did.
#[derive(Clone, Debug, PartialEq)]
pub enum CompactResult {
    /// Nothing was asked of the model: no window on a threshold call, under the threshold, or
    /// nothing new to summarize.
    Skipped,
    /// The conversation is as it was. The cause was already warned through the `Approver`.
    Failed { usage: Option<Usage> },
    /// The summary is saved. `before` and `after` are `estimate_tokens` of the conversation.
    Compacted {
        usage: Option<Usage>,
        before: u64,
        after: u64,
    },
}

impl CompactResult {
    /// The summarizer call's usage, when it answered.
    pub fn usage(&self) -> Option<&Usage> {
        match self {
            CompactResult::Skipped => None,
            CompactResult::Failed { usage } | CompactResult::Compacted { usage, .. } => {
                usage.as_ref()
            }
        }
    }

    fn without_cost(mut self) -> Self {
        if let CompactResult::Failed { usage: Some(usage) }
        | CompactResult::Compacted {
            usage: Some(usage), ..
        } = &mut self
        {
            usage.cost = None;
        }
        self
    }
}

/// The conversation could not be emptied.
#[derive(Debug)]
pub enum ClearError {
    /// A subagent is running or has a report to deliver, so nothing was changed.
    SubagentsRunning,
    Store {
        action: &'static str,
        source: rig_memory::SessionError,
    },
}

impl fmt::Display for ClearError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClearError::SubagentsRunning => f.write_str(
                "subagents are still running or have reports to deliver; /clear is refused until they finish or are killed",
            ),
            ClearError::Store { action, .. } => {
                write!(f, "the conversation could not be {action}")
            }
        }
    }
}

impl std::error::Error for ClearError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClearError::SubagentsRunning => None,
            ClearError::Store { source, .. } => Some(source),
        }
    }
}

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub dir: PathBuf,
    pub ledger: Arc<Ledger>,
    pub audit: Arc<AuditLog>,
    todos: Arc<SessionTodos>,
    scheduler: Arc<Scheduler>,
    goal: Arc<SessionGoal>,
    store: DirStore,
    checkpoints: Arc<CheckpointStore>,
    policy: CompactionPolicy,
    autocompact_window: Option<u64>,
    context_reset: Arc<OnceLock<ContextReset>>,
}

/// What runs when the model's context is emptied or compacted.
pub type ContextReset = Arc<dyn Fn() + Send + Sync>;

impl Session {
    /// Creates the session directory, owner-only, and opens both stores in it. A directory
    /// that already exists is an error, so a reused id can never append to another session.
    pub fn create(data_dir: &Path, id: &str) -> Result<Session, SessionError> {
        let sessions = data_dir.join(SESSIONS_DIR);
        let io_error = |path: &Path| {
            let path = path.to_path_buf();
            move |source| SessionError::Io {
                op: SessionOp::Create,
                path,
                source,
            }
        };
        std::fs::create_dir_all(&sessions).map_err(io_error(&sessions))?;
        let dir = sessions.join(id);
        match owner_only::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(SessionError::InUse(id.to_string()));
            }
            Err(e) => return Err(io_error(&dir)(e)),
        }
        let ledger = Ledger::create(&dir).map_err(io_error(&dir))?;
        let audit = AuditLog::create(data_dir, id)
            .map_err(io_error(&data_dir.join(crate::audit::AUDIT_DIR)))?;
        let store = DirStore::open(&dir).map_err(|e| SessionError::Store(e.to_string()))?;
        let checkpoints = CheckpointStore::open(&dir).map_err(io_error(&dir))?;
        let ledger = Arc::new(ledger);
        Ok(Session {
            id: id.to_string(),
            scheduler: Arc::new(Scheduler::new(&dir, ledger.clone(), Arc::new(SystemClock))),
            goal: Arc::new(SessionGoal::new(&dir, ledger.clone())),
            ledger,
            audit: Arc::new(audit),
            todos: Arc::new(SessionTodos::new(&dir)),
            dir,
            store,
            checkpoints: Arc::new(checkpoints),
            policy: CompactionPolicy::default(),
            autocompact_window: None,
            context_reset: Arc::default(),
        })
    }

    /// Reopens the session `id`: its conversation log and its ledger, with the audit log
    /// continuing. Nothing that was granted during it is restored.
    pub fn open(data_dir: &Path, id: &str) -> Result<Session, SessionError> {
        if !is_session_id(id) {
            return Err(SessionError::Missing(id.to_string()));
        }
        let dir = data_dir.join(SESSIONS_DIR).join(id);
        if !dir.join(crate::ledger::LEDGER_DIR).is_dir() {
            return Err(SessionError::Missing(id.to_string()));
        }
        let io_error = |path: &Path| {
            let path = path.to_path_buf();
            move |source| SessionError::Io {
                op: SessionOp::Open,
                path,
                source,
            }
        };
        let ledger = Ledger::open(&dir).map_err(io_error(&dir))?;
        let audit = AuditLog::create(data_dir, id)
            .map_err(io_error(&data_dir.join(crate::audit::AUDIT_DIR)))?;
        let store = DirStore::open(&dir).map_err(|e| SessionError::Store(e.to_string()))?;
        let checkpoints = CheckpointStore::open(&dir).map_err(io_error(&dir))?;
        let ledger = Arc::new(ledger);
        Ok(Session {
            id: id.to_string(),
            scheduler: Arc::new(Scheduler::load(&dir, ledger.clone(), Arc::new(SystemClock))),
            goal: Arc::new(SessionGoal::load(&dir, ledger.clone())),
            ledger,
            audit: Arc::new(audit),
            todos: Arc::new(SessionTodos::load(&dir)),
            dir,
            store,
            checkpoints: Arc::new(checkpoints),
            policy: CompactionPolicy::default(),
            autocompact_window: None,
            context_reset: Arc::default(),
        })
    }

    /// Whether [`open`](Self::open) would succeed on `id`, decided without writing: `open` repairs
    /// a torn ledger tail and starts the audit log, which a session that is then not used must not get.
    pub fn check_openable(data_dir: &Path, id: &str) -> Result<(), SessionError> {
        if !is_session_id(id) {
            return Err(SessionError::Missing(id.to_string()));
        }
        let dir = data_dir.join(SESSIONS_DIR).join(id);
        let path = dir
            .join(crate::ledger::LEDGER_DIR)
            .join(crate::ledger::LEDGER_FILE);
        if !dir.join(crate::ledger::LEDGER_DIR).is_dir() {
            return Err(SessionError::Missing(id.to_string()));
        }
        let io_error = |source| SessionError::Io {
            op: SessionOp::Open,
            path: dir.clone(),
            source,
        };
        let text = std::fs::read_to_string(&path).map_err(io_error)?;
        let mut lines = text.split_inclusive('\n').peekable();
        while let Some(raw) = lines.next() {
            if raw.trim().is_empty() {
                continue;
            }
            let torn_tail = lines.peek().is_none() && !raw.ends_with('\n');
            if let Err(e) = serde_json::from_str::<crate::ledger::Entry>(raw)
                && !torn_tail
            {
                return Err(io_error(io::Error::other(e)));
            }
        }
        Ok(())
    }

    /// Copies the session `source` under the new id `id` and opens the copy. The audit log
    /// starts empty: it records what this session decides.
    pub fn fork(data_dir: &Path, source: &str, id: &str) -> Result<Session, SessionError> {
        if !is_session_id(source) {
            return Err(SessionError::Missing(source.to_string()));
        }
        let sessions = data_dir.join(SESSIONS_DIR);
        let from = sessions.join(source);
        if !from.is_dir() {
            return Err(SessionError::Missing(source.to_string()));
        }
        let to = sessions.join(id);
        match owner_only::create_dir(&to) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(SessionError::InUse(id.to_string()));
            }
            Err(source) => {
                return Err(SessionError::Io {
                    op: SessionOp::Create,
                    path: to,
                    source,
                });
            }
        }
        copy_tree(&from, &to).map_err(|source| SessionError::Io {
            op: SessionOp::Create,
            path: to.clone(),
            source,
        })?;
        Session::open(data_dir, id)
    }

    /// Sets what runs when `clear` or a successful compaction drops the conversation's earlier
    /// tool results; shared by every clone. The first hook set stays.
    pub fn on_context_reset(&self, hook: ContextReset) {
        // A second hook is ignored: one session has one set of instruction bookkeeping.
        let _ = self.context_reset.set(hook);
    }

    fn context_was_reset(&self) {
        if let Some(hook) = self.context_reset.get() {
            hook();
        }
    }

    /// [`fork`](Self::fork), then names the copy: `name`, else the source's name with
    /// ` (branch)` after it, else none. The copy's ledger carries the source's records, so it
    /// is given its own, or `resolve` would find two sessions of one name.
    pub fn fork_named(
        data_dir: &Path,
        source: &str,
        id: &str,
        name: Option<&str>,
    ) -> Result<Session, SessionError> {
        let copy = Session::fork(data_dir, source, id)?;
        match copy.name_the_branch(name) {
            Ok(()) => Ok(copy),
            Err(e) => {
                // An unnamed copy would carry the source's name and make `resolve` ambiguous.
                let _ = std::fs::remove_dir_all(&copy.dir);
                Err(e)
            }
        }
    }

    fn name_the_branch(&self, name: Option<&str>) -> Result<(), SessionError> {
        let copy = self;
        let name = match name {
            Some(name) => Some(name.to_string()),
            None => copy
                .name()
                .map_err(|source| SessionError::Io {
                    op: SessionOp::Open,
                    path: copy.ledger.path().to_path_buf(),
                    source,
                })?
                .map(|inherited| {
                    let room = NAME_MAX_CHARS - BRANCH_SUFFIX.chars().count();
                    let head: String = inherited.chars().take(room).collect();
                    format!("{}{BRANCH_SUFFIX}", head.trim_end())
                }),
        };
        if let Some(name) = name {
            copy.rename(&name).map_err(|e| SessionError::Io {
                op: SessionOp::Create,
                path: copy.ledger.path().to_path_buf(),
                source: io::Error::other(e),
            })?;
        }
        Ok(())
    }

    /// Names the session, or renames it; the latest name wins. Returns the name as recorded.
    pub fn rename(&self, name: &str) -> Result<String, RenameError> {
        let name = validate_name(name).map_err(RenameError::Name)?;
        self.ledger
            .append(crate::ledger::Record::SessionNamed { name: name.clone() })
            .map_err(RenameError::Ledger)?;
        Ok(name)
    }

    /// The session's name, if it has one.
    pub fn name(&self) -> io::Result<Option<String>> {
        Ok(scan_ledger(self.ledger.path(), None)?.name)
    }

    /// The name `/rename` gives with no argument: drawn from the first thing the human said
    /// since the last `/clear`.
    pub fn suggested_name(&self) -> Option<String> {
        derive_name(self.ledger.view().user_messages.first()?)
    }

    /// The task lists of this session, shared by every clone of it.
    pub fn todos(&self) -> &Arc<SessionTodos> {
        &self.todos
    }

    /// The scheduled prompts of this session, shared by every clone of it.
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }

    /// The goal of this session, shared by every clone of it.
    pub fn goal(&self) -> &Arc<SessionGoal> {
        &self.goal
    }

    /// Runs the schedule on `clock` instead of the wall clock, reloading what is saved. A seam
    /// for tests, which move time themselves; call it before the session is used.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Session {
        self.scheduler = Arc::new(Scheduler::load(&self.dir, self.ledger.clone(), clock));
        self
    }

    pub fn store(&self) -> DirStore {
        self.store.clone()
    }

    /// Compacts the conversation once it passes `window` tokens, when that is sooner than the
    /// model's window less the summarizer's reserve. The kept tail and the summary are capped
    /// at a quarter of it, so a compaction leaves the conversation under it.
    pub fn set_autocompact_window(&mut self, window: Option<u64>) {
        self.autocompact_window = window;
        let mut policy = CompactionPolicy::default();
        if let Some(window) = window {
            let quarter = window / 4;
            policy.keep_recent = policy
                .keep_recent
                .min(usize::try_from(quarter).unwrap_or(usize::MAX));
            let limits = &mut policy.summarizer.limits;
            limits.summary_tokens = limits.summary_tokens.min(quarter.max(1));
        }
        self.policy = policy;
    }

    /// Whether a conversation of `tokens` has outgrown the model's window or the configured one.
    pub fn over_threshold(&self, tokens: u64, spec: &ModelSpec) -> bool {
        fitted_policy(&self.policy, spec.context_window).over_threshold(tokens, spec)
            || self.autocompact_window.is_some_and(|w| tokens > w)
    }

    /// The conversation size above which a threshold compaction runs, `None` when it never
    /// does: the model is not in the catalog, or neither its window nor a setting gives a limit.
    pub fn autocompact_at(&self, spec: Option<&ModelSpec>) -> Option<u64> {
        let spec = spec?;
        let reserve = self.reserve(Some(spec));
        let reserved = spec
            .context_window
            .map(|window| u64::from(window).saturating_sub(reserve));
        match (reserved, self.autocompact_window) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The summarizer's reserve: the part of the model's window a threshold compaction keeps free,
    /// smaller for a small window.
    pub fn reserve(&self, spec: Option<&ModelSpec>) -> u64 {
        let window = spec.and_then(|spec| spec.context_window);
        fitted_policy(&self.policy, window)
            .summarizer
            .limits
            .reserve
    }

    /// Compacts the conversation when it has outgrown the model's window. Nothing happens
    /// when the model's window is not known and no window is configured. `priced` is
    /// `ModelHandle::catalog_billed`: the summarizer call runs outside a turn, so it drops an
    /// unpriced model's cost and tells the telemetry bridge itself.
    pub async fn compact_if_needed(
        &self,
        model: &DynModel<Completion>,
        priced: bool,
        approver: &dyn Approver,
    ) -> CompactResult {
        self.compact_logged(model, priced, CompactReason::Threshold, approver)
            .await
    }

    /// [`compact`](Self::compact) inside a `fontaine.compaction` span.
    pub async fn compact_logged(
        &self,
        model: &DynModel<Completion>,
        priced: bool,
        reason: CompactReason,
        approver: &dyn Approver,
    ) -> CompactResult {
        let why = match reason {
            CompactReason::Asked { .. } => "asked",
            CompactReason::Threshold => "threshold",
            CompactReason::Overflow => "overflow",
        };
        let span = tracing::info_span!(
            target: "fontaine::telemetry",
            "fontaine.compaction",
            model.priced = priced,
            compaction.reason = why,
        );
        let result =
            tracing::Instrument::instrument(self.compact(model, reason, approver), span).await;
        if priced {
            result
        } else {
            result.without_cost()
        }
    }

    /// Summarizes the older part of the conversation with one call to `model` and saves the
    /// result. A compaction that cannot finish leaves the saved conversation as it was, warns,
    /// and never fails the run.
    pub async fn compact(
        &self,
        model: &DynModel<Completion>,
        reason: CompactReason,
        approver: &dyn Approver,
    ) -> CompactResult {
        let warn = |message: &str| warn(approver, message);
        let failed = |usage| CompactResult::Failed { usage };
        let Some(spec) = model.spec() else {
            if matches!(reason, CompactReason::Asked { .. }) {
                warn("the model's catalog entry is unknown, so it cannot be asked to summarize");
                return failed(None);
            }
            return CompactResult::Skipped;
        };
        if reason == CompactReason::Threshold
            && spec.context_window.is_none()
            && self.autocompact_window.is_none()
        {
            return CompactResult::Skipped;
        }
        let mut conversation = match load_async(&self.store, CONVERSATION_LOG).await {
            Ok(conversation) => conversation,
            Err(e) => {
                warn(&format!(
                    "the conversation could not be read: {}",
                    chain(&e)
                ));
                return failed(None);
            }
        };
        let policy = fitted_policy(&self.policy, spec.context_window);
        let before = estimate_tokens(conversation.messages());
        if reason == CompactReason::Threshold && !self.over_threshold(before, spec) {
            return CompactResult::Skipped;
        }
        let state = summary_state(&conversation);
        let plan = match policy.plan_summary(&conversation, &state, &reason, spec) {
            Ok(Some(plan)) => plan,
            Ok(None) => return CompactResult::Skipped,
            Err(e) => {
                warn(&format!(
                    "the model cannot be asked to summarize: {}",
                    chain(&e)
                ));
                return failed(None);
            }
        };
        let response = match model.call(plan.request.clone()).await {
            Ok(response) => response,
            Err(e) => {
                warn(&format!("the summarizing call failed: {}", chain(&e)));
                return failed(None);
            }
        };
        let usage = response.usage;
        let mut state = plan.state;
        state.summary = match Summarizer::summary_text(&response) {
            Ok(text) => text,
            Err(e) => {
                warn(&format!("the summary cannot be used: {}", chain(&e)));
                return failed(Some(usage));
            }
        };
        if apply_summary(&mut conversation, plan.keep_from, &state).is_none() {
            warn("the summary could not be applied");
            return failed(Some(usage));
        }
        let after = estimate_tokens(conversation.messages());
        if let Err(e) = save_async(&self.store, CONVERSATION_LOG, &mut conversation).await {
            warn(&format!(
                "the compacted conversation could not be saved: {}",
                chain(&e)
            ));
            return failed(Some(usage));
        }
        self.context_was_reset();
        CompactResult::Compacted {
            usage: Some(usage),
            before,
            after,
        }
    }

    /// Empties the model's context. The log keeps the old messages as logged changes; only the
    /// live conversation is empty. The compaction asides go too, or the next summary would be
    /// built on the cleared conversation's.
    pub async fn clear(&self) -> Result<(), ClearError> {
        self.replace_conversation("clear", None).await?;
        self.checkpoints.begin_epoch();
        Ok(())
    }

    /// Makes the live conversation what it was before the log record `conv_n`, which a turn
    /// checkpoint noted. The log keeps the rewound states as logged changes, so a later rewind
    /// to an earlier turn still folds correctly, also across a compaction.
    pub async fn rewind_conversation(&self, conv_n: u64) -> Result<(), ClearError> {
        self.replace_conversation("rewind", Some(conv_n)).await
    }

    async fn replace_conversation(&self, by: &str, until: Option<u64>) -> Result<(), ClearError> {
        let fail = |action| move |source| ClearError::Store { action, source };
        let mut conversation = load_async(&self.store, CONVERSATION_LOG)
            .await
            .map_err(fail("read"))?;
        // The compaction records of the restored past stay with its summary entry, or the next
        // compaction would build on an empty state and replace the summary.
        let (entries, compactions) = match until {
            None => (Vec::new(), Vec::new()),
            Some(n) => {
                let past = rig_memory::as_of(&self.store, CONVERSATION_LOG, n)
                    .map_err(fail("read as it was"))?;
                let entries = (0..past.messages().len())
                    .filter_map(|at| past.entry(at))
                    .collect();
                let compactions = past
                    .asides()
                    .filter(|(_, aside)| {
                        aside.kind == rig_memory::COMPACTED
                            && aside.author == rig_memory::summary_author()
                    })
                    .map(|(_, aside)| aside.clone())
                    .collect();
                (entries, compactions)
            }
        };
        let asides: Vec<_> = conversation.asides().map(|(id, _)| id).collect();
        for id in asides {
            conversation.remove_aside(id);
        }
        conversation.set_entries(entries, Author::plugin(by));
        for aside in compactions {
            conversation.add_aside(aside);
        }
        save_async(&self.store, CONVERSATION_LOG, &mut conversation)
            .await
            .map_err(fail("saved"))?;
        self.context_was_reset();
        Ok(())
    }

    pub fn checkpoints(&self) -> &Arc<CheckpointStore> {
        &self.checkpoints
    }

    /// The human submitted a prompt: notes where the conversation and the intent view stand so
    /// a rewind can come back to this point. `recorded` says the prompt is already in the ledger
    /// as the human's message. Returns the turn's ordinal.
    pub fn begin_turn(
        &self,
        prompt: &str,
        recorded: bool,
    ) -> Result<u64, rig_memory::SessionError> {
        let history = rig_memory::history(&self.store, CONVERSATION_LOG)?;
        let conv_n = history.last().map_or(0, |(stamp, _)| stamp.n + 1);
        let held = self.ledger.view().user_messages.len();
        let user_messages_before = held.saturating_sub(usize::from(recorded));
        Ok(self
            .checkpoints
            .begin_turn(conv_n, prompt, user_messages_before))
    }
}

fn warn(approver: &dyn Approver, message: &str) {
    tracing::warn!("compaction skipped: {message}");
    approver.warn(&format!("compaction skipped: {message}"));
}

/// Whether `id` can name a session directory: ids are UUIDs, and nothing that climbs out of the
/// sessions directory is one.
fn is_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Copies the files under `from` into `to`, which exists. Everything written is owner-only.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            owner_only::create_dir(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            owner_only::write(&target, &std::fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

/// The longest session name, in characters.
pub const NAME_MAX_CHARS: usize = 80;

/// The longest name `/rename` derives from the first message, in characters.
const DERIVED_NAME_CHARS: usize = 60;

/// The longest first prompt a listing carries, in characters.
const FIRST_PROMPT_CHARS: usize = 100;

/// Appended to a source's name when a fork of it is not named.
const BRANCH_SUFFIX: &str = " (branch)";

/// Why a session name was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    ControlCharacter,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Empty => f.write_str("a session name cannot be empty"),
            NameError::TooLong => write!(
                f,
                "a session name is at most {NAME_MAX_CHARS} characters long"
            ),
            NameError::ControlCharacter => {
                f.write_str("a session name cannot contain control characters")
            }
        }
    }
}

impl std::error::Error for NameError {}

/// The name `name` stands for: trimmed, 1 to [`NAME_MAX_CHARS`] characters, no control
/// characters.
pub fn validate_name(name: &str) -> Result<String, NameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::ControlCharacter);
    }
    if name.chars().count() > NAME_MAX_CHARS {
        return Err(NameError::TooLong);
    }
    Ok(name.to_string())
}

/// A name drawn from the first non-blank line of `message`, cut to 60 characters. `None` when
/// there is no such line.
pub fn derive_name(message: &str) -> Option<String> {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let flat: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(DERIVED_NAME_CHARS)
        .collect();
    let name = flat.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Renaming a session failed.
#[derive(Debug)]
pub enum RenameError {
    Name(NameError),
    Ledger(io::Error),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenameError::Name(_) => f.write_str("the name is not valid"),
            RenameError::Ledger(_) => f.write_str("the name could not be recorded"),
        }
    }
}

impl std::error::Error for RenameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenameError::Name(source) => Some(source),
            RenameError::Ledger(source) => Some(source),
        }
    }
}

/// A fresh session id.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// What `list` knows of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub name: Option<String>,
    /// The first thing the human said since the last `/clear`: its first line, cut for display.
    pub first_prompt: Option<String>,
    /// When the ledger was last written.
    pub updated: std::time::SystemTime,
    pub primary_root: String,
    /// The session this one was last forked from, if it is a branch.
    pub branched_from: Option<String>,
}

/// What one pass over a ledger yields. A record is skipped when it does not parse: a listing
/// must not fail because one session's ledger is torn.
#[derive(Default)]
struct Scan {
    primary_root: Option<String>,
    name: Option<String>,
    first_prompt: Option<String>,
    branched_from: Option<String>,
}

/// Streams the ledger. With `wanted` (a `path_key`), stops at the first start whose root differs,
/// so the caller can discard the session without reading the rest.
fn scan_ledger(path: &Path, wanted: Option<&str>) -> io::Result<Scan> {
    use crate::ledger::{Entry, Record};
    use std::io::BufRead;
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut scan = Scan::default();
    for line in reader.lines() {
        let line = line?;
        // The file index can hold tens of thousands of paths and says nothing a listing wants.
        if line.contains(r#""kind":"file_index""#) {
            continue;
        }
        let Ok(Entry { record, .. }) = serde_json::from_str(&line) else {
            continue;
        };
        match record {
            Record::SessionStart {
                workspace_roots, ..
            } => {
                if scan.primary_root.is_none() {
                    scan.primary_root = workspace_roots.into_iter().next();
                    if let (Some(root), Some(wanted)) = (&scan.primary_root, wanted)
                        && crate::ledger::path_key(root) != wanted
                    {
                        return Ok(scan);
                    }
                }
            }
            Record::SessionNamed { name } => scan.name = Some(name),
            Record::Resumed { source, how } => {
                if how.starts_with("fork") {
                    scan.branched_from = Some(source);
                }
            }
            Record::UserMessage { text, agent: None } => {
                if scan.first_prompt.is_none() {
                    scan.first_prompt = Some(text);
                }
            }
            Record::ContextCleared {} => scan.first_prompt = None,
            _ => {}
        }
    }
    Ok(scan)
}

fn first_line_cut(text: &str, chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut cut: String = line.chars().take(chars).collect();
    if line.chars().count() > chars {
        cut.push('…');
    }
    Some(cut)
}

/// The sessions under `data_dir`, newest first. With `root`, only those started in it; without,
/// every session whose ledger records a start. A session whose ledger cannot be read is left out.
pub fn list(data_dir: &Path, root: Option<&Path>) -> Vec<SessionInfo> {
    let wanted = root.map(|root| crate::ledger::path_key(&root.display().to_string()));
    let Ok(entries) = std::fs::read_dir(data_dir.join(SESSIONS_DIR)) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if !is_session_id(&id) {
            continue;
        }
        let ledger = entry
            .path()
            .join(crate::ledger::LEDGER_DIR)
            .join(crate::ledger::LEDGER_FILE);
        let Ok(updated) = std::fs::metadata(&ledger).and_then(|m| m.modified()) else {
            continue;
        };
        let Ok(scan) = scan_ledger(&ledger, wanted.as_deref()) else {
            continue;
        };
        let Some(primary_root) = scan.primary_root else {
            continue;
        };
        if wanted
            .as_ref()
            .is_some_and(|wanted| crate::ledger::path_key(&primary_root) != *wanted)
        {
            continue;
        }
        found.push(SessionInfo {
            id,
            name: scan.name,
            first_prompt: scan
                .first_prompt
                .and_then(|text| first_line_cut(&text, FIRST_PROMPT_CHARS)),
            updated,
            primary_root,
            branched_from: scan.branched_from,
        });
    }
    found.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    found
}

/// The id of the most recently updated session whose primary root is `root`.
pub fn latest_for_root(data_dir: &Path, root: &Path) -> Option<String> {
    list(data_dir, Some(root)).into_iter().next().map(|s| s.id)
}

/// A session id or name that did not pick exactly one session.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    Missing(String),
    /// Several sessions started in the root carry this name; their ids, newest first.
    Ambiguous {
        name: String,
        ids: Vec<String>,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::Missing(text) => {
                write!(f, "no session with ID or name {text} to resume")
            }
            ResolveError::Ambiguous { name, ids } => write!(
                f,
                "several sessions are named {name}; resume one by its ID: {}",
                ids.join(", ")
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

impl ResolveError {
    /// An unknown or ambiguous session is a usage error.
    pub fn exit_code(&self) -> u8 {
        crate::config::CONFIG_EXIT_CODE
    }
}

/// The id of the session `text` names: a session directory of that name, else the one session
/// started in `root` that carries the name `text`, compared without regard to case.
pub fn resolve(data_dir: &Path, root: &Path, text: &str) -> Result<String, ResolveError> {
    if is_session_id(text)
        && data_dir
            .join(SESSIONS_DIR)
            .join(text)
            .join(crate::ledger::LEDGER_DIR)
            .is_dir()
    {
        return Ok(text.to_string());
    }
    let wanted = text.trim().to_lowercase();
    let mut ids: Vec<String> = list(data_dir, Some(root))
        .into_iter()
        .filter(|info| {
            info.name
                .as_ref()
                .is_some_and(|name| name.to_lowercase() == wanted)
        })
        .map(|info| info.id)
        .collect();
    match ids.len() {
        0 => Err(ResolveError::Missing(text.to_string())),
        1 => Ok(ids.remove(0)),
        _ => Err(ResolveError::Ambiguous {
            name: text.to_string(),
            ids,
        }),
    }
}

/// The default reserve and summary size leave no room on a small window: the threshold falls
/// to zero and the summarizer is sent nothing. Below twice their sum each is capped at a quarter
/// of the window, so compaction starts by three quarters full and the summarizer gets at
/// least half.
fn fitted_policy(policy: &CompactionPolicy, window: Option<u32>) -> CompactionPolicy {
    let mut fitted = policy.clone();
    let Some(window) = window else {
        return fitted;
    };
    let limits = &mut fitted.summarizer.limits;
    if u64::from(window) < 2 * (limits.reserve + limits.summary_tokens) {
        limits.reserve = limits.reserve.min(u64::from(window / 4));
        limits.summary_tokens = limits.summary_tokens.min(u64::from(window / 4));
    }
    fitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::catalog::{Catalog, ModelSpec};

    fn spec(window: u32) -> ModelSpec {
        let resolved = Catalog::builtin()
            .resolve("openai/gpt-4o-mini")
            .expect("gpt-4o is in the catalog");
        let mut spec: ModelSpec = (*resolved.spec).clone();
        spec.context_window = Some(window);
        spec
    }

    #[test]
    fn default_window_compacts_only_a_long_conversation_with_a_budget() {
        let policy = fitted_policy(&CompactionPolicy::default(), Some(16_384));
        let spec = spec(16_384);
        assert!(!policy.over_threshold(1_000, &spec));
        assert!(policy.over_threshold(13_000, &spec));
        let limits = &policy.summarizer.limits;
        assert!(16_384 - limits.reserve - limits.summary_tokens > 0);
    }

    #[test]
    fn large_window_keeps_default_limits() {
        let policy = CompactionPolicy::default();
        assert_eq!(fitted_policy(&policy, Some(200_000)), policy);
        assert_eq!(fitted_policy(&policy, None), policy);
    }
}
