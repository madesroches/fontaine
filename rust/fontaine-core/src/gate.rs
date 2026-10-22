//! What the permission pipeline knows about the session, shared by every wrapped tool and
//! changed by the surfaces: the mode, the workspace scope, the rules and the grants live behind
//! a lock so a change takes effect on the very next call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use fontaine_tools::scope::Scope;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audit::{AuditLog, DecisionRecord};
use crate::config::{PermissionMode, Slot};
use crate::ledger::{Ledger, Record};
use crate::permission_rules::{Rule, RuleSource, SourcedRule};

const TARGET: &str = "fontaine::telemetry";

pub struct StateParams {
    pub session_id: String,
    pub mode: PermissionMode,
    pub scope: Arc<Scope>,
    pub deny: Vec<SourcedRule>,
    pub ask: Vec<SourcedRule>,
    pub allow: Vec<SourcedRule>,
    pub ledger: Arc<Ledger>,
    pub audit: Arc<AuditLog>,
    /// The user's home directory, for `~/` rules and the critical-path check.
    pub home: Option<PathBuf>,
    /// Fontaine's own config and data directories, which file writes never reach unprompted.
    pub protected_dirs: Vec<PathBuf>,
}

/// The rules in force, as they stand now.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub deny: Vec<SourcedRule>,
    pub ask: Vec<SourcedRule>,
    /// Configured allow rules followed by the grants made during the session.
    pub allow: Vec<SourcedRule>,
}

struct Inner {
    mode: PermissionMode,
    scope: Arc<Scope>,
    deny: Vec<SourcedRule>,
    ask: Vec<SourcedRule>,
    allow: Vec<SourcedRule>,
}

/// The receiving end of the decisions the pipeline makes; the turn runner drains it into its
/// event stream.
pub struct DecisionFeed {
    rx: UnboundedReceiver<DecisionRecord>,
}

impl DecisionFeed {
    pub async fn next(&mut self) -> Option<DecisionRecord> {
        self.rx.recv().await
    }

    pub fn try_next(&mut self) -> Option<DecisionRecord> {
        self.rx.try_recv().ok()
    }
}

pub struct SessionState {
    pub session_id: String,
    pub ledger: Arc<Ledger>,
    pub audit: Arc<AuditLog>,
    pub home: Option<PathBuf>,
    pub protected_dirs: Vec<PathBuf>,
    inner: RwLock<Inner>,
    current_calls: Mutex<HashMap<String, String>>,
    decisions: UnboundedSender<DecisionRecord>,
    feed: tokio::sync::Mutex<DecisionFeed>,
}

impl SessionState {
    pub fn new(params: StateParams) -> SessionState {
        let (decisions, rx) = unbounded_channel();
        SessionState {
            session_id: params.session_id,
            ledger: params.ledger,
            audit: params.audit,
            home: params.home,
            protected_dirs: params.protected_dirs,
            inner: RwLock::new(Inner {
                mode: params.mode,
                scope: params.scope,
                deny: params.deny,
                ask: params.ask,
                allow: params.allow,
            }),
            current_calls: Mutex::new(HashMap::new()),
            decisions,
            feed: tokio::sync::Mutex::new(DecisionFeed { rx }),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    pub fn mode(&self) -> PermissionMode {
        self.read().mode
    }

    /// Switches the mode for the next call. The change is recorded in the ledger and reported
    /// as a `permission_mode_changed` event; setting the mode it already has does neither.
    pub fn set_mode(&self, to: PermissionMode) {
        let from = std::mem::replace(&mut self.write().mode, to);
        if from == to {
            return;
        }
        self.append(Record::ModeChange {
            from: from.as_str().to_string(),
            to: to.as_str().to_string(),
        });
        tracing::info!(
            target: TARGET,
            from = from.as_str(),
            to = to.as_str(),
            "permission_mode_changed"
        );
    }

    pub fn scope(&self) -> Arc<Scope> {
        self.read().scope.clone()
    }

    pub fn set_scope(&self, scope: Arc<Scope>) {
        self.write().scope = scope;
    }

    pub fn rules(&self) -> Rules {
        let inner = self.read();
        Rules {
            deny: inner.deny.clone(),
            ask: inner.ask.clone(),
            allow: inner.allow.clone(),
        }
    }

    /// Adds an allow rule that holds from the next call. An "allow session" answer uses
    /// [`RuleSource::Session`]; an "always allow" answer, which is also written to the user's
    /// settings file, the user scope.
    pub fn grant(&self, rule: Rule, source: RuleSource) {
        let rule = SourcedRule::new(rule, source);
        let mut inner = self.write();
        if !inner.allow.contains(&rule) {
            inner.allow.push(rule);
        }
    }

    /// The user-scope source a persisted "always allow" rule is labelled with.
    pub const ALWAYS: RuleSource = RuleSource::Config(Slot::User);

    pub fn append(&self, record: Record) {
        if let Err(e) = self.ledger.append(record) {
            tracing::warn!(error = %e, "the session ledger could not be written");
        }
    }

    /// Notes which model tool call `agent` is executing, for the audit record. Tool calls run
    /// one at a time, so one slot per agent is enough.
    pub fn set_current_call(&self, agent: &str, call_id: String) {
        self.current_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.to_string(), call_id);
    }

    pub fn current_call(&self, agent: &str) -> Option<String> {
        self.current_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .cloned()
    }

    pub fn publish(&self, record: DecisionRecord) {
        // No consumer is not an error: the audit log already holds the record.
        let _ = self.decisions.send(record);
    }

    /// The decision feed. One consumer at a time holds it.
    pub async fn feed(&self) -> tokio::sync::MutexGuard<'_, DecisionFeed> {
        self.feed.lock().await
    }
}
