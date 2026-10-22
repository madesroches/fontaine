//! What the permission pipeline knows about the session, shared by every wrapped tool and
//! changed by the surfaces: the mode, the workspace scope, the rules and the grants live behind
//! a lock so a change takes effect on the very next call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use fontaine_guardian::{EnvironmentConfig, Guardian, LoopBreaker, RuleConfig, Settings};
use fontaine_tools::scope::Scope;
use rig_core::completion::Usage;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audit::{AuditLog, DecisionRecord};
use crate::config::{PermissionMode, Slot};
use crate::ledger::{Ledger, Record};
use crate::permission_rules::{Rule, RuleSource, SourcedRule, is_broad_code_execution};

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
    pub guardian: Arc<Guardian>,
    pub auto: AutoContext,
}

/// The guardian's standing inputs, fixed for the session: the expanded rule lists, the
/// environment prose and the project instructions it reads.
#[derive(Clone, Debug, Default)]
pub struct AutoContext {
    pub environment: EnvironmentConfig,
    pub rules: RuleConfig,
    /// The primary root's and the user's instruction files, as the agent itself reads them.
    pub project_instructions: Option<String>,
}

/// The guardian a session has when nothing configured one: every adjudication is a denial.
pub fn no_guardian() -> Arc<Guardian> {
    Arc::new(Guardian::unavailable(
        "none",
        "no guardian is configured",
        Settings::default(),
    ))
}

/// A guardian denial the human can look at again with `/denied`.
#[derive(Clone, Debug, PartialEq)]
pub struct DeniedCall {
    pub tool: String,
    pub agent: String,
    pub tool_use_id: Option<String>,
    pub args: serde_json::Value,
    pub args_digest: String,
    pub summary: String,
    pub rule: String,
    pub explanation: String,
    /// Set once the human retried it with `r`.
    pub retried: bool,
}

/// A one-shot human approval of a call the guardian denied: the same agent, tool and
/// arguments, consumed by the next matching call.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RetryApproval {
    agent: String,
    tool: String,
    args_digest: String,
}

/// What the guardian has cost so far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GuardianUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub calls: u64,
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
    guardian: RwLock<Arc<Guardian>>,
    pub auto: AutoContext,
    breaker: Mutex<LoopBreaker>,
    denied: Mutex<Vec<DeniedCall>>,
    retries: Mutex<Vec<RetryApproval>>,
    guardian_usage: Mutex<GuardianUsage>,
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
            guardian: RwLock::new(params.guardian),
            auto: params.auto,
            breaker: Mutex::default(),
            denied: Mutex::default(),
            retries: Mutex::default(),
            guardian_usage: Mutex::default(),
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

    pub fn guardian(&self) -> Arc<Guardian> {
        self.guardian
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replaces the guardian for the next adjudication; its verdict cache starts empty.
    pub fn set_guardian(&self, guardian: Arc<Guardian>) {
        *self.guardian.write().unwrap_or_else(|e| e.into_inner()) = guardian;
    }

    /// Notes one guardian denial. Returns whether it paused `auto`.
    pub fn guardian_denied(&self) -> bool {
        self.breaker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .denied()
    }

    /// The guardian allowed a call, or the human did: a run of denials is over.
    pub fn guardian_allowed(&self) {
        self.breaker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allowed();
    }

    /// Whether repeated denials have handed the session back to the human.
    pub fn auto_paused(&self) -> bool {
        self.breaker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .paused()
    }

    /// The human approved a call: `auto` resumes.
    pub fn resume_auto(&self) {
        self.breaker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resume();
    }

    pub fn record_denied(&self, call: DeniedCall) {
        self.denied
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call);
    }

    /// Every call the guardian denied, oldest first.
    pub fn denied_calls(&self) -> Vec<DeniedCall> {
        self.denied
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Records the human's one-shot approval of denied call `index` (as `/denied` lists it): the
    /// next call by the same agent with the same tool and arguments goes past the guardian.
    /// The deterministic floor still runs first. Returns the call, or `None` for a bad index.
    pub fn approve_retry(&self, index: usize) -> Option<DeniedCall> {
        let mut denied = self.denied.lock().unwrap_or_else(|e| e.into_inner());
        let call = denied.get_mut(index)?;
        call.retried = true;
        let approval = RetryApproval {
            agent: call.agent.clone(),
            tool: call.tool.clone(),
            args_digest: call.args_digest.clone(),
        };
        let call = call.clone();
        drop(denied);
        self.retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(approval);
        Some(call)
    }

    /// Consumes the retry approval matching this call, if there is one.
    pub fn take_retry(&self, agent: &str, tool: &str, args_digest: &str) -> bool {
        let mut retries = self.retries.lock().unwrap_or_else(|e| e.into_inner());
        match retries
            .iter()
            .position(|r| r.agent == agent && r.tool == tool && r.args_digest == args_digest)
        {
            Some(at) => {
                retries.remove(at);
                true
            }
            None => false,
        }
    }

    pub fn add_guardian_usage(&self, usage: &Usage) {
        let mut total = self
            .guardian_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        total.calls += 1;
        total.input_tokens += usage.input_tokens.unwrap_or(0);
        total.output_tokens += usage.output_tokens.unwrap_or(0);
        if let Some(cost) = usage.cost {
            *total.cost_usd.get_or_insert(0.0) += cost.total;
        }
    }

    pub fn guardian_usage(&self) -> GuardianUsage {
        *self
            .guardian_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// The allow rules `auto` sets aside while it is on: broad code-execution rules, whether
    /// configured or granted during the session.
    pub fn suspended_allow_rules(&self) -> Vec<String> {
        self.rules()
            .allow
            .iter()
            .filter(|rule| is_broad_code_execution(&rule.rule))
            .map(SourcedRule::label)
            .collect()
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
