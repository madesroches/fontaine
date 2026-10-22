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

pub const SESSIONS_DIR: &str = "sessions";

/// The log, within the session's store, that holds the conversation.
pub const CONVERSATION_LOG: &str = "main";

#[derive(Debug)]
pub enum SessionError {
    /// The session id names a directory that already exists.
    InUse(String),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Store(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::InUse(id) => write!(f, "session ID {id} is already in use"),
            SessionError::Io { path, source } => {
                write!(f, "cannot create {}: {source}", path.display())
            }
            SessionError::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionError {}

impl SessionError {
    /// A reused session id is a usage error; anything else is a failure of the run.
    pub fn exit_code(&self) -> u8 {
        match self {
            SessionError::InUse(_) => crate::config::CONFIG_EXIT_CODE,
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
            move |source| SessionError::Io { path, source }
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

    pub fn store(&self) -> DirStore {
        self.store.clone()
    }

    /// Compacts the conversation when it has outgrown the model's window. Nothing happens
    /// when the model's window is not known.
    pub async fn compact_if_needed(&self, model: &DynModel<Completion>) -> Option<Usage> {
        self.compact(model, CompactReason::Threshold).await
    }

    /// Summarizes the older part of the conversation with one call to `model` and saves the
    /// result. A compaction that cannot finish leaves the saved conversation as it was, warns,
    /// and never fails the run. The returned usage is the summarizer call's, when it answered.
    pub async fn compact(
        &self,
        model: &DynModel<Completion>,
        reason: CompactReason,
    ) -> Option<Usage> {
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

fn warn(message: &str) {
    tracing::warn!("compaction skipped: {message}");
    eprintln!("fontaine: warning: compaction skipped: {message}");
}
