//! A session's two stores. rig-memory holds the model context, which compaction may rewrite;
//! the ledger beside it is append-only. Both live in `<data dir>/sessions/<session id>/`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rig_core::DynModel;
use rig_core::completion::Usage;
use rig_core::operation::Completion;
use rig_memory::{
    CompactReason, CompactionPolicy, DirStore, Summarizer, apply_summary, estimate_tokens,
    load_async, save_async, summary_state,
};

use crate::audit::AuditLog;
use crate::ledger::Ledger;
use crate::owner_only;
use crate::permissions::Approver;

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

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub dir: PathBuf,
    pub ledger: Arc<Ledger>,
    pub audit: Arc<AuditLog>,
    store: DirStore,
    policy: CompactionPolicy,
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
        Ok(Session {
            id: id.to_string(),
            dir,
            ledger: Arc::new(ledger),
            audit: Arc::new(audit),
            store,
            policy: CompactionPolicy::default(),
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
        Ok(Session {
            id: id.to_string(),
            dir,
            ledger: Arc::new(ledger),
            audit: Arc::new(audit),
            store,
            policy: CompactionPolicy::default(),
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

    pub fn store(&self) -> DirStore {
        self.store.clone()
    }

    /// Compacts the conversation when it has outgrown the model's window. Nothing happens
    /// when the model's window is not known. `priced` is `ModelHandle::catalog_billed`: the
    /// summarizer call runs outside a turn, so it drops an unpriced model's cost and tells the
    /// telemetry bridge itself.
    pub async fn compact_if_needed(
        &self,
        model: &DynModel<Completion>,
        priced: bool,
        approver: &dyn Approver,
    ) -> Option<Usage> {
        let span = tracing::info_span!(
            target: "fontaine::telemetry",
            "fontaine.compaction",
            model.priced = priced,
        );
        let mut usage = tracing::Instrument::instrument(
            self.compact(model, CompactReason::Threshold, approver),
            span,
        )
        .await?;
        if !priced {
            usage.cost = None;
        }
        Some(usage)
    }

    /// Summarizes the older part of the conversation with one call to `model` and saves the
    /// result. A compaction that cannot finish leaves the saved conversation as it was, warns,
    /// and never fails the run. The returned usage is the summarizer call's, when it answered.
    pub async fn compact(
        &self,
        model: &DynModel<Completion>,
        reason: CompactReason,
        approver: &dyn Approver,
    ) -> Option<Usage> {
        let warn = |message: &str| warn(approver, message);
        let spec = model.spec()?;
        if reason == CompactReason::Threshold && spec.context_window.is_none() {
            return None;
        }
        let mut conversation = match load_async(&self.store, CONVERSATION_LOG).await {
            Ok(conversation) => conversation,
            Err(e) => {
                warn(&format!("the conversation could not be read: {e}"));
                return None;
            }
        };
        if reason == CompactReason::Threshold
            && !self
                .policy
                .over_threshold(estimate_tokens(conversation.messages()), spec)
        {
            return None;
        }
        let state = summary_state(&conversation);
        let plan = match self
            .policy
            .plan_summary(&conversation, &state, &reason, spec)
        {
            Ok(Some(plan)) => plan,
            Ok(None) => return None,
            Err(e) => {
                warn(&format!("the model cannot be asked to summarize: {e}"));
                return None;
            }
        };
        let response = match model.call(plan.request.clone()).await {
            Ok(response) => response,
            Err(e) => {
                warn(&format!("the summarizing call failed: {e}"));
                return None;
            }
        };
        let usage = response.usage;
        let mut state = plan.state;
        state.summary = match Summarizer::summary_text(&response) {
            Ok(text) => text,
            Err(e) => {
                warn(&format!("the summary cannot be used: {e}"));
                return Some(usage);
            }
        };
        if apply_summary(&mut conversation, plan.keep_from, &state).is_none() {
            warn("the summary could not be applied");
            return Some(usage);
        }
        if let Err(e) = save_async(&self.store, CONVERSATION_LOG, &mut conversation).await {
            warn(&format!(
                "the compacted conversation could not be saved: {e}"
            ));
        }
        Some(usage)
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
