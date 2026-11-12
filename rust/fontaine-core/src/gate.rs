//! What the permission pipeline knows about the session, shared by every wrapped tool and
//! changed by the surfaces: the mode, the workspace scope, the rules and the grants live behind
//! a lock so a change takes effect on the very next call.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use fontaine_agents::budget::Spend;
use fontaine_agents::mode::{Ladder, RootMode};
use fontaine_agents::registry::AgentTypes;
use fontaine_agents::tree::AgentTree;
use fontaine_guardian::composition::Composition;
use fontaine_guardian::{EnvironmentConfig, Guardian, LoopBreaker, RuleConfig, Settings};
use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::live::Live;
use fontaine_tools::lock::{lock, lock_recovering, read, write};
use fontaine_tools::scope::Scope;
use rig_core::completion::{UnsupportedOption, Usage};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;

use crate::afk_report::{AfkSource, AfkStop};
use crate::audit::{AuditLog, Decision, DecisionRecord, args_digest};
use crate::config::{PermissionMode, Slot};
use crate::effort::Effort;
use crate::ledger::{Ledger, Record};
use crate::permission_rules::{
    CallContext, Posture, Rule, RuleSource, SourcedRule, is_broad_code_execution, matches,
};
use crate::permissions::{Approver, PauseChoice};
use crate::preview::DiffPreview;
use crate::project_env::{self, ProjectEnvironment};
use crate::tier::Tier;
use crate::turn::SessionEvent;
use crate::usage_ledger::{Bucket, Served, UsageLedger, UsageSource};
use crate::usage_limit::UsageLimit;
use crate::usage_pause::{PauseEnd, PauseEvent, PauseInfo, UsagePause, describe_reset};

const TARGET: &str = "fontaine::telemetry";

/// Why a call from an agent the session's tree no longer holds is refused.
pub const UNREGISTERED_AGENT: &str = "the calling agent is no longer registered";

/// AFK mode as the pipeline reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AfkState {
    /// The stretch that is running: `Some` exactly while the session's mode is `afk`.
    pub stretch: Option<AfkStretch>,
    /// The temperature stop no review has cleared yet.
    pub stopped: Option<AfkStop>,
}

/// What `SessionState::set_mode` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModeChanged {
    /// False when the session was already in the mode.
    pub changed: bool,
    /// The ledger position of the `Afk` record that started the AFK stretch this change ended;
    /// the surface reviews that stretch.
    pub closed_stretch: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AfkStretch {
    /// The ledger position of the record that started the stretch.
    pub started_seq: u64,
    pub source: AfkSource,
}

/// Why AFK mode and the guardian escape hatch cannot both be on, or why AFK cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfkRefusal {
    /// Entering AFK mode while the guardian is off: nothing would be converted to a denial.
    GuardianOff,
    /// `/guardian-disable` while AFK mode is on.
    AfkOn,
    /// Either, while a temperature stop waits for a review.
    Stopped,
    /// Entering AFK mode when the ledger cannot record the start: the stretch would never open in the
    /// view, so its temperature could not rise. The write failure itself is reported as a warning.
    LedgerUnwritable,
}

impl std::fmt::Display for AfkRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AfkRefusal::GuardianOff => {
                "the guardian is disabled for this session: run /guardian-enable first"
            }
            AfkRefusal::AfkOn => {
                "AFK mode is on: leave it with the mode-cycle key before disabling the guardian"
            }
            AfkRefusal::Stopped => {
                "the session is stopped by the AFK temperature ceiling: run /afk review first"
            }
            AfkRefusal::LedgerUnwritable => {
                "AFK mode did not start: the session ledger could not be written"
            }
        })
    }
}

/// Why an unattended turn may not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Repeated denials paused `auto` while AFK is on.
    BreakerPaused,
    /// The temperature ceiling stopped the session.
    Stopped,
}

/// The message every call gets while a temperature stop waits for a review.
pub const AFK_STOPPED_MESSAGE: &str = "the session was stopped by the AFK temperature ceiling; no further tool calls run until the user reviews it";

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
        PermissionMode::Auto | PermissionMode::Afk => RootMode::Ladder(Ladder::Auto),
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
    pub agent_types: Arc<Live<AgentTypes>>,
    /// Telemetry may carry MCP server and tool names (`log_tool_details`).
    pub reveal_mcp_names: bool,
    pub auto_edits: AutoEdits,
    /// The reasoning effort the main agent starts at: `[model] effort` or `--effort`.
    pub effort: Option<Effort>,
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

/// The guardian's composition as a session runs it.
#[derive(Clone)]
pub struct CompositionStage {
    pub composition: Arc<Composition>,
    /// The `<account>/<model>` of every read, comma-joined.
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

/// A trusted skill active for an agent: its name and the wildcard shell rules it declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedSkillNote {
    pub name: String,
    pub declared: Vec<String>,
}

/// How long a surface lets a usage pause last. The default waits for as long as it takes, which
/// only a session whose human can cancel should.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageWait {
    /// The longest wait; `None` for no limit.
    pub cap: Option<std::time::Duration>,
    /// The longest wait while AFK is on, when nobody is there to cancel.
    pub afk_cap: Option<std::time::Duration>,
    /// Do not wait at all: the first pause ends at once as a failure.
    pub exit: bool,
}

/// A usage pause that ended in failure, which the run that waited on it must not outlive.
#[derive(Clone, Debug)]
pub struct UsagePauseEnded {
    pub end: PauseEnd,
    pub info: PauseInfo,
}

impl std::fmt::Display for UsagePauseEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let how = match self.end {
            PauseEnd::CapReached => "the wait reached its limit",
            _ => "the wait was cancelled",
        };
        write!(
            f,
            "the {} account's usage window is spent ({}); {how}",
            self.info.account,
            describe_reset(self.info.reset_at, std::time::SystemTime::now()),
        )
    }
}

impl std::error::Error for UsagePauseEnded {}

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
    usage_pause: UsagePause,
    usage_wait: RwLock<UsageWait>,
    /// Where the guardian's account reads its own usage figures, for a pause on that account.
    guardian_quota: RwLock<Option<Arc<dyn crate::quota::QuotaSource>>>,
    afk: RwLock<AfkState>,
    afk_ceiling: RwLock<f64>,
    /// The blocked outcome was already announced for the breaker pause now in force.
    afk_block_announced: AtomicBool,
    /// Counts temperature stops, so a surface that holds a receiver sees each one exactly once.
    afk_stops: watch::Sender<u64>,
    setup_warned: AtomicBool,
    effort: RwLock<Option<Effort>>,
    /// Every model and level the human was told were incompatible, so a session says it once per
    /// pairing however main turns and child spawns interleave.
    effort_warned: Mutex<HashSet<(String, Effort)>>,
    agents: Arc<AgentTree>,
    agent_types: Arc<Live<AgentTypes>>,
    reveal_mcp_names: bool,
    pub auto_edits: AutoEdits,
    waiting: Mutex<HashMap<String, usize>>,
    agent_sink: Mutex<Option<AgentEventSink>>,
    guardian: RwLock<Arc<Guardian>>,
    composition: RwLock<Option<CompositionStage>>,
    composition_warned: AtomicBool,
    auto: RwLock<AutoContext>,
    breaker: Mutex<LoopBreaker>,
    denied: Mutex<Vec<DeniedCall>>,
    retries: Mutex<Vec<RetryApproval>>,
    usage: UsageLedger,
    skill_grants: Mutex<HashMap<String, Vec<SourcedRule>>>,
    skill_notes: Mutex<HashMap<String, Vec<TrustedSkillNote>>>,
    /// Skills the human was told lend nothing, so a session says it once per skill.
    skill_warned: Mutex<HashSet<String>>,
    child_restrictions: Mutex<HashMap<String, ChildRestrictions>>,
    current_calls: Mutex<HashMap<String, String>>,
    /// How many tool calls each agent has made, which resets a hook's continuation streak: an
    /// agent that keeps working between stops is not looping.
    tool_calls: Mutex<HashMap<String, u64>>,
    /// Context hooks added that the next root prompt carries, as harness notes.
    hook_context: Mutex<Vec<String>>,
    prompt_context: Mutex<Vec<String>>,
    decisions: UnboundedSender<DecisionRecord>,
    previews: UnboundedSender<(String, DiffPreview)>,
    feed: tokio::sync::Mutex<DecisionFeed>,
}

impl SessionState {
    pub fn new(params: StateParams) -> SessionState {
        let (decisions, rx) = unbounded_channel();
        let (previews, previews_rx) = unbounded_channel();
        let mode = Arc::new(RwLock::new(params.mode));
        let restored_stop = params.ledger.view().afk().stopped().cloned();
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
            usage_pause: UsagePause::new(),
            usage_wait: RwLock::new(UsageWait::default()),
            guardian_quota: RwLock::new(None),
            afk: RwLock::new(AfkState {
                stretch: None,
                stopped: restored_stop,
            }),
            afk_ceiling: RwLock::new(fontaine_guardian::temperature::DEFAULT_CEILING),
            afk_block_announced: AtomicBool::new(false),
            afk_stops: watch::channel(0).0,
            setup_warned: AtomicBool::new(false),
            effort: RwLock::new(params.effort),
            effort_warned: Mutex::new(HashSet::new()),
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
            composition: RwLock::new(None),
            composition_warned: AtomicBool::new(false),
            auto: RwLock::new(params.auto),
            breaker: Mutex::default(),
            denied: Mutex::default(),
            retries: Mutex::default(),
            usage: UsageLedger::default(),
            skill_grants: Mutex::default(),
            skill_notes: Mutex::default(),
            skill_warned: Mutex::default(),
            child_restrictions: Mutex::default(),
            current_calls: Mutex::new(HashMap::new()),
            tool_calls: Mutex::new(HashMap::new()),
            hook_context: Mutex::new(Vec::new()),
            prompt_context: Mutex::new(Vec::new()),
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
    pub fn agent_types(&self) -> Arc<AgentTypes> {
        self.agent_types.get()
    }

    /// The guardian's standing inputs as the next adjudication reads them.
    pub fn auto_context(&self) -> AutoContext {
        read(&self.auto, "auto context").clone()
    }

    /// Replaces the guardian's rule prose for the next adjudication; the verdict cache key
    /// digests the rules, so an earlier verdict never answers for the new ones.
    pub fn set_auto_context(&self, auto: AutoContext) {
        *write(&self.auto, "auto context") = auto;
    }

    /// Swaps the deny and ask rules a plugin brought: every `RuleSource::Plugin` entry is
    /// replaced, every other entry is left alone.
    pub fn set_plugin_rules(&self, deny: Vec<SourcedRule>, ask: Vec<SourcedRule>) {
        let mut inner = write(&self.inner, "rules");
        inner
            .deny
            .retain(|entry| entry.source != RuleSource::Plugin);
        inner.deny.extend(deny);
        inner.ask.retain(|entry| entry.source != RuleSource::Plugin);
        inner.ask.extend(ask);
    }

    /// Switches the mode for the next call. The change is recorded in the ledger and reported
    /// as a `permission_mode_changed` event; setting the mode it already has does neither.
    ///
    /// Entering `afk` is refused while the guardian is off, while a temperature stop waits for a
    /// review, or when the ledger cannot record the start; the mode is then unchanged. Leaving it
    /// is never refused and returns the closed stretch for the surface to review. `source` is
    /// recorded only when AFK mode starts or ends: it is never entered by anything but the
    /// user's own flag or key press, never persisted and never restored by a resume.
    pub fn set_mode(
        &self,
        to: PermissionMode,
        source: AfkSource,
    ) -> Result<ModeChanged, AfkRefusal> {
        self.switch_mode(write(&self.afk, "afk"), to, source)
    }

    /// `set_mode` for a caller that already holds the AFK lock, so a stop and the mode it ends
    /// change together. The AFK lock is taken before the mode lock, as in `set_guardian_off`.
    fn switch_mode(
        &self,
        mut afk: std::sync::RwLockWriteGuard<'_, AfkState>,
        to: PermissionMode,
        source: AfkSource,
    ) -> Result<ModeChanged, AfkRefusal> {
        let mut mode = write(&self.mode, "mode");
        let from = *mode;
        if from == to {
            return Ok(ModeChanged::default());
        }
        let mut closed_stretch = None;
        if to == PermissionMode::Afk {
            if afk.stopped.is_some() {
                return Err(AfkRefusal::Stopped);
            }
            if self.guardian_off() {
                return Err(AfkRefusal::GuardianOff);
            }
            let Some(started_seq) = self.append_seq(Record::Afk { on: true, source }) else {
                return Err(AfkRefusal::LedgerUnwritable);
            };
            afk.stretch = Some(AfkStretch {
                started_seq,
                source,
            });
        } else if from == PermissionMode::Afk {
            self.append(Record::Afk { on: false, source });
            closed_stretch = afk.stretch.take().map(|stretch| stretch.started_seq);
        }
        *mode = to;
        self.append(Record::ModeChange {
            from: from.as_str().to_string(),
            to: to.as_str().to_string(),
        });
        // The session events read the mode and the AFK state, so both guards go first.
        drop(mode);
        drop(afk);
        if to == PermissionMode::Afk || from == PermissionMode::Afk {
            let on = to == PermissionMode::Afk;
            self.record_session_event(
                "afk",
                serde_json::json!({ "on": on, "source": source }),
                Tier::Afk,
                if on { "afk_on" } else { "afk_off" },
                if on { "AFK mode on" } else { "AFK mode off" },
            );
        }
        tracing::info!(
            target: TARGET,
            from = from.as_str(),
            to = to.as_str(),
            "permission_mode_changed"
        );
        Ok(ModeChanged {
            changed: true,
            closed_stretch,
        })
    }

    /// Whether the session escape hatch is on: every root call runs before the floor and the
    /// guardian. The interactive TUI is the only place that sets this.
    pub fn guardian_off(&self) -> bool {
        *read(&self.guardian_off, "guardian off")
    }

    /// The session's reasoning effort, which the next turn sends; `None` sends nothing.
    pub fn effort(&self) -> Option<Effort> {
        *read(&self.effort, "effort")
    }

    pub fn set_effort(&self, effort: Option<Effort>) {
        *write(&self.effort, "effort") = effort;
    }

    /// Tells the human, once per model and level, that `model` cannot take the session's effort
    /// and the turn runs without it.
    pub fn warn_effort_refused(&self, model: &str, effort: Effort, refusal: &UnsupportedOption) {
        let first = lock(&self.effort_warned, "effort warning").insert((model.to_string(), effort));
        if !first {
            return;
        }
        self.approver.warn(&format!(
            "effort `{effort}` is not used with {model}: {}; the model's own default applies",
            refusal.reason
        ));
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
    /// the ledger and the audit log; it is never persisted and never loosens a subagent. Refused
    /// while AFK mode is on or stopped: it would turn off the very denials AFK relies on.
    pub fn set_guardian_off(&self, off: bool) -> Result<(), AfkRefusal> {
        // The AFK lock is taken first here and in `set_mode`, so the two cannot interleave.
        let afk = read(&self.afk, "afk");
        if off {
            if afk.stopped.is_some() {
                return Err(AfkRefusal::Stopped);
            }
            if afk.stretch.is_some() {
                return Err(AfkRefusal::AfkOn);
            }
        }
        let from = std::mem::replace(&mut *write(&self.guardian_off, "guardian off"), off);
        drop(afk);
        if from == off {
            return Ok(());
        }
        self.append(Record::GuardianOff { off });
        self.record_session_event(
            "guardian",
            serde_json::json!({ "off": off }),
            Tier::GuardianOff,
            if off { "guardian_off" } else { "guardian_on" },
            if off {
                "guardian disabled"
            } else {
                "guardian enabled"
            },
        );
        Ok(())
    }

    /// The AFK state now.
    pub fn afk(&self) -> AfkState {
        read(&self.afk, "afk").clone()
    }

    /// Whether AFK mode is on: nobody can be asked.
    pub fn afk_on(&self) -> bool {
        self.mode() == PermissionMode::Afk
    }

    /// The temperature at which an AFK stretch stops.
    pub fn afk_ceiling(&self) -> f64 {
        *read(&self.afk_ceiling, "afk ceiling")
    }

    pub fn set_afk_ceiling(&self, ceiling: f64) {
        *write(&self.afk_ceiling, "afk ceiling") = ceiling;
    }

    /// Stops the session: every call from every agent is denied until a review. Does only what
    /// can run inside the turn it ends: the stop state, its records and the signal. The surface's
    /// stop handler kills the agents and jobs. Returns false when a stop already stands.
    pub fn afk_stop(&self, value: f64, ceiling: f64, contributing: Vec<u64>) -> bool {
        let mut afk = write(&self.afk, "afk");
        if afk.stopped.is_some() {
            return false;
        }
        let seq = self.append_seq(Record::AfkStop {
            value,
            ceiling,
            contributing: contributing.clone(),
        });
        afk.stopped = Some(AfkStop {
            seq: seq.unwrap_or(0),
            value,
            ceiling,
            contributing: contributing.clone(),
        });
        drop(afk);
        let list = contributing
            .iter()
            .map(|seq| format!("#{seq}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.record_session_event_as(
            "afk",
            serde_json::json!({ "value": value, "ceiling": ceiling, "contributing": contributing }),
            Tier::Afk,
            "afk_stop",
            &format!(
                "session temperature {value:.1} reached the ceiling {ceiling:.1}; contributing denials: {list}"
            ),
            Decision::Deny,
        );
        self.afk_stops.send_modify(|stops| *stops += 1);
        true
    }

    /// The human reviewed a stopped session: it may run again, in `manual` when AFK mode was on
    /// (the human who just reviewed is present). Evidence is kept, and the denial breaker is
    /// untouched. Returns false when no stop stands.
    pub fn afk_review(&self) -> bool {
        let mut afk = write(&self.afk, "afk");
        let Some(stop) = afk.stopped.take() else {
            return false;
        };
        self.append(Record::AfkReviewed {});
        if *read(&self.mode, "mode") == PermissionMode::Afk {
            self.switch_mode(afk, PermissionMode::Manual, AfkSource::Command)
                .expect("leaving AFK mode is never refused");
        } else {
            drop(afk);
        }
        self.record_session_event(
            "afk",
            serde_json::json!({ "stop_seq": stop.seq }),
            Tier::Afk,
            "afk_review",
            "the AFK temperature stop was reviewed",
        );
        true
    }

    /// True once for each time the denial breaker blocks an AFK session, so a surface announces
    /// the outcome exactly once however often it asks.
    pub fn announce_blocked(&self) -> bool {
        if self.unattended_blocked() == Some(Blocked::BreakerPaused) {
            !self.afk_block_announced.swap(true, Ordering::Relaxed)
        } else {
            self.afk_block_announced.store(false, Ordering::Relaxed);
            false
        }
    }

    /// Whether `announce_blocked` would return true, without claiming the announcement.
    pub fn block_unannounced(&self) -> bool {
        self.unattended_blocked() == Some(Blocked::BreakerPaused)
            && !self.afk_block_announced.load(Ordering::Relaxed)
    }

    /// A receiver that sees each temperature stop as a change of its value.
    pub fn afk_stop_signal(&self) -> watch::Receiver<u64> {
        self.afk_stops.subscribe()
    }

    /// Why a turn nobody typed (a notification, a scheduled prompt, a goal step) may not start.
    pub fn unattended_blocked(&self) -> Option<Blocked> {
        let afk = read(&self.afk, "afk");
        if afk.stopped.is_some() {
            Some(Blocked::Stopped)
        } else if afk.stretch.is_some() && self.auto_paused() {
            Some(Blocked::BreakerPaused)
        } else {
            None
        }
    }

    /// The human trusted `skill` at `digest` in the terminal: recorded in the ledger and the
    /// audit log, as an approval is.
    pub fn record_skill_trust(&self, skill: &str, digest: &str) {
        self.append(Record::SkillTrust {
            name: skill.to_string(),
            digest: digest.to_string(),
        });
        self.record_session_event(
            "skill_trust",
            serde_json::json!({ "name": skill, "digest": digest }),
            Tier::Human,
            "skill_trust",
            &format!("skill {skill} trusted at {digest}"),
        );
    }

    /// The human submitted a bug report with `/bug`: recorded in the ledger and the audit log.
    pub fn record_bug_report(&self, method: &str, level: &str, url: &str) {
        self.append(Record::BugReport {
            method: method.to_string(),
            level: level.to_string(),
            url: url.to_string(),
        });
        self.record_session_event(
            "bug",
            serde_json::json!({ "method": method, "level": level, "url": url }),
            Tier::BugReport,
            "bug_report",
            &format!("bug report filed by {method} at {level} level"),
        );
    }

    /// A model list moved `agent` from `from` to `to` after the serving entry ran out of quota:
    /// recorded in the ledger and the audit log, so a resumed session and the log show the switch.
    pub fn record_model_fall_through(
        &self,
        agent: &str,
        role: Option<&str>,
        from: &str,
        to: &str,
        class: &str,
    ) {
        self.append(Record::ModelFellThrough {
            agent: agent.to_string(),
            role: role.map(str::to_string),
            from: from.to_string(),
            to: to.to_string(),
            class: class.to_string(),
        });
        self.record_session_event(
            "model_fell_through",
            serde_json::json!({ "agent": agent, "role": role, "from": from, "to": to, "class": class }),
            Tier::Mode,
            "model_fell_through",
            &format!("{agent} moved from {from} to {to} after {class}"),
        );
    }

    /// The session's usage pause: every provider call waits at it while it is in force.
    pub fn usage_pause(&self) -> &UsagePause {
        &self.usage_pause
    }

    /// Makes the pause's begin and end durable: ledger records, audit entries and telemetry. The
    /// session is shared, so the observer holds it weakly and the pause never keeps it alive.
    pub fn wire_usage_pause(self: &Arc<Self>) {
        let state = Arc::downgrade(self);
        self.usage_pause.observe(Arc::new(move |event| {
            if let Some(state) = state.upgrade() {
                state.record_usage_event(event);
            }
        }));
        let pause = self.usage_pause.clone();
        self.agents.set_pause_clock(move || pause.total_paused());
    }

    fn record_usage_event(&self, event: &PauseEvent) {
        let epoch = |at: Option<std::time::SystemTime>| {
            at.and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
        };
        match event {
            PauseEvent::Paused(info) => {
                let reset_at = epoch(info.reset_at);
                self.append(Record::UsagePaused {
                    role: info.role.as_str().to_string(),
                    account: info.account.clone(),
                    reset_at,
                });
                tracing::info!(
                    target: TARGET,
                    role = info.role.as_str(),
                    account = info.account.as_str(),
                    reset_at = reset_at,
                    "usage_paused"
                );
                self.record_session_event(
                    "usage_paused",
                    serde_json::json!({
                        "role": info.role.as_str(),
                        "account": info.account,
                        "reset_at": reset_at,
                    }),
                    Tier::Mode,
                    "usage_paused",
                    &format!("the {} usage window is spent", info.account),
                );
            }
            PauseEvent::Resumed { end, info, waited } => {
                self.append(Record::UsageResumed { end: *end });
                tracing::info!(
                    target: TARGET,
                    role = info.role.as_str(),
                    account = info.account.as_str(),
                    end = end.as_str(),
                    waited_secs = waited.as_secs(),
                    "usage_resumed"
                );
                self.record_session_event(
                    "usage_resumed",
                    serde_json::json!({
                        "role": info.role.as_str(),
                        "account": info.account,
                        "end": end.as_str(),
                    }),
                    Tier::Mode,
                    "usage_resumed",
                    &format!("the {} usage pause ended: {}", info.account, end.as_str()),
                );
            }
        }
    }

    pub fn set_guardian_quota(&self, source: Option<Arc<dyn crate::quota::QuotaSource>>) {
        *write(&self.guardian_quota, "guardian quota") = source;
    }

    pub fn guardian_quota(&self) -> Option<Arc<dyn crate::quota::QuotaSource>> {
        read(&self.guardian_quota, "guardian quota").clone()
    }

    /// How the surface wants usage pauses bounded.
    pub fn set_usage_wait(&self, wait: UsageWait) {
        *write(&self.usage_wait, "usage wait") = wait;
    }

    pub fn usage_wait(&self) -> UsageWait {
        *read(&self.usage_wait, "usage wait")
    }

    /// The longest a pause started now may last: none at all for `exit`, the cap otherwise, and
    /// the AFK cap while AFK is on because nobody can cancel then.
    fn usage_deadline(&self) -> Option<std::time::Duration> {
        let wait = self.usage_wait();
        if wait.exit {
            return Some(std::time::Duration::ZERO);
        }
        wait.cap.or_else(|| wait.afk_cap.filter(|_| self.afk_on()))
    }

    /// Holds the whole run because `account`'s window is spent, and waits for it to end. `Err`
    /// when the wait was cancelled or ran out, which the caller reports as its own failure.
    pub async fn pause_for_usage(
        &self,
        role: crate::quota::UsageRole,
        account: &str,
        limit: &UsageLimit,
        source: Option<Arc<dyn crate::quota::QuotaSource>>,
    ) -> Result<(), UsagePauseEnded> {
        let info = PauseInfo::new(role, account, limit.reset_at)
            .with_deadline(self.usage_deadline())
            .with_source(source);
        if self.usage_pause.engage(info.clone()) {
            self.offer_usage_choice(info);
        }
        self.usage_gate().await
    }

    /// Asks the human what to do about the pause, unless nobody can answer. A task of the
    /// session's own, so a caller that is dropped does not leave the pause without its prompt;
    /// it ends with the pause.
    fn offer_usage_choice(&self, info: PauseInfo) {
        if self.afk_on() {
            return;
        }
        let (approver, pause) = (self.approver.clone(), self.usage_pause.clone());
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = pause.closed() => {}
                choice = approver.usage_paused(info) => match choice {
                    PauseChoice::Resume => pause.resume(),
                    PauseChoice::Cancel => pause.cancel(),
                    PauseChoice::Wait => {}
                },
            }
        });
    }

    /// The gate every provider call waits at.
    pub async fn usage_gate(&self) -> Result<(), UsagePauseEnded> {
        match self.usage_pause.wait_open().await {
            Some(end) if end.is_failure() => Err(self.ended(end)),
            _ => Ok(()),
        }
    }

    fn ended(&self, end: PauseEnd) -> UsagePauseEnded {
        let info = self
            .usage_pause
            .failure()
            .map(|(_, info)| info)
            .unwrap_or_else(|| PauseInfo::new(crate::quota::UsageRole::Main, "the", None));
        UsagePauseEnded { end, info }
    }

    /// An audit entry for something the human did to the session rather than a tool call.
    fn record_session_event(
        &self,
        tool: &str,
        args: serde_json::Value,
        tier: Tier,
        rule: &str,
        explanation: &str,
    ) {
        self.record_session_event_as(tool, args, tier, rule, explanation, Decision::Allow);
    }

    fn record_session_event_as(
        &self,
        tool: &str,
        args: serde_json::Value,
        tier: Tier,
        rule: &str,
        explanation: &str,
        decision: Decision,
    ) {
        let mut record = DecisionRecord::new(
            self.session_id.clone(),
            ROOT_AGENT.to_string(),
            tool.to_string(),
            args_digest(&args),
            tier,
            decision,
            self.mode().rules().as_str().to_string(),
            false,
        );
        record.afk = self.afk_on();
        record.rule = Some(rule.to_string());
        record.explanation = Some(explanation.to_string());
        crate::permissions::record_decision(self, &record);
    }

    /// Records a plugin install, update, removal, enable, disable or reload, in the ledger and
    /// the audit log. The human typed the command; this is the evidence that it ran.
    pub fn record_plugin_change(&self, op: &str, plugin: &str, fingerprint: &str) {
        self.append(Record::PluginChange {
            op: op.to_string(),
            plugin: plugin.to_string(),
            fingerprint: fingerprint.to_string(),
        });
        self.record_session_event(
            "plugin_change",
            serde_json::json!({ "op": op, "plugin": plugin, "fingerprint": fingerprint }),
            Tier::Human,
            "plugin_change",
            &format!("plugin {op}: {plugin}"),
        );
    }

    pub fn guardian(&self) -> Arc<Guardian> {
        read(&self.guardian, "guardian").clone()
    }

    /// The composition that replaces a bare guardian call, when one is configured.
    pub fn composition(&self) -> Option<CompositionStage> {
        read(&self.composition, "composition").clone()
    }

    pub fn set_composition(&self, stage: Option<CompositionStage>) {
        *write(&self.composition, "composition") = stage;
    }

    /// True the first time it is asked, so a broken composition is reported once per session.
    pub fn first_composition_warning(&self) -> bool {
        !self.composition_warned.swap(true, Ordering::Relaxed)
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

    /// Records one model call: in the session's usage ledger under `source` and against
    /// `agent`'s budget. `sent_request` is false for a record that stands for no request.
    /// `served` names the model that made the call and the role it was asked for under.
    pub fn record_usage(
        &self,
        source: UsageSource,
        agent: &str,
        usage: &Usage,
        sent_request: bool,
        served: &Served,
    ) {
        self.usage.record(source, usage, sent_request, served);
        self.agents.debit(agent, &spend_of(usage));
    }

    /// The session's usage ledger.
    pub fn usage_ledger(&self) -> &UsageLedger {
        &self.usage
    }

    /// Counts a guardian call made for `agent`.
    pub fn add_guardian_usage(
        &self,
        agent: &str,
        usage: &Usage,
        sent_request: bool,
        served: &Served,
    ) {
        self.record_usage(UsageSource::Guardian, agent, usage, sent_request, served);
    }

    /// What the guardian has cost across the whole tree.
    pub fn guardian_usage(&self) -> Bucket {
        self.usage.bucket(UsageSource::Guardian)
    }

    /// Counts a goal evaluation's model calls against the root, so the spend a goal's cost cap
    /// reads includes them.
    pub fn add_goal_usage(&self, usage: &Usage, served: &Served) {
        self.record_usage(UsageSource::Goal, ROOT_AGENT, usage, true, served);
    }

    /// What goal evaluations have cost this session, which outlives the goal.
    pub fn goal_usage(&self) -> Bucket {
        self.usage.bucket(UsageSource::Goal)
    }

    /// What subagents' model calls have cost.
    pub fn subagent_usage(&self) -> Bucket {
        self.usage.bucket(UsageSource::Subagent)
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

    /// Tells the guardian, for `agent` until the turn ends, that a skill the user trusted is
    /// active and declares wildcard shell rules. Context only: it allows nothing.
    pub fn note_trusted_skill(&self, agent: &str, note: TrustedSkillNote) {
        let mut notes = lock(&self.skill_notes, "skill notes");
        let held = notes.entry(agent.to_string()).or_default();
        if !held.contains(&note) {
            held.push(note);
        }
    }

    pub fn trusted_skill_notes(&self, agent: &str) -> Vec<TrustedSkillNote> {
        lock(&self.skill_notes, "skill notes")
            .get(agent)
            .cloned()
            .unwrap_or_default()
    }

    /// Tells the human, once per skill, that `skill` declares `allowed-tools` the session does not
    /// honour because the user has not trusted this version of it.
    pub fn warn_skill_not_trusted(&self, skill: &str, stale: bool) {
        if !lock(&self.skill_warned, "skill warning").insert(skill.to_string()) {
            return;
        }
        let why = if stale {
            format!(
                "changed since you trusted it; run `/skills trust {skill}` or `fontaine plugin trust {skill}` to honour its allowed-tools again"
            )
        } else {
            format!("is not trusted; `fontaine plugin trust {skill}` makes its allowed-tools count")
        };
        self.approver
            .warn(&format!("skill {skill} declares allowed-tools but {why}"));
    }

    /// A user turn ended: the rules skills lent for it end too.
    pub fn end_turn(&self) {
        lock(&self.skill_grants, "skill grants").clear();
        lock(&self.skill_notes, "skill notes").clear();
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
        self.append_seq(record);
    }

    /// Appends `record` and returns its ledger position, or `None` when it could not be written.
    /// A denial while AFK is on can push the session temperature to its ceiling, which stops the
    /// session.
    fn append_seq(&self, record: Record) -> Option<u64> {
        let denial = matches!(record, Record::Denial { .. });
        let seq = match self.ledger.append(record) {
            Ok(seq) => Some(seq),
            Err(e) => {
                self.sink_failed(Sink::Ledger, &e);
                None
            }
        };
        if denial && seq.is_some() {
            self.stop_if_too_hot();
        }
        seq
    }

    /// Stops the session when the stretch's temperature has reached the ceiling.
    fn stop_if_too_hot(&self) {
        if !self.afk_on() {
            return;
        }
        let ceiling = self.afk_ceiling();
        let reached = {
            let ledger = self.ledger.view();
            let temperature = ledger.afk().temperature();
            (temperature.current() >= ceiling).then(|| {
                let contributing: Vec<u64> = temperature
                    .contributions()
                    .iter()
                    .filter(|c| temperature.remaining(c) >= 0.1)
                    .map(|c| c.seq)
                    .collect();
                (temperature.current(), contributing)
            })
        };
        if let Some((value, contributing)) = reached {
            self.afk_stop(value, ceiling, contributing);
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
        *lock(&self.tool_calls, "tool call counts")
            .entry(agent.to_string())
            .or_default() += 1;
    }

    /// The number of tool calls `agent` has started.
    pub fn tool_calls_of(&self, agent: &str) -> u64 {
        lock(&self.tool_calls, "tool call counts")
            .get(agent)
            .copied()
            .unwrap_or(0)
    }

    /// Queues a harness note for the next root prompt.
    pub fn queue_hook_context(&self, note: String) {
        lock(&self.hook_context, "hook context").push(note);
    }

    /// Sets the note a `user_prompt_submit` hook added to the prompt being gated, replacing any
    /// left by an earlier gate. Kept apart from the session-start queue so a command that ends
    /// without a turn can drop it without dropping those.
    pub fn set_prompt_context(&self, note: Option<String>) {
        *lock(&self.prompt_context, "prompt context") = note.into_iter().collect();
    }

    /// Drops the prompt-gate note: the gated command ended without running its turn.
    pub fn clear_prompt_context(&self) {
        lock(&self.prompt_context, "prompt context").clear();
    }

    /// The queued notes, emptying the queue.
    pub fn take_hook_context(&self) -> Vec<String> {
        let mut notes = std::mem::take(&mut *lock(&self.hook_context, "hook context"));
        notes.extend(std::mem::take(&mut *lock(
            &self.prompt_context,
            "prompt context",
        )));
        notes
    }

    /// Drops the queued notes: the conversation they were for is gone.
    pub fn clear_hook_context(&self) {
        lock(&self.hook_context, "hook context").clear();
        self.clear_prompt_context();
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

    fn state_in(dir: &std::path::Path, id: &str) -> Arc<SessionState> {
        let agents = Arc::new(AgentTree::new(Limits::default(), 1));
        agents.set_root_tools(|| {
            fontaine_tools::descriptor::names()
                .map(String::from)
                .collect()
        });
        Arc::new(SessionState::new(StateParams {
            session_id: id.into(),
            mode: PermissionMode::Auto,
            scope: Arc::new(Scope::new(dir.to_path_buf(), [])),
            deny: Vec::new(),
            ask: Vec::new(),
            allow: Vec::new(),
            ledger: Arc::new(Ledger::create(dir).unwrap()),
            audit: Arc::new(AuditLog::create(dir, id).unwrap()),
            approver: Arc::new(DenyAll),
            home: None,
            protected_dirs: Vec::new(),
            scratch: None,
            memory_root: None,
            guardian: no_guardian(),
            auto: AutoContext::default(),
            agents: agents.clone(),
            agent_types: Arc::new(Live::new(AgentTypes::builtin())),
            reveal_mcp_names: false,
            auto_edits: AutoEdits::default(),
            effort: None,
        }))
    }

    #[test]
    #[should_panic(expected = "the loop breaker lock is not poisoned")]
    fn a_poisoned_breaker_panics_instead_of_deciding() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(dir.path(), "poison-test");
        let holder = state.clone();
        let _ = std::thread::spawn(move || {
            let _held = holder.breaker.lock().unwrap();
            panic!("poison the breaker");
        })
        .join();
        state.auto_paused();
    }

    #[test]
    fn a_filed_bug_report_reaches_the_ledger_and_the_audit_log() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(dir.path(), "bug-test");
        state.record_bug_report("gh", "minimal", "https://github.com/o/r/issues/1");
        let entries = state.ledger.read().unwrap();
        assert!(entries.iter().any(|e| matches!(
            &e.record,
            Record::BugReport { method, level, url }
                if method == "gh" && level == "minimal" && url.ends_with("/issues/1")
        )));
        let audit = state.audit.read().unwrap();
        let record = audit.iter().find(|r| r.tool == "bug").unwrap();
        assert_eq!(record.tier, Tier::BugReport);
        assert_eq!(record.decision, Decision::Allow);
        assert_eq!(record.rule.as_deref(), Some("bug_report"));
    }
}
