//! What the permission pipeline knows about the session, shared by every wrapped tool and
//! changed by the surfaces: the mode, the workspace scope, the rules and the grants live behind
//! a lock so a change takes effect on the very next call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use fontaine_agents::budget::Spend;
use fontaine_agents::mode::{Ladder, RootMode};
use fontaine_agents::tree::AgentTree;
use fontaine_guardian::{EnvironmentConfig, Guardian, LoopBreaker, RuleConfig, Settings};
use fontaine_tools::scope::Scope;
use rig_core::completion::Usage;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audit::{AuditLog, DecisionRecord};
use crate::config::{PermissionMode, Slot};
use crate::ledger::{Ledger, Record};
use crate::permission_rules::{
    CallContext, Posture, Rule, RuleSource, SourcedRule, is_broad_code_execution, matches,
};

const TARGET: &str = "fontaine::telemetry";

/// Decisions kept per agent for the panel.
const DECISIONS_PER_AGENT: usize = 200;

/// Lines of activity kept per agent for the panel.
const AGENT_LOG_LINES: usize = 500;

pub struct WaitGuard {
    state: Arc<SessionState>,
    agent: String,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if let Some(n) = self
            .state
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&self.agent)
        {
            *n = n.saturating_sub(1);
        }
        self.state.agents.poke();
    }
}

/// The root session's mode as the agent tree sees it.
pub fn root_mode(mode: PermissionMode) -> RootMode {
    match mode {
        PermissionMode::Plan => RootMode::Plan,
        PermissionMode::DontAsk => RootMode::Ladder(Ladder::DontAsk),
        PermissionMode::Manual => RootMode::Ladder(Ladder::Manual),
        PermissionMode::AcceptEdits => RootMode::Ladder(Ladder::AcceptEdits),
        PermissionMode::Auto => RootMode::Ladder(Ladder::Auto),
    }
}

/// The permission mode a rung of the ladder stands for.
pub fn permission_mode(rung: Ladder) -> PermissionMode {
    match rung {
        Ladder::DontAsk => PermissionMode::DontAsk,
        Ladder::Manual => PermissionMode::Manual,
        Ladder::AcceptEdits => PermissionMode::AcceptEdits,
        Ladder::Auto => PermissionMode::Auto,
    }
}

/// What an agent file narrows about one child's tools, applied on top of the mode.
#[derive(Clone, Debug, Default)]
pub struct ChildRestrictions {
    /// Calls of a tool named here must match one of that tool's rules.
    pub only: Vec<Rule>,
    /// Calls matching any of these are denied.
    pub deny: Vec<Rule>,
}

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
    /// The session's agents, root included.
    pub agents: Arc<AgentTree>,
    /// Telemetry may carry MCP server and tool names (`log_tool_details`).
    pub reveal_mcp_names: bool,
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

/// Who granted a one-shot retry. The audit record names it, and only a human's grant counts as a
/// human approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetrySource {
    Human,
    /// A user `permission_denied` hook, by id.
    Hook(String),
}

/// A one-shot approval of a call the guardian denied: the same agent, tool and
/// arguments, consumed by the next matching call.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RetryApproval {
    source: RetrySource,
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
    mode: Arc<RwLock<PermissionMode>>,
    agents: Arc<AgentTree>,
    reveal_mcp_names: bool,
    decisions_by_agent: Mutex<HashMap<String, std::collections::VecDeque<DecisionRecord>>>,
    waiting: Mutex<HashMap<String, usize>>,
    agent_logs: Mutex<HashMap<String, Vec<String>>>,
    guardian: RwLock<Arc<Guardian>>,
    pub auto: AutoContext,
    breaker: Mutex<LoopBreaker>,
    denied: Mutex<Vec<DeniedCall>>,
    retries: Mutex<Vec<RetryApproval>>,
    guardian_usage: Mutex<GuardianUsage>,
    guardian_usage_by: Mutex<HashMap<String, GuardianUsage>>,
    subagent_usage: Mutex<GuardianUsage>,
    skill_grants: Mutex<HashMap<String, Vec<SourcedRule>>>,
    child_restrictions: Mutex<HashMap<String, ChildRestrictions>>,
    current_calls: Mutex<HashMap<String, String>>,
    decisions: UnboundedSender<DecisionRecord>,
    feed: tokio::sync::Mutex<DecisionFeed>,
}

impl SessionState {
    pub fn new(params: StateParams) -> SessionState {
        let (decisions, rx) = unbounded_channel();
        let mode = Arc::new(RwLock::new(params.mode));
        {
            let cell = mode.clone();
            params
                .agents
                .set_root_mode(move || root_mode(*cell.read().unwrap_or_else(|e| e.into_inner())));
        }
        SessionState {
            session_id: params.session_id,
            ledger: params.ledger,
            audit: params.audit,
            home: params.home,
            protected_dirs: params.protected_dirs,
            mode,
            agents: params.agents,
            reveal_mcp_names: params.reveal_mcp_names,
            decisions_by_agent: Mutex::default(),
            waiting: Mutex::default(),
            agent_logs: Mutex::default(),
            inner: RwLock::new(Inner {
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
            guardian_usage_by: Mutex::default(),
            subagent_usage: Mutex::default(),
            skill_grants: Mutex::default(),
            child_restrictions: Mutex::default(),
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
        *self.mode.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The session's agents.
    pub fn agents(&self) -> &Arc<AgentTree> {
        &self.agents
    }

    /// Switches the mode for the next call. The change is recorded in the ledger and reported
    /// as a `permission_mode_changed` event; setting the mode it already has does neither.
    pub fn set_mode(&self, to: PermissionMode) {
        let from = std::mem::replace(
            &mut *self.mode.write().unwrap_or_else(|e| e.into_inner()),
            to,
        );
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
    /// The deterministic floor still runs first. Returns the call, or `None` for a bad index or a
    /// call already approved, so one entry queues one approval.
    pub fn approve_retry(&self, index: usize) -> Option<DeniedCall> {
        let mut denied = self.denied.lock().unwrap_or_else(|e| e.into_inner());
        let call = denied.get_mut(index)?;
        if call.retried {
            return None;
        }
        call.retried = true;
        let approval = RetryApproval {
            source: RetrySource::Human,
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

    /// Queues a one-shot retry approval for a call by `agent` with this tool and arguments digest,
    /// the same grant `/denied` then `r` makes, without a list entry to point at.
    pub fn grant_retry(&self, source: RetrySource, agent: &str, tool: &str, args_digest: &str) {
        self.retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(RetryApproval {
                source,
                agent: agent.to_string(),
                tool: tool.to_string(),
                args_digest: args_digest.to_string(),
            });
    }

    /// Consumes the retry approval matching this call, if there is one.
    pub fn take_retry(&self, agent: &str, tool: &str, args_digest: &str) -> Option<RetrySource> {
        let mut retries = self.retries.lock().unwrap_or_else(|e| e.into_inner());
        retries
            .iter()
            .position(|r| r.agent == agent && r.tool == tool && r.args_digest == args_digest)
            .map(|at| retries.remove(at).source)
    }

    /// Counts a guardian call made for `agent`: in the session total, in the agent's own tally
    /// and against the agent's budget.
    pub fn add_guardian_usage(&self, agent: &str, usage: &Usage) {
        let add = |total: &mut GuardianUsage| {
            total.input_tokens += usage.input_tokens.unwrap_or(0);
            total.output_tokens += usage.output_tokens.unwrap_or(0);
            if let Some(cost) = usage.cost {
                *total.cost_usd.get_or_insert(0.0) += cost.total;
            }
        };
        add(&mut self
            .guardian_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner()));
        add(self
            .guardian_usage_by
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(agent.to_string())
            .or_default());
        self.agents.debit(agent, &spend_of(usage));
    }

    /// What the guardian has cost across the whole tree.
    pub fn guardian_usage(&self) -> GuardianUsage {
        *self
            .guardian_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// What the guardian has cost for `agent` alone.
    pub fn guardian_usage_of(&self, agent: &str) -> GuardianUsage {
        self.guardian_usage_by
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .copied()
            .unwrap_or_default()
    }

    /// Counts a subagent's model call. The agent's own tally is the tree's; this is the
    /// session-wide total the headless result reports.
    pub fn add_subagent_usage(&self, usage: &Usage) {
        let mut total = self
            .subagent_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        total.input_tokens += usage.input_tokens.unwrap_or(0);
        total.output_tokens += usage.output_tokens.unwrap_or(0);
        if let Some(cost) = usage.cost {
            *total.cost_usd.get_or_insert(0.0) += cost.total;
        }
    }

    pub fn subagent_usage(&self) -> GuardianUsage {
        *self
            .subagent_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Lets `agent` use the rules a skill carries until the end of the user turn. The invoking
    /// agent only: a subagent it spawns holds none of them.
    pub fn grant_skill_rules(&self, agent: &str, rules: Vec<Rule>) {
        let mut grants = self.skill_grants.lock().unwrap_or_else(|e| e.into_inner());
        let held = grants.entry(agent.to_string()).or_default();
        for rule in rules {
            let rule = SourcedRule::new(rule, RuleSource::Skill);
            if !held.contains(&rule) {
                held.push(rule);
            }
        }
    }

    pub fn skill_grants(&self, agent: &str) -> Vec<SourcedRule> {
        self.skill_grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .cloned()
            .unwrap_or_default()
    }

    /// A user turn ended: the rules skills lent for it end too.
    pub fn end_turn(&self) {
        self.skill_grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn set_child_restrictions(&self, agent: &str, restrictions: ChildRestrictions) {
        self.child_restrictions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.to_string(), restrictions);
    }

    /// Why `agent`'s agent file forbids this call, if it does.
    pub fn child_restriction(&self, agent: &str, call: &CallContext<'_>) -> Option<String> {
        let held = self
            .child_restrictions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let restrictions = held.get(agent)?;
        if let Some(rule) = restrictions
            .deny
            .iter()
            .find(|rule| matches(rule, call, Posture::Deny))
        {
            return Some(format!("this agent type may not use `{rule}`"));
        }
        let limits: Vec<&Rule> = restrictions
            .only
            .iter()
            .filter(|rule| rule.tool == call.tool)
            .collect();
        if !limits.is_empty()
            && !limits
                .iter()
                .any(|rule| matches(rule, call, Posture::Allow))
        {
            return Some(format!(
                "this agent type may use `{}` only as {}",
                call.tool,
                limits
                    .iter()
                    .map(|rule| format!("`{rule}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        None
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
        {
            let mut by_agent = self
                .decisions_by_agent
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let list = by_agent.entry(record.agent_id.clone()).or_default();
            if list.len() == DECISIONS_PER_AGENT {
                list.pop_front();
            }
            list.push_back(record.clone());
        }
        // No consumer is not an error: the audit log already holds the record.
        let _ = self.decisions.send(record);
    }

    /// The decisions made for `agent`, oldest first, for the agents panel.
    pub fn decisions_of(&self, agent: &str) -> Vec<DecisionRecord> {
        self.decisions_by_agent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .map(|list| list.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// `agent` is waiting on a human. The guard ends the wait however the call ends, a cancelled
    /// one included.
    pub fn wait_on_human(self: &Arc<Self>, agent: &str) -> WaitGuard {
        *self
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(agent.to_string())
            .or_default() += 1;
        self.agents.poke();
        WaitGuard {
            state: self.clone(),
            agent: agent.to_string(),
        }
    }

    /// Adds a line to what `agent` has been doing, for the agents panel.
    pub fn log_agent_line(&self, agent: &str, line: String) {
        let mut logs = self.agent_logs.lock().unwrap_or_else(|e| e.into_inner());
        let list = logs.entry(agent.to_string()).or_default();
        if list.len() == AGENT_LOG_LINES {
            list.remove(0);
        }
        list.push(line);
    }

    pub fn agent_log(&self, agent: &str) -> Vec<String> {
        self.agent_logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .cloned()
            .unwrap_or_default()
    }

    pub fn is_waiting(&self, agent: &str) -> bool {
        self.waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .is_some_and(|n| *n > 0)
    }

    /// The name telemetry carries for a tool: an MCP tool is `third-party` unless names are
    /// opted in.
    pub fn telemetry_tool_name<'a>(&self, tool: &'a str) -> &'a str {
        if !self.reveal_mcp_names && fontaine_tools::names::is_mcp(tool) {
            fontaine_tools::guarded::HIDDEN
        } else {
            tool
        }
    }

    /// The decision feed. One consumer at a time holds it.
    pub async fn feed(&self) -> tokio::sync::MutexGuard<'_, DecisionFeed> {
        self.feed.lock().await
    }
}

/// A usage report as the agent tree counts it.
pub fn spend_of(usage: &Usage) -> Spend {
    Spend {
        input_tokens: usage.input_tokens.unwrap_or(0),
        output_tokens: usage.output_tokens.unwrap_or(0),
        cost_usd: usage.cost.map(|cost| cost.total),
    }
}
