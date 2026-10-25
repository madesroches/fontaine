//! A session's two stores, plus its task list and schedule files. rig-memory holds the model context, which compaction
//! may rewrite; the ledger beside it is append-only. Both live in `<data dir>/sessions/<session id>/`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    store: DirStore,
    policy: CompactionPolicy,
    autocompact_window: Option<u64>,
}

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
        let ledger = Arc::new(ledger);
        Ok(Session {
            id: id.to_string(),
            scheduler: Arc::new(Scheduler::new(&dir, ledger.clone(), Arc::new(SystemClock))),
            ledger,
            audit: Arc::new(audit),
            todos: Arc::new(SessionTodos::new(&dir)),
            dir,
            store,
            policy: CompactionPolicy::default(),
            autocompact_window: None,
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
        let ledger = Arc::new(ledger);
        Ok(Session {
            id: id.to_string(),
            scheduler: Arc::new(Scheduler::load(&dir, ledger.clone(), Arc::new(SystemClock))),
            ledger,
            audit: Arc::new(audit),
            todos: Arc::new(SessionTodos::load(&dir)),
            dir,
            store,
            policy: CompactionPolicy::default(),
            autocompact_window: None,
        })
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

    /// The task lists of this session, shared by every clone of it.
    pub fn todos(&self) -> &Arc<SessionTodos> {
        &self.todos
    }

    /// The scheduled prompts of this session, shared by every clone of it.
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
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
        let fail = |action| move |source| ClearError::Store { action, source };
        let mut conversation = load_async(&self.store, CONVERSATION_LOG)
            .await
            .map_err(fail("read"))?;
        let asides: Vec<_> = conversation.asides().map(|(id, _)| id).collect();
        for id in asides {
            conversation.remove_aside(id);
        }
        conversation.set_entries(Vec::new(), Author::plugin("clear"));
        save_async(&self.store, CONVERSATION_LOG, &mut conversation)
            .await
            .map_err(fail("saved"))
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

/// The id of the most recently updated session whose primary root is `root`.
pub fn latest_for_root(data_dir: &Path, root: &Path) -> Option<String> {
    use std::io::BufRead;
    let wanted = crate::ledger::path_key(&root.display().to_string());
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(data_dir.join(SESSIONS_DIR))
        .ok()?
        .flatten()
    {
        let ledger = entry
            .path()
            .join(crate::ledger::LEDGER_DIR)
            .join(crate::ledger::LEDGER_FILE);
        let Ok(file) = std::fs::File::open(&ledger) else {
            continue;
        };
        let Ok(updated) = file.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let mut first = String::new();
        if std::io::BufReader::new(file).read_line(&mut first).is_err() {
            continue;
        }
        let Ok(crate::ledger::Entry {
            record:
                crate::ledger::Record::SessionStart {
                    workspace_roots, ..
                },
            ..
        }) = serde_json::from_str(first.trim())
        else {
            continue;
        };
        let same = workspace_roots
            .first()
            .is_some_and(|primary| crate::ledger::path_key(primary) == wanted);
        if same && best.as_ref().is_none_or(|(at, _)| updated > *at) {
            best = Some((updated, entry.file_name().to_string_lossy().into_owned()));
        }
    }
    best.map(|(_, id)| id)
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
