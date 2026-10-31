//! What the permission pipeline knows about the session, shared by every wrapped tool and
//! changed by the surfaces: the mode, the workspace scope, the rules and the grants live behind
//! a lock so a change takes effect on the very next call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use fontaine_agents::budget::Spend;
use fontaine_agents::mode::{Ladder, RootMode};
use fontaine_agents::registry::AgentTypes;
use fontaine_agents::tree::AgentTree;
use fontaine_guardian::{EnvironmentConfig, Guardian, LoopBreaker, RuleConfig, Screen, Settings};
use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::lock::{lock, lock_recovering, read, write};
use fontaine_tools::scope::Scope;
use rig_core::completion::Usage;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audit::{AuditLog, Decision, DecisionRecord, args_digest, now_rfc3339};
use crate::config::{PermissionMode, Slot};
use crate::ledger::{Ledger, Record};
use crate::permission_rules::{
    CallContext, Posture, Rule, RuleSource, SourcedRule, is_broad_code_execution, matches,
};
use crate::permissions::Approver;
use crate::preview::DiffPreview;
use crate::project_env::{self, ProjectEnvironment};
use crate::turn::SessionEvent;

const TARGET: &str = "fontaine::telemetry";

/// Why a call from an agent the session's tree no longer holds is refused.
pub const UNREGISTERED_AGENT: &str = "the calling agent is no longer registered";

/// What a surface that shows a subagent's own transcript receives about it.
#[derive(Clone, Debug)]
pub enum AgentViewEvent {
    /// The first message of the view: the task the agent was given, or the words the human typed
    /// to resume it.
    Prompt(String),
    Event(SessionEvent),
}

/// Where every event of a subagent's run is sent, with the agent's id.
pub type AgentEventSink = UnboundedSender<(String, AgentViewEvent)>;

pub struct WaitGuard {
    state: Arc<SessionState>,
    agent: String,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if let Some(n) = lock(&self.state.waiting, "waiting").get_mut(&self.agent) {
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

/// Which edits `auto` lets through after the floor without asking the guardian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoEdits {
    pub tracked: bool,
    pub scratch: bool,
}

impl Default for AutoEdits {
    fn default() -> Self {
        AutoEdits {
            tracked: true,
            scratch: true,
        }
    }
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
    /// Told once when the audit log or the ledger cannot be written.
    pub approver: Arc<dyn Approver>,
    /// The user's home directory, for `~/` rules and the critical-path check.
    pub home: Option<PathBuf>,
    /// Fontaine's own config and data directories, which file writes never reach unprompted.
    pub protected_dirs: Vec<PathBuf>,
    /// The session's scratch root, one of the scope's roots, when it could be created.
    pub scratch: Option<PathBuf>,
    /// The auto-memory store, one of the scope's roots, when auto memory is on and the directory
    /// could be created.
    pub memory_root: Option<PathBuf>,
    pub guardian: Arc<Guardian>,
    pub auto: AutoContext,
    /// The session's agents, root included.
    pub agents: Arc<AgentTree>,
    /// The agent types the session can spawn, which tell the guardian what a spawn can do.
    pub agent_types: Arc<AgentTypes>,
    /// Telemetry may carry MCP server and tool names (`log_tool_details`).
    pub reveal_mcp_names: bool,
    pub auto_edits: AutoEdits,
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

/// The screen stage as a session runs it.
#[derive(Clone)]
pub struct ScreenStage {
    pub screen: Arc<Screen>,
    /// `<account>/<model>`.
    pub label: String,
    /// Score and record every call, but never stop or allow one.
    pub shadow: bool,
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
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cost_usd: Option<f64>,
}

impl GuardianUsage {
    /// Adds one call's tokens and cost.
    pub fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens.unwrap_or(0);
        self.output_tokens += usage.output_tokens.unwrap_or(0);
        self.cache_read_input_tokens += usage.cached_input_tokens.unwrap_or(0);
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        if let Some(cost) = usage.cost {
            *self.cost_usd.get_or_insert(0.0) += cost.total;
        }
    }
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

/// What the pipeline published for a call.
pub enum Published {
    Decision(Box<DecisionRecord>),
    /// The diff of an allowed `edit` or `write`, with the id of the call.
    Preview(String, DiffPreview),
}

/// The receiving end of the decisions the pipeline makes; the turn runner drains it into its
/// event stream.
pub struct DecisionFeed {
    rx: UnboundedReceiver<DecisionRecord>,
    previews: UnboundedReceiver<(String, DiffPreview)>,
}

impl DecisionFeed {
    /// The next thing published, a decision before a preview. Previews travel on a channel of
    /// their own because the audit log holds the decision record and must not carry them.
    pub async fn next_published(&mut self) -> Option<Published> {
        tokio::select! {
            biased;
            Some(decision) = self.rx.recv() => Some(Published::Decision(Box::new(decision))),
            Some((id, preview)) = self.previews.recv() => Some(Published::Preview(id, preview)),
            else => None,
        }
    }

    pub fn try_next_preview(&mut self) -> Option<(String, DiffPreview)> {
        self.previews.try_recv().ok()
    }

    pub async fn next(&mut self) -> Option<DecisionRecord> {
        self.rx.recv().await
    }

    pub fn try_next(&mut self) -> Option<DecisionRecord> {
        self.rx.try_recv().ok()
    }
}

/// A write-only record the session keeps; each reaches the human at most once when it fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sink {
    Audit,
    Ledger,
}

impl Sink {
    fn what(self) -> &'static str {
        match self {
            Sink::Audit => "the audit log",
            Sink::Ledger => "the session ledger",
        }
    }

    /// The `internal_error` class. The io error is not exported: it can contain a path.
    fn class(self) -> &'static str {
        match self {
            Sink::Audit => "audit_write",
            Sink::Ledger => "ledger_write",
        }
    }
}

pub struct SessionState {
    pub session_id: String,
    pub ledger: Arc<Ledger>,
    pub audit: Arc<AuditLog>,
    approver: Arc<dyn Approver>,
    sink_warned: [AtomicBool; 2],
    pub home: Option<PathBuf>,
    pub protected_dirs: Vec<PathBuf>,
    pub scratch: Option<PathBuf>,
    pub memory_root: Option<PathBuf>,
    inner: RwLock<Inner>,
    mode: Arc<RwLock<PermissionMode>>,
    guardian_off: RwLock<bool>,
    setup_warned: AtomicBool,
    agents: Arc<AgentTree>,
    agent_types: Arc<AgentTypes>,
    reveal_mcp_names: bool,
    pub auto_edits: AutoEdits,
    waiting: Mutex<HashMap<String, usize>>,
    agent_sink: Mutex<Option<AgentEventSink>>,
    guardian: RwLock<Arc<Guardian>>,
    screen: RwLock<Option<ScreenStage>>,
    screen_warned: AtomicBool,
    pub auto: AutoContext,
    breaker: Mutex<LoopBreaker>,
    denied: Mutex<Vec<DeniedCall>>,
    retries: Mutex<Vec<RetryApproval>>,
    guardian_usage: Mutex<GuardianUsage>,
    guardian_usage_by: Mutex<HashMap<String, GuardianUsage>>,
    subagent_usage: Mutex<GuardianUsage>,
    goal_usage: Mutex<GuardianUsage>,
    skill_grants: Mutex<HashMap<String, Vec<SourcedRule>>>,
    child_restrictions: Mutex<HashMap<String, ChildRestrictions>>,
    current_calls: Mutex<HashMap<String, String>>,
    decisions: UnboundedSender<DecisionRecord>,
    previews: UnboundedSender<(String, DiffPreview)>,
    feed: tokio::sync::Mutex<DecisionFeed>,
}

impl SessionState {
    pub fn new(params: StateParams) -> SessionState {
        let (decisions, rx) = unbounded_channel();
        let (previews, previews_rx) = unbounded_channel();
        let mode = Arc::new(RwLock::new(params.mode));
        {
            let cell = mode.clone();
            params
                .agents
                .set_root_mode(move || root_mode(*read(&cell, "mode")));
        }
        SessionState {
            session_id: params.session_id,
            ledger: params.ledger,
            audit: params.audit,
            approver: params.approver,
            sink_warned: Default::default(),
            home: params.home,
            protected_dirs: params.protected_dirs,
            scratch: params.scratch,
            memory_root: params.memory_root,
            mode,
            guardian_off: RwLock::new(false),
            setup_warned: AtomicBool::new(false),
            agents: params.agents,
            agent_types: params.agent_types,
            reveal_mcp_names: params.reveal_mcp_names,
            auto_edits: params.auto_edits,
            waiting: Mutex::default(),
            agent_sink: Mutex::default(),
            inner: RwLock::new(Inner {
                scope: params.scope,
                deny: params.deny,
                ask: params.ask,
                allow: params.allow,
            }),
            guardian: RwLock::new(params.guardian),
            screen: RwLock::new(None),
            screen_warned: AtomicBool::new(false),
            auto: params.auto,
            breaker: Mutex::default(),
            denied: Mutex::default(),
            retries: Mutex::default(),
            guardian_usage: Mutex::default(),
            guardian_usage_by: Mutex::default(),
            goal_usage: Mutex::default(),
            subagent_usage: Mutex::default(),
            skill_grants: Mutex::default(),
            child_restrictions: Mutex::default(),
            current_calls: Mutex::new(HashMap::new()),
            decisions,
            previews,
            feed: tokio::sync::Mutex::new(DecisionFeed {
                rx,
                previews: previews_rx,
            }),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        read(&self.inner, "session rules")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        write(&self.inner, "session rules")
    }

    pub fn mode(&self) -> PermissionMode {
        *read(&self.mode, "mode")
    }

    /// The session's agents.
    pub fn agents(&self) -> &Arc<AgentTree> {
        &self.agents
    }

    /// The agent types the `agent` tool can start.
    pub fn agent_types(&self) -> &AgentTypes {
        &self.agent_types
    }

    /// Switches the mode for the next call. The change is recorded in the ledger and reported
    /// as a `permission_mode_changed` event; setting the mode it already has does neither.
    pub fn set_mode(&self, to: PermissionMode) {
        let from = std::mem::replace(&mut *write(&self.mode, "mode"), to);
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

    /// Whether the session escape hatch is on: every root call runs before the floor and the
    /// guardian. The interactive TUI is the only place that sets this.
    pub fn guardian_off(&self) -> bool {
        *read(&self.guardian_off, "guardian off")
    }

    /// Whether the session has already told the human to run `fontaine auto-mode setup`, or has
    /// just applied its result: either way the warning is not raised again.
    pub fn setup_warning_shown(&self) -> bool {
        self.setup_warned.load(Ordering::Relaxed)
    }

    pub fn mark_setup_warning_shown(&self) {
        self.setup_warned.store(true, Ordering::Relaxed);
    }

    /// Makes `record` the project's facts for the next guardian call. The value is filtered to
    /// the remotes recorded when the session started, as session start filters it, and appended
    /// to the ledger; the latest record wins in the ledger view. Returns what was applied.
    pub fn apply_project_environment(&self, record: ProjectEnvironment) -> ProjectEnvironment {
        let remotes = self.ledger.view().git_remotes.clone();
        let record = project_env::filter_to_remotes(record, &remotes);
        self.append(Record::ProjectEnvironment {
            project_environment: record.clone(),
        });
        self.mark_setup_warning_shown();
        record
    }

    /// Turns the escape hatch on or off for the rest of the session. The toggle is recorded in
    /// the ledger and the audit log; it is never persisted and never loosens a subagent.
    pub fn set_guardian_off(&self, off: bool) {
        let from = std::mem::replace(&mut *write(&self.guardian_off, "guardian off"), off);
        if from == off {
            return;
        }
        self.append(Record::GuardianOff { off });
        crate::permissions::record_decision(
            self,
            &DecisionRecord {
                ts: now_rfc3339(),
                session_id: self.session_id.clone(),
                agent_id: ROOT_AGENT.to_string(),
                tool_use_id: None,
                tool: "guardian".to_string(),
                args_digest: args_digest(&serde_json::json!({ "off": off })),
                original_args_digest: None,
                tier: crate::tier::Tier::GuardianOff,
                decision: Decision::Allow,
                rule: Some(if off { "guardian_off" } else { "guardian_on" }.to_string()),
                confidence: None,
                guardian_model: None,
                latency_ms: 0,
                guardian_profile: None,
                guardian_verdict: None,
                guardian_cache_hit: None,
                verdict_transport: None,
                guardian_latency_ms: None,
                fail_closed_reason: None,
                screen_risk: None,
                screen_allow_risk: None,
                screen_steering: None,
                screen_model: None,
                screen_latency_ms: None,
                screen_outcome: None,
                guardian_dropped_input_tokens: None,
                guardian_dropped_provider: None,
                guardian_dropped_model: None,
                guardian_dropped_priced: None,
                permission_mode: self.mode().as_str().to_string(),
                headless: false,
                explanation: Some(if off {
                    "guardian disabled".to_string()
                } else {
                    "guardian enabled".to_string()
                }),
            },
        );
    }

    pub fn guardian(&self) -> Arc<Guardian> {
        read(&self.guardian, "guardian").clone()
    }

    /// The screen stage in front of the guardian, when one is configured.
    pub fn screen(&self) -> Option<ScreenStage> {
        read(&self.screen, "screen").clone()
    }

    pub fn set_screen(&self, stage: Option<ScreenStage>) {
        *write(&self.screen, "screen") = stage;
    }

    /// True the first time it is asked, so a broken screen is reported once per session.
    pub fn first_screen_warning(&self) -> bool {
        !self.screen_warned.swap(true, Ordering::Relaxed)
    }

    /// Replaces the guardian for the next adjudication; its verdict cache starts empty.
    pub fn set_guardian(&self, guardian: Arc<Guardian>) {
        *write(&self.guardian, "guardian") = guardian;
    }

    /// Notes one guardian denial. Returns whether it paused `auto`.
    pub fn guardian_denied(&self) -> bool {
        lock(&self.breaker, "loop breaker").denied()
    }

    /// The guardian allowed a call, or the human did: a run of denials is over.
    pub fn guardian_allowed(&self) {
        lock(&self.breaker, "loop breaker").allowed();
    }

    /// Whether repeated denials have handed the session back to the human.
    pub fn auto_paused(&self) -> bool {
        lock(&self.breaker, "loop breaker").paused()
    }

    /// The human approved a call: `auto` resumes.
    pub fn resume_auto(&self) {
        lock(&self.breaker, "loop breaker").resume();
    }

    pub fn record_denied(&self, call: DeniedCall) {
        lock(&self.denied, "denied").push(call);
    }

    /// Every call the guardian denied, oldest first.
    pub fn denied_calls(&self) -> Vec<DeniedCall> {
        lock(&self.denied, "denied").clone()
    }

    /// Records the human's one-shot approval of denied call `index` (as `/denied` lists it): the
    /// next call by the same agent with the same tool and arguments goes past the guardian.
    /// The deterministic floor still runs first. Returns the call, or `None` for a bad index or a
    /// call already approved, so one entry queues one approval.
    pub fn approve_retry(&self, index: usize) -> Option<DeniedCall> {
        let mut denied = lock(&self.denied, "denied");
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
        lock(&self.retries, "retries").push(approval);
        Some(call)
    }

    /// Queues a one-shot retry approval for a call by `agent` with this tool and arguments digest,
    /// the same grant `/denied` then `r` makes, without a list entry to point at.
    pub fn grant_retry(&self, source: RetrySource, agent: &str, tool: &str, args_digest: &str) {
        lock(&self.retries, "retries").push(RetryApproval {
            source,
            agent: agent.to_string(),
            tool: tool.to_string(),
            args_digest: args_digest.to_string(),
        });
    }

    /// Consumes the retry approval matching this call, if there is one.
    pub fn take_retry(&self, agent: &str, tool: &str, args_digest: &str) -> Option<RetrySource> {
        let mut retries = lock(&self.retries, "retries");
        retries
            .iter()
            .position(|r| r.agent == agent && r.tool == tool && r.args_digest == args_digest)
            .map(|at| retries.remove(at).source)
    }

    /// Counts a guardian call made for `agent`: in the session total, in the agent's own tally
    /// and against the agent's budget.
    pub fn add_guardian_usage(&self, agent: &str, usage: &Usage) {
        lock(&self.guardian_usage, "guardian usage").add(usage);
        lock(&self.guardian_usage_by, "guardian usage by")
            .entry(agent.to_string())
            .or_default()
            .add(usage);
        self.agents.debit(agent, &spend_of(usage));
    }

    /// What the guardian has cost across the whole tree.
    pub fn guardian_usage(&self) -> GuardianUsage {
        *lock(&self.guardian_usage, "guardian usage")
    }

    /// What the guardian has cost for `agent` alone.
    pub fn guardian_usage_of(&self, agent: &str) -> GuardianUsage {
        lock(&self.guardian_usage_by, "guardian usage by")
            .get(agent)
            .copied()
            .unwrap_or_default()
    }

    /// Counts a goal evaluation's model calls: in the session total and against the root, so
    /// the spend a goal's cost cap reads includes them.
    pub fn add_goal_usage(&self, usage: &Usage) {
        lock(&self.goal_usage, "goal usage").add(usage);
        self.agents.debit(ROOT_AGENT, &spend_of(usage));
    }

    /// What goal evaluations have cost this session, which outlives the goal.
    pub fn goal_usage(&self) -> GuardianUsage {
        *lock(&self.goal_usage, "goal usage")
    }

    /// Counts a subagent's model call. The agent's own tally is the tree's; this is the
    /// session-wide total the headless result reports.
    pub fn add_subagent_usage(&self, usage: &Usage) {
        lock(&self.subagent_usage, "subagent usage").add(usage);
    }

    pub fn subagent_usage(&self) -> GuardianUsage {
        *lock(&self.subagent_usage, "subagent usage")
    }

    /// Lets `agent` use the rules a skill carries until the end of the user turn. The invoking
    /// agent only: a subagent it spawns holds none of them.
    pub fn grant_skill_rules(&self, agent: &str, rules: Vec<Rule>) {
        let mut grants = lock(&self.skill_grants, "skill grants");
        let held = grants.entry(agent.to_string()).or_default();
        for rule in rules {
            let rule = SourcedRule::new(rule, RuleSource::Skill);
            if !held.contains(&rule) {
                held.push(rule);
            }
        }
    }

    pub fn skill_grants(&self, agent: &str) -> Vec<SourcedRule> {
        lock(&self.skill_grants, "skill grants")
            .get(agent)
            .cloned()
            .unwrap_or_default()
    }

    /// A user turn ended: the rules skills lent for it end too.
    pub fn end_turn(&self) {
        lock(&self.skill_grants, "skill grants").clear();
    }

    pub fn set_child_restrictions(&self, agent: &str, restrictions: ChildRestrictions) {
        lock(&self.child_restrictions, "child restrictions")
            .insert(agent.to_string(), restrictions);
    }

    /// Why `agent`'s agent file, or any ancestor's, forbids this call: a subagent never holds
    /// more authority than the agent that spawned it.
    pub fn child_restriction(&self, agent: &str, call: &CallContext<'_>) -> Option<String> {
        if !self.agents().lineage_intact(agent) {
            return Some(UNREGISTERED_AGENT.to_string());
        }
        let held = lock(&self.child_restrictions, "child restrictions");
        let mut at = Some(agent.to_string());
        // A tree is a few layers deep; the bound keeps a corrupt parent link finite.
        for _ in 0..64 {
            let id = at?;
            if let Some(why) = held.get(&id).and_then(|r| restriction_on(r, call)) {
                return Some(why);
            }
            at = self.agents().parent_of(&id);
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
            self.sink_failed(Sink::Ledger, &e);
        }
    }

    /// Logs every failure, and warns the human on the first one per sink, so a full disk does
    /// not warn on every decision.
    pub fn sink_failed(&self, sink: Sink, error: &std::io::Error) {
        tracing::warn!(error = %error, "{} could not be written", sink.what());
        tracing::info!(
            target: "fontaine::telemetry",
            class = sink.class(),
            "internal_error"
        );
        if !self.sink_warned[sink as usize].swap(true, Ordering::Relaxed) {
            self.approver.warn(&format!(
                "{} could not be written: {}",
                sink.what(),
                fontaine_tools::error_chain::chain(error)
            ));
        }
    }

    /// Notes which model tool call `agent` is executing, for the audit record. Tool calls run
    /// one at a time, so one slot per agent is enough.
    pub fn set_current_call(&self, agent: &str, call_id: String) {
        lock(&self.current_calls, "current calls").insert(agent.to_string(), call_id);
    }

    /// Forgets the call, so work the harness starts on its own is not attributed to the last
    /// real one.
    pub fn clear_current_call(&self, agent: &str) {
        lock(&self.current_calls, "current calls").remove(agent);
    }

    pub fn current_call(&self, agent: &str) -> Option<String> {
        lock(&self.current_calls, "current calls")
            .get(agent)
            .cloned()
    }

    pub fn publish(&self, record: DecisionRecord) {
        if let Some(sink) = self.view_sink_for(&record.agent_id) {
            // A surface that went away has no one to tell; the audit log holds the record.
            let _ = sink.send((
                record.agent_id.clone(),
                AgentViewEvent::Event(SessionEvent::Decision(Box::new(record))),
            ));
            return;
        }
        // No consumer is not an error: the audit log already holds the record.
        let _ = self.decisions.send(record);
    }

    /// Hands the diff an allowed `edit` or `write` call of `agent` is about to make to whoever
    /// follows the decision feed.
    pub fn publish_preview(&self, agent: &str, tool_use_id: String, preview: DiffPreview) {
        if let Some(sink) = self.view_sink_for(agent) {
            // A surface that went away has no one to tell; the preview is display only.
            let _ = sink.send((
                agent.to_string(),
                AgentViewEvent::Event(SessionEvent::ToolPreview {
                    id: tool_use_id,
                    preview: Box::new(preview),
                }),
            ));
            return;
        }
        // No consumer is not an error: the preview is display only.
        let _ = self.previews.send((tool_use_id, preview));
    }

    /// Where the events of every subagent run go, so a surface can show each agent's own
    /// transcript. With none installed, as in a headless run, they are dropped, and a subagent's
    /// decisions stay on the root feed.
    pub fn set_agent_event_sink(&self, sink: AgentEventSink) {
        // The sender is replaced whole; a panic cannot leave it half-updated.
        *lock_recovering(&self.agent_sink) = Some(sink);
    }

    /// Sends `event` of `agent` to the sink when one is installed.
    pub fn emit_agent_event(&self, agent: &str, event: AgentViewEvent) {
        // The sender is replaced whole; a panic cannot leave it half-updated.
        let sink = lock_recovering(&self.agent_sink).clone();
        if let Some(sink) = sink {
            // A surface that went away has no one to tell.
            let _ = sink.send((agent.to_string(), event));
        }
    }

    /// The sink for a subagent's records, which keep off the root feed while one is installed.
    fn view_sink_for(&self, agent: &str) -> Option<AgentEventSink> {
        if agent == ROOT_AGENT {
            return None;
        }
        // The sender is replaced whole; a panic cannot leave it half-updated.
        lock_recovering(&self.agent_sink).clone()
    }

    /// `agent` is waiting on a human. The guard ends the wait however the call ends, a cancelled
    /// one included.
    pub fn wait_on_human(self: &Arc<Self>, agent: &str) -> WaitGuard {
        *lock(&self.waiting, "waiting")
            .entry(agent.to_string())
            .or_default() += 1;
        self.agents.poke();
        WaitGuard {
            state: self.clone(),
            agent: agent.to_string(),
        }
    }

    pub fn is_waiting(&self, agent: &str) -> bool {
        lock(&self.waiting, "waiting")
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
        unpriced_tokens: if usage.cost.is_none() {
            usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0)
        } else {
            0
        },
    }
}

fn restriction_on(restrictions: &ChildRestrictions, call: &CallContext<'_>) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::DenyAll;
    use fontaine_agents::tree::Limits;

    #[test]
    fn spend_of_counts_the_tokens_of_a_call_with_no_price_as_unpriced() {
        let mut usage = Usage::new();
        usage.input_tokens = Some(30);
        usage.output_tokens = Some(12);
        let unpriced = spend_of(&usage);
        assert_eq!(unpriced.unpriced_tokens, 42);
        assert_eq!(unpriced.cost_usd, None);

        usage.cost = Some(rig_core::completion::Cost::from_total(0.5));
        let priced = spend_of(&usage);
        assert_eq!(priced.unpriced_tokens, 0);
        assert_eq!(priced.cost_usd, Some(0.5));
    }

    #[test]
    #[should_panic(expected = "the loop breaker lock is not poisoned")]
    fn a_poisoned_breaker_panics_instead_of_deciding() {
        let dir = tempfile::tempdir().unwrap();
        let agents = Arc::new(AgentTree::new(Limits::default(), 1));
        agents.set_root_tools(|| {
            fontaine_tools::descriptor::names()
                .map(String::from)
                .collect()
        });
        let state = Arc::new(SessionState::new(StateParams {
            session_id: "poison-test".into(),
            mode: PermissionMode::Auto,
            scope: Arc::new(Scope::new(dir.path().to_path_buf(), [])),
            deny: Vec::new(),
            ask: Vec::new(),
            allow: Vec::new(),
            ledger: Arc::new(Ledger::create(dir.path()).unwrap()),
            audit: Arc::new(AuditLog::create(dir.path(), "poison-test").unwrap()),
            approver: Arc::new(DenyAll),
            home: None,
            protected_dirs: Vec::new(),
            scratch: None,
            memory_root: None,
            guardian: no_guardian(),
            auto: AutoContext::default(),
            agents: agents.clone(),
            agent_types: Arc::new(AgentTypes::builtin()),
            reveal_mcp_names: false,
            auto_edits: AutoEdits::default(),
        }));
        let holder = state.clone();
        let _ = std::thread::spawn(move || {
            let _held = holder.breaker.lock().unwrap();
            panic!("poison the breaker");
        })
        .join();
        state.auto_paused();
    }
}
