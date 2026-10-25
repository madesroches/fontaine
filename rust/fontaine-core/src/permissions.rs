//! The permission pipeline: the one place that decides whether a tool call runs. Every wrapped
//! tool reaches it through the `ToolGate` trait. The order is the one in the design: user
//! pre-tool-use hooks, the deterministic floor (deny rules, ask rules, critical paths, protected
//! paths) in every mode, then the read-only fast path, the shell pre-classifier, allow rules and
//! finally what the mode does with a call nobody pre-approved.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fontaine_guardian::cascade::{Decided, ScreenInput};
use fontaine_guardian::{
    Adjudication, Decision as GuardianDecision, FailClosed, ScreenOutcome, ScreenScores, WireIds,
};
use fontaine_tools::ask_user::{Question, QuestionReply};
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::{CallView, GateDecision, ToolGate};
use fontaine_tools::preclassify::{Environment, Preclassifier, analyze};
use fontaine_tools::risk::RiskClass;
use fontaine_tools::scope::Scope;
use futures::future::BoxFuture;
use serde_json::Value;
use tracing::Instrument;

use crate::adjudication::{self, action_summary};
use crate::audit::{Decision, DecisionRecord, args_digest, now_rfc3339};
use crate::config::PermissionMode;
use crate::destructive::touched_paths;
use crate::floor::{self, CriticalEnv};
use crate::gate::{DeniedCall, RetrySource, ScreenStage, SessionState, permission_mode};
use crate::ledger::Record;
use crate::permission_rules::{
    CallContext, Posture, RuleSource, SourcedRule, is_broad_code_execution, matches, parse_rule,
    propose,
};
use crate::runtime_error::RuntimeError;
use crate::tier::Tier;
use crate::user_config;

const TARGET: &str = "fontaine::telemetry";

/// What the human is asked at an approval prompt.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub tool: String,
    pub agent: String,
    pub tool_use_id: Option<String>,
    pub args: Value,
    /// The call: the full command, path or URL.
    pub summary: String,
    /// Which part of the pipeline asked.
    pub tier: Tier,
    /// Why the call needs approval.
    pub reason: String,
    pub offers: Offers,
    /// The exact rule "allow session" and "always allow" would add.
    pub proposed_rule: Option<String>,
}

/// Which answers beyond "allow once" and "deny" the prompt offers. Prompts forced by the floor
/// and shell calls that are not one literal simple command offer neither, because no allow rule
/// can act there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offers {
    pub session: bool,
    pub always: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    AllowSession,
    AllowAlways,
    Deny { feedback: Option<String> },
}

/// The human-approval callback.
pub trait Approver: Send + Sync {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer>;

    /// Something the human should know that is not an answer to a prompt, such as a settings
    /// file that could not be written.
    fn notice(&self, _message: &str) {}

    /// A warning about the session itself. Defaults to stderr; a full-screen frontend must
    /// override it, since a stray line corrupts the screen.
    fn warn(&self, message: &str) {
        eprintln!("fontaine: warning: {message}");
    }

    /// The pinned guardian cannot be used at all (no credential, unknown account). Nothing is
    /// substituted silently: the human chooses whether to try again, to run with `manual`
    /// instead, or to leave the call denied.
    fn guardian_unavailable<'a>(
        &'a self,
        _info: GuardianUnavailable,
    ) -> BoxFuture<'a, UnavailableChoice> {
        Box::pin(async { UnavailableChoice::Deny })
    }

    /// The model's multiple-choice questions for the human. Only a frontend with a dialog
    /// answers; every other approver reports that nobody can.
    fn ask_user<'a>(
        &'a self,
        _agent: &'a str,
        _questions: Vec<Question>,
    ) -> BoxFuture<'a, QuestionReply> {
        Box::pin(async { QuestionReply::Unavailable })
    }
}

/// Why the guardian could not be asked, for the human who has to decide what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardianUnavailable {
    /// `<account>/<model>`.
    pub model: String,
    pub reason: String,
    /// The call that needed it.
    pub tool: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableChoice {
    /// Ask the guardian again; the surface has re-resolved or replaced it.
    Retry,
    /// Switch the session to `manual` and prompt for this call.
    Manual,
    /// Leave the call denied.
    Deny,
}

/// An approver that denies everything, for runs where nobody can answer.
pub struct DenyAll;

impl Approver for DenyAll {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    /// With no interface to show it on, a notice goes to stderr.
    fn notice(&self, message: &str) {
        eprintln!("fontaine: {message}");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookPermission {
    Deny,
    Ask,
    Allow,
}

/// What one `pre_tool_use` hook answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookDecision {
    pub permission: Option<HookPermission>,
    /// Replaces the call's arguments; every later step judges the rewritten call.
    pub updated_input: Option<Value>,
    pub reason: Option<String>,
    /// A note for the guardian, shown to it inside an untrusted block.
    pub guardian_context: Option<String>,
}

#[derive(Debug)]
pub enum HookError {
    Failed {
        context: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    Malformed {
        context: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl HookError {
    pub fn failed(context: impl Into<String>) -> Self {
        HookError::Failed {
            context: context.into(),
            source: None,
        }
    }

    pub fn failed_by(
        context: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        HookError::Failed {
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn malformed(context: impl Into<String>) -> Self {
        HookError::Malformed {
            context: context.into(),
            source: None,
        }
    }

    pub fn malformed_by(
        context: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        HookError::Malformed {
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookError::Failed { context, .. } | HookError::Malformed { context, .. } => {
                f.write_str(context)
            }
        }
    }
}

impl std::error::Error for HookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HookError::Failed { source, .. } | HookError::Malformed { source, .. } => source
                .as_deref()
                .map(|e| e as &(dyn std::error::Error + 'static)),
        }
    }
}

pub struct PreToolUseEvent<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub agent: &'a str,
    pub tool_use_id: Option<&'a str>,
    pub session_id: &'a str,
    /// The primary workspace root.
    pub cwd: &'a Path,
    /// The agent's permission mode in force for this call, by its settings name.
    pub permission_mode: &'a str,
    pub workspace_roots: &'a [String],
}

/// The slot for user `pre_tool_use` hooks. A hook that errors, times out or answers something
/// unreadable contributes no decision; it never loosens anything.
pub trait PreToolUseHook: Send + Sync {
    fn id(&self) -> String;

    /// The matcher the hook is configured with, in the permission-rule grammar. A hook `allow`
    /// skips the guardian in `auto` only when this is a narrow rule; a hook that names none, or
    /// a broad one, is suspended there like a broad allow rule.
    fn matcher(&self) -> Option<String> {
        None
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Whether this hook's `allow` counts. A hook the repository defines can tighten a call
    /// but never approve one.
    fn can_allow(&self) -> bool {
        true
    }

    fn run<'a>(
        &'a self,
        event: PreToolUseEvent<'a>,
    ) -> BoxFuture<'a, Result<HookDecision, HookError>>;
}

/// What the pipeline tells a `permission_denied` hook.
pub struct PermissionDeniedEvent<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub agent: &'a str,
    pub tool_use_id: Option<&'a str>,
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub permission_mode: &'a str,
    pub workspace_roots: &'a [String],
    pub tier: Tier,
    pub reason: &'a str,
}

/// The slot for user `permission_denied` hooks. The answer can only say the agent may retry
/// the call; the floor and the guardian are not consulted differently because of it.
pub trait PermissionDeniedHook: Send + Sync {
    fn id(&self) -> String;

    fn matcher(&self) -> Option<String> {
        None
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Whether its `retry` counts. A hook the repository defines never loosens a decision.
    fn can_retry(&self) -> bool {
        true
    }

    /// `Ok(true)` when the hook says the agent may retry exactly this call.
    fn run<'a>(
        &'a self,
        event: PermissionDeniedEvent<'a>,
    ) -> BoxFuture<'a, Result<bool, HookError>>;
}

pub struct PipelineParams {
    pub state: Arc<SessionState>,
    pub approver: Arc<dyn Approver>,
    /// Nothing can be asked: every would-prompt outcome becomes a denial.
    pub headless: bool,
    pub hooks: Vec<Arc<dyn PreToolUseHook>>,
    pub denied_hooks: Vec<Arc<dyn PermissionDeniedHook>>,
    /// `[shell_preclassifier] allow_extra`.
    pub allow_extra: Vec<String>,
    /// The user settings file "always allow" appends to.
    pub user_config: Option<PathBuf>,
}

pub struct PermissionPipeline {
    state: Arc<SessionState>,
    approver: Arc<dyn Approver>,
    headless: bool,
    hooks: Vec<Arc<dyn PreToolUseHook>>,
    denied_hooks: Vec<Arc<dyn PermissionDeniedHook>>,
    preclassifier: Preclassifier,
    user_config: Option<PathBuf>,
}

#[derive(Default)]
struct HookOutcome {
    deny: Option<(String, Option<String>)>,
    ask: Option<String>,
    /// The hook id, and whether its matcher is narrow enough to act in `auto`.
    allow: Option<(String, bool)>,
    updated_input: Option<Value>,
    guardian_context: Vec<String>,
}

/// One call as the pipeline carries it from the first hook to the last record.
struct CallCtx<'a> {
    call: &'a CallView<'a>,
    /// The arguments every step judges: the model's, or a hook's rewrite of them.
    args: Value,
    tool_use_id: Option<String>,
    digest: String,
    original_digest: Option<String>,
    mode: PermissionMode,
    started: Instant,
}

/// What the screen made of a call, for the audit record of whatever decides it.
struct Screened {
    outcome: ScreenOutcome,
    model: String,
    dropped: Option<Dropped>,
}

/// The guardian request a settling screen cut short.
struct Dropped {
    input_tokens: u64,
    provider: String,
    model: String,
    priced: bool,
}

impl Screened {
    fn annotate(&self, record: &mut DecisionRecord) {
        record.screen_risk = self.outcome.risk();
        record.screen_allow_risk = self.outcome.allow_risk();
        record.screen_steering = self.outcome.steering();
        record.screen_model = Some(self.model.clone());
        record.screen_latency_ms = Some(self.outcome.latency_ms());
        record.screen_outcome = Some(self.outcome.as_str().to_string());
        if let Some(dropped) = &self.dropped {
            record.guardian_dropped_input_tokens = Some(dropped.input_tokens);
            record.guardian_dropped_provider = Some(dropped.provider.clone());
            record.guardian_dropped_model = Some(dropped.model.clone());
            record.guardian_dropped_priced = Some(dropped.priced);
        }
    }
}

/// A call that needs the human, or is denied because there is none.
struct Prompt {
    tier: Tier,
    rule: Option<String>,
    reason: String,
    /// The floor forced it: no allow rule can act on it.
    forced: bool,
}

enum Verdict {
    Allow {
        tier: Tier,
        rule: Option<String>,
    },
    Deny {
        tier: Tier,
        rule: Option<String>,
        why: String,
    },
    Prompt(Prompt),
    /// Nothing before the mode pre-approved or refused the call: the guardian decides.
    Guardian,
}

fn denied(tool: &str, why: &str) -> String {
    format!(
        "`{tool}` was denied: {why}. Do not retry it or look for another route to the same \
         effect; tell the user what you could not do."
    )
}

/// The file-tool calls a read-only shell command stands for: every path the pre-classifier
/// validated as a `read` of it, and as the matching `ls`/`grep` when the
/// command is one. Words that are not paths only cause an extra look at the rules.
fn ruled_hit<'a>(
    list: &'a [SourcedRule],
    reads: &[(&'static str, Value)],
    scope: &Scope,
    home: Option<&std::path::Path>,
    posture: Posture,
) -> Option<&'a SourcedRule> {
    list.iter().find(|r| {
        reads.iter().any(|(tool, args)| {
            let ctx = CallContext {
                tool,
                args,
                scope,
                home,
            };
            matches(&r.rule, &ctx, posture)
        })
    })
}

fn ruled_read_calls(
    command: &str,
    paths: &[String],
    cwd: Option<&str>,
    scope: &Scope,
) -> Vec<(&'static str, Value)> {
    let Some(argv) = analyze(command).simple else {
        return Vec::new();
    };
    let base = match cwd {
        Some(cwd) => scope.resolved(cwd),
        None => scope.primary().to_path_buf(),
    };
    let tools: &[&'static str] = match argv[0].rsplit(['/', '\\']).next().unwrap_or("") {
        "ls" => &["read", "ls"],
        "grep" | "rg" => &["read", "grep"],
        _ => &["read"],
    };
    let mut calls = Vec::new();
    for candidate in paths {
        let path = base.join(candidate).to_string_lossy().into_owned();
        for tool in tools {
            calls.push((*tool, serde_json::json!({ "path": path })));
        }
    }
    calls
}

impl PermissionPipeline {
    pub fn new(params: PipelineParams) -> PermissionPipeline {
        PermissionPipeline {
            preclassifier: Preclassifier::new(&params.allow_extra),
            state: params.state,
            approver: params.approver,
            headless: params.headless,
            hooks: params.hooks,
            denied_hooks: params.denied_hooks,
            user_config: params.user_config,
        }
    }

    pub fn state(&self) -> &Arc<SessionState> {
        &self.state
    }

    /// The mode this agent's calls are judged under right now, and whether edits are blocked
    /// for it. The root's is the session mode; a child's is the tighter of its own and its
    /// parent's, so a child is never looser than its parent and follows it on every call.
    fn posture_of(&self, agent: &str) -> (PermissionMode, bool) {
        let tree = self.state.agents();
        if agent == fontaine_tools::gate::ROOT_AGENT || !tree.is_child(agent) {
            return (self.state.mode(), false);
        }
        let posture = tree.posture(agent);
        (permission_mode(posture.mode), posture.plan_block)
    }

    fn workspace_view(&self) -> (PathBuf, Vec<String>) {
        let scope = self.state.scope();
        (
            scope.primary().to_path_buf(),
            scope
                .roots()
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
        )
    }

    async fn run_hooks(
        &self,
        call: &CallView<'_>,
        tool_use_id: Option<&str>,
        mode: PermissionMode,
    ) -> HookOutcome {
        let mut outcome = HookOutcome::default();
        if self.hooks.is_empty() {
            return outcome;
        }
        let mut args = call.args.clone();
        let scope = self.state.scope();
        let home = self.state.home.as_deref();
        let (cwd, roots) = self.workspace_view();
        for hook in &self.hooks {
            let rule = hook.matcher().and_then(|text| parse_rule(&text).ok());
            // A hook that names a matcher runs for the calls it covers; one that cannot be read
            // runs for every call, since a hook can only tighten by running.
            let covers = |args: &Value, posture: Posture| match &rule {
                Some(rule) => matches(
                    rule,
                    &CallContext {
                        tool: call.tool,
                        args,
                        scope: &scope,
                        home,
                    },
                    posture,
                ),
                None => true,
            };
            if !covers(&args, Posture::Deny) {
                continue;
            }
            let event = PreToolUseEvent {
                tool: call.tool,
                args: &args,
                agent: call.agent,
                tool_use_id,
                session_id: &self.state.session_id,
                cwd: &cwd,
                permission_mode: mode.as_str(),
                workspace_roots: &roots,
            };
            let id = hook.id();
            let decision = match tokio::time::timeout(hook.timeout(), hook.run(event)).await {
                Ok(Ok(decision)) => decision,
                Ok(Err(e)) => {
                    tracing::warn!(hook = %id, error = ?e, "a pre_tool_use hook failed; it decides nothing");
                    continue;
                }
                Err(_) => {
                    tracing::warn!(hook = %id, "a pre_tool_use hook timed out; it decides nothing");
                    continue;
                }
            };
            if let Some(updated) = decision.updated_input {
                if updated.is_object() {
                    args = updated.clone();
                    // An allow vouches for the arguments it saw, not for a rewrite.
                    outcome.allow = None;
                    outcome.updated_input = Some(updated);
                } else {
                    tracing::warn!(hook = %id, "a pre_tool_use hook returned input that is not an object; ignored");
                }
            }
            match decision.permission {
                Some(HookPermission::Deny) => {
                    outcome.deny.get_or_insert((id, decision.reason));
                }
                Some(HookPermission::Ask) => {
                    outcome.ask.get_or_insert(id);
                }
                Some(HookPermission::Allow) if !hook.can_allow() => {
                    tracing::debug!(hook = %id, "a project hook's allow is ignored");
                }
                // Like an allow rule, it acts only where its matcher would match as an allow rule
                // does: exactly one simple command, not a chain.
                Some(HookPermission::Allow) if covers(&args, Posture::Allow) => {
                    let narrow = rule
                        .as_ref()
                        .is_some_and(|rule| !is_broad_code_execution(rule));
                    outcome.allow.get_or_insert((id, narrow));
                }
                Some(HookPermission::Allow) => {}
                None => {}
            }
            if let Some(note) = decision.guardian_context {
                outcome.guardian_context.push(note);
            }
        }
        outcome
    }

    /// The verdict for one call, with no side effects.
    fn evaluate(
        &self,
        call: &CallView<'_>,
        args: &Value,
        hook: &HookOutcome,
        mode: PermissionMode,
        plan_block: bool,
    ) -> Verdict {
        let scope = self.state.scope();
        let rules = self.state.rules();
        let home = self.state.home.as_deref();
        let ctx = CallContext {
            tool: call.tool,
            args,
            scope: &scope,
            home,
        };
        let tool = call.tool;
        let edit_class = call.risk == RiskClass::WorkspaceWrite;

        if let Some((id, reason)) = &hook.deny {
            let why = match reason {
                Some(reason) => format!("the pre_tool_use hook `{id}` forbids it ({reason})"),
                None => format!("the pre_tool_use hook `{id}` forbids it"),
            };
            return Verdict::Deny {
                tier: Tier::Hook,
                rule: Some(id.clone()),
                why,
            };
        }
        if let Some(rule) = rules
            .deny
            .iter()
            .find(|r| matches(&r.rule, &ctx, Posture::Deny))
        {
            return Verdict::Deny {
                tier: Tier::DenyRule,
                rule: Some(rule.label()),
                why: format!("the permission rule `{}` forbids it", rule.label()),
            };
        }
        if let Some(why) = self.state.child_restriction(call.agent, &ctx) {
            return Verdict::Deny {
                tier: Tier::DenyRule,
                rule: Some("agent tools".to_string()),
                why,
            };
        }
        if (mode == PermissionMode::Plan || plan_block) && edit_class {
            return Verdict::Deny {
                tier: Tier::PlanBlock,
                rule: None,
                why: "plan mode never edits files; leave plan mode to make changes".to_string(),
            };
        }

        let forced_ask = rules
            .ask
            .iter()
            .find(|r| matches(&r.rule, &ctx, Posture::Ask))
            .map(|r| (Tier::AskRule, r.label()))
            .or_else(|| hook.ask.clone().map(|id| (Tier::Hook, id)));
        if let Some((tier, rule)) = forced_ask {
            return self.would_prompt(
                mode,
                Prompt {
                    tier,
                    reason: format!("`{rule}` asks for approval before this runs"),
                    rule: Some(rule),
                    forced: true,
                },
            );
        }
        if tool == "shell"
            && let Some(command) = args.get("command").and_then(Value::as_str)
        {
            let env = CriticalEnv {
                scope: &scope,
                home,
            };
            let cwd = args.get("cwd").and_then(Value::as_str);
            if let Some(found) = floor::critical_shell(command, cwd, &env) {
                return self.would_prompt(
                    mode,
                    Prompt {
                        tier: Tier::CriticalPath,
                        reason: format!("it deletes a critical path ({found})"),
                        rule: Some(found),
                        forced: true,
                    },
                );
            }
        }
        if matches!(tool, "edit" | "write")
            && let Some(found) =
                floor::protected_path(&scope, home, &self.state.protected_dirs, args)
        {
            // In `auto` the guardian takes the place of the forced prompt, but only for a path
            // the file tool can reach at all; the others are refused below.
            if mode == PermissionMode::Auto && scope.locate(tool, args).is_ok() {
                return Verdict::Guardian;
            }
            return self.would_prompt(
                mode,
                Prompt {
                    tier: Tier::ProtectedPath,
                    reason: format!("it writes a protected location ({found})"),
                    rule: Some(found),
                    forced: true,
                },
            );
        }

        let located = scope.locate(tool, args);
        if call.risk == RiskClass::ReadOnly && (located.is_ok() || names_no_path(tool)) {
            return Verdict::Allow {
                tier: Tier::ReadOnlyFastPath,
                rule: None,
            };
        }
        if tool == "shell"
            && let Some(command) = args.get("command").and_then(Value::as_str)
        {
            let env = Environment {
                scope: &scope,
                cwd: args.get("cwd").and_then(Value::as_str),
            };
            if let Some(paths) = self.preclassifier.read_paths(command, &env) {
                // The read-only commands read the paths they are given, so the file-read rules
                // apply to those paths as they would to the read tool.
                let reads = ruled_read_calls(command, &paths, env.cwd, &scope);
                if let Some(rule) = ruled_hit(&rules.deny, &reads, &scope, home, Posture::Deny) {
                    return Verdict::Deny {
                        tier: Tier::DenyRule,
                        rule: Some(rule.label()),
                        why: format!("the permission rule `{}` forbids it", rule.label()),
                    };
                }
                if let Some(rule) = ruled_hit(&rules.ask, &reads, &scope, home, Posture::Ask) {
                    let rule = rule.label();
                    return self.would_prompt(
                        mode,
                        Prompt {
                            tier: Tier::AskRule,
                            reason: format!("`{rule}` asks for approval before this runs"),
                            rule: Some(rule),
                            forced: true,
                        },
                    );
                }
                return Verdict::Allow {
                    tier: Tier::Tier05,
                    rule: None,
                };
            }
        }
        // `auto` sets broad code-execution rules aside for as long as it is on: a blanket
        // `shell` or `python *` grant would otherwise run anything past the guardian.
        let suspended = |rule: &SourcedRule| {
            mode == PermissionMode::Auto && is_broad_code_execution(&rule.rule)
        };
        let skill_grants = self.state.skill_grants(call.agent);
        if let Some(rule) = rules
            .allow
            .iter()
            .chain(skill_grants.iter())
            .find(|r| !suspended(r) && matches(&r.rule, &ctx, Posture::Allow))
        {
            return Verdict::Allow {
                tier: Tier::AllowRule,
                rule: Some(rule.label()),
            };
        }
        if let Some((id, narrow)) = &hook.allow
            && (mode != PermissionMode::Auto || *narrow)
        {
            return Verdict::Allow {
                tier: Tier::Hook,
                rule: Some(id.clone()),
            };
        }

        let outside = match &located {
            // Model-facing text; the scope error has no source.
            Err(e) if call.risk != RiskClass::Arbitrary => Some(format!(
                "{e}; file tools work inside the workspace roots only (add one with /add-dir)"
            )),
            _ => None,
        };
        // The tool refuses an out-of-scope path whatever the answer, so asking would only offer
        // grants that cannot take effect.
        if let Some(why) = outside
            && mode != PermissionMode::DontAsk
        {
            return Verdict::Deny {
                tier: Tier::Mode,
                rule: None,
                why,
            };
        }
        let needs_approval = || Prompt {
            tier: Tier::Mode,
            rule: None,
            reason: "it needs the user's approval".to_string(),
            forced: false,
        };
        match mode {
            PermissionMode::DontAsk => Verdict::Deny {
                tier: Tier::DontAsk,
                rule: None,
                why: "dontAsk mode only runs calls that are pre-approved".to_string(),
            },
            PermissionMode::Manual => self.would_prompt(mode, needs_approval()),
            // Plan mode never edits (refused above); what is left is consequential, and the
            // guardian judges it as it does in `auto`.
            PermissionMode::Plan | PermissionMode::Auto => Verdict::Guardian,
            PermissionMode::AcceptEdits => {
                if edit_class && located.is_ok() {
                    Verdict::Allow {
                        tier: Tier::Mode,
                        rule: None,
                    }
                } else {
                    self.would_prompt(mode, needs_approval())
                }
            }
        }
    }

    /// A prompt becomes a denial when nobody can answer it: in a headless run and in `dontAsk`.
    fn would_prompt(&self, mode: PermissionMode, prompt: Prompt) -> Verdict {
        if mode == PermissionMode::DontAsk {
            return Verdict::Deny {
                tier: prompt.tier,
                rule: prompt.rule,
                why: format!(
                    "dontAsk mode denies what would need approval: {}",
                    prompt.reason
                ),
            };
        }
        if self.headless {
            return Verdict::Deny {
                tier: prompt.tier,
                rule: prompt.rule,
                why: format!("{}, and a headless run cannot ask the user", prompt.reason),
            };
        }
        Verdict::Prompt(prompt)
    }

    fn record(
        &self,
        cx: &CallCtx<'_>,
        tier: Tier,
        decision: Decision,
        rule: Option<String>,
        explanation: Option<String>,
    ) -> DecisionRecord {
        DecisionRecord {
            ts: now_rfc3339(),
            session_id: self.state.session_id.clone(),
            agent_id: cx.call.agent.to_string(),
            tool_use_id: cx.tool_use_id.clone(),
            tool: cx.call.tool.to_string(),
            args_digest: cx.digest.clone(),
            original_args_digest: cx.original_digest.clone(),
            tier,
            decision,
            rule,
            confidence: None,
            guardian_model: None,
            latency_ms: cx.started.elapsed().as_millis() as u64,
            guardian_profile: None,
            guardian_verdict: None,
            guardian_cache_hit: None,
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
            permission_mode: cx.mode.as_str().to_string(),
            headless: self.headless,
            explanation,
        }
    }

    /// The guardian's part of a record.
    fn with_guardian(
        mut record: DecisionRecord,
        adjudication: &Adjudication,
        screened: Option<&Screened>,
    ) -> DecisionRecord {
        if let Some(screened) = screened {
            screened.annotate(&mut record);
        }
        record.confidence = adjudication.confidence;
        record.guardian_model = Some(adjudication.model.clone());
        record.guardian_profile = Some(adjudication.profile.as_str().to_string());
        record.guardian_verdict = Some(adjudication.decision.as_str().to_string());
        record.guardian_cache_hit = Some(adjudication.cache_hit);
        record.guardian_latency_ms = Some(adjudication.latency_ms);
        record.fail_closed_reason = adjudication.fail_closed.map(|r| r.as_str().to_string());
        record
    }

    /// Puts the decision on the `fontaine.permission` span.
    fn note(tier: Tier, decision: Decision) {
        let span = tracing::Span::current();
        span.record("tier", tier.as_str());
        span.record("decision", decision.as_str());
    }

    /// What the ledger keeps of an executed call. The paths a read-only fast-path call names
    /// are not kept: reading does not create anything.
    fn action(&self, cx: &CallCtx<'_>, tier: Tier) -> Record {
        let paths = if matches!(tier, Tier::ReadOnlyFastPath | Tier::Tier05) {
            Vec::new()
        } else {
            touched_paths(cx.call.tool, &cx.args, &self.state.scope())
        };
        // Checked before the tool runs. A shell command only names paths, so its words prove
        // nothing was created; only `write` creates a file (`edit` reads its target first).
        let created = if cx.call.tool == "write" {
            paths
                .iter()
                .filter(|p| !Path::new(p).exists())
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        Record::Action {
            tool: cx.call.tool.to_string(),
            agent: cx.call.agent.to_string(),
            paths,
            created,
            summary: action_summary(cx.call.tool, &cx.args),
        }
    }

    fn denial(&self, cx: &CallCtx<'_>, tier: Tier, rule: Option<&str>, reason: &str) -> Record {
        Record::Denial {
            tool: cx.call.tool.to_string(),
            agent: cx.call.agent.to_string(),
            reason: reason.to_string(),
            rule: Some(rule.unwrap_or(tier.as_str()).to_string()),
            summary: action_summary(cx.call.tool, &cx.args),
        }
    }

    fn allow(&self, cx: &CallCtx<'_>, tier: Tier, rule: Option<String>) -> GateDecision {
        Self::note(tier, Decision::Allow);
        self.emit(self.record(cx, tier, Decision::Allow, rule, None));
        self.state.append(self.action(cx, tier));
        GateDecision::Allow {
            args: cx.args.clone(),
        }
    }

    async fn deny(
        &self,
        cx: &CallCtx<'_>,
        tier: Tier,
        rule: Option<String>,
        message: String,
    ) -> GateDecision {
        Self::note(tier, Decision::Deny);
        self.emit(self.record(
            cx,
            tier,
            Decision::Deny,
            rule.clone(),
            Some(message.clone()),
        ));
        self.state
            .append(self.denial(cx, tier, rule.as_deref(), &message));
        // The deterministic floor and the mode have no retry grant to give, so a hook's `retry`
        // changes nothing here; the hook still hears about the denial.
        self.denied_hooks(cx, tier, &message).await;
        GateDecision::Deny { message }
    }

    /// Tells the `permission_denied` hooks about a denial. Returns whether one of them, one the
    /// user defined, says the agent may retry exactly this call; names that hook.
    async fn denied_hooks(&self, cx: &CallCtx<'_>, tier: Tier, reason: &str) -> Option<String> {
        if self.denied_hooks.is_empty() {
            return None;
        }
        let scope = self.state.scope();
        let (cwd, roots) = self.workspace_view();
        let mut retry = None;
        for hook in &self.denied_hooks {
            let covered = match hook.matcher().and_then(|text| parse_rule(&text).ok()) {
                Some(rule) => matches(
                    &rule,
                    &CallContext {
                        tool: cx.call.tool,
                        args: &cx.args,
                        scope: &scope,
                        home: self.state.home.as_deref(),
                    },
                    Posture::Deny,
                ),
                None => true,
            };
            if !covered {
                continue;
            }
            let event = PermissionDeniedEvent {
                tool: cx.call.tool,
                args: &cx.args,
                agent: cx.call.agent,
                tool_use_id: cx.tool_use_id.as_deref(),
                session_id: &self.state.session_id,
                cwd: &cwd,
                permission_mode: cx.mode.as_str(),
                workspace_roots: &roots,
                tier,
                reason,
            };
            let id = hook.id();
            match tokio::time::timeout(hook.timeout(), hook.run(event)).await {
                Ok(Ok(true)) if hook.can_retry() => retry = retry.or(Some(id.to_string())),
                Ok(Ok(true)) => {
                    tracing::debug!(hook = %id, "a project hook's retry is ignored");
                }
                Ok(Ok(false)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(hook = %id, error = ?e, "a permission_denied hook failed; it decides nothing");
                }
                Err(_) => {
                    tracing::warn!(hook = %id, "a permission_denied hook timed out; it decides nothing");
                }
            }
        }
        retry
    }

    async fn adjudicate(&self, call: CallView<'_>) -> GateDecision {
        let started = Instant::now();
        let tool_use_id = self.state.current_call(call.agent);
        let (mode, plan_block) = self.posture_of(call.agent);
        let hook = self.run_hooks(&call, tool_use_id.as_deref(), mode).await;
        let args = hook
            .updated_input
            .clone()
            .unwrap_or_else(|| call.args.clone());
        let cx = CallCtx {
            call: &call,
            digest: args_digest(&args),
            original_digest: hook.updated_input.is_some().then(|| args_digest(call.args)),
            args,
            tool_use_id,
            mode,
            started,
        };

        match self.evaluate(&call, &cx.args, &hook, mode, plan_block) {
            Verdict::Allow { tier, rule } => self.allow(&cx, tier, rule),
            Verdict::Deny { tier, rule, why } => {
                self.deny(&cx, tier, rule, denied(call.tool, &why)).await
            }
            Verdict::Prompt(prompt) => self.prompt_or_ask(&cx, prompt).await,
            Verdict::Guardian => self.guardian_branch(&cx, &hook).await,
        }
    }

    /// Records the prompt and puts it to the human.
    async fn prompt_or_ask(&self, cx: &CallCtx<'_>, prompt: Prompt) -> GateDecision {
        Self::note(prompt.tier, Decision::Prompt);
        self.emit(self.record(cx, prompt.tier, Decision::Prompt, prompt.rule.clone(), None));
        self.ask_human(cx, prompt).await
    }

    /// A prompt that nobody can answer is a denial; otherwise it is asked.
    async fn human_or_deny(&self, cx: &CallCtx<'_>, prompt: Prompt) -> GateDecision {
        match self.would_prompt(cx.mode, prompt) {
            Verdict::Prompt(prompt) => self.prompt_or_ask(cx, prompt).await,
            Verdict::Deny { tier, rule, why } => {
                self.deny(cx, tier, rule, denied(cx.call.tool, &why)).await
            }
            Verdict::Allow { .. } | Verdict::Guardian => {
                self.deny(
                    cx,
                    Tier::Guardian,
                    None,
                    denied(cx.call.tool, "it needs the user's approval"),
                )
                .await
            }
        }
    }

    /// The guardian decides, unless the human already settled this exact call.
    async fn guardian_branch(&self, cx: &CallCtx<'_>, hook: &HookOutcome) -> GateDecision {
        let call = cx.call;
        // `/denied` then `r`: the human approved this exact call once. The floor has already
        // run; the guardian is not asked and the approval never enters the cache or the
        // breaker.
        if let Some(source) = self.state.take_retry(call.agent, call.tool, &cx.digest) {
            match source {
                RetrySource::Human => {
                    Self::note(Tier::Human, Decision::Allow);
                    self.emit(self.record(
                        cx,
                        Tier::Human,
                        Decision::Allow,
                        Some("retry approved by the user".to_string()),
                        None,
                    ));
                    self.approved(cx, Tier::Human, "once");
                }
                // A hook's grant is not a human's answer: it must not resume paused auto mode
                // or reset the guardian breaker.
                RetrySource::Hook(id) => {
                    Self::note(Tier::Hook, Decision::Allow);
                    self.emit(self.record(
                        cx,
                        Tier::Hook,
                        Decision::Allow,
                        Some(id),
                        Some("retry approved by a permission_denied hook".to_string()),
                    ));
                    self.state.append(self.action(cx, Tier::Hook));
                }
            }
            return GateDecision::Allow {
                args: cx.args.clone(),
            };
        }
        if self.state.auto_paused() {
            return self
                .human_or_deny(
                    cx,
                    Prompt {
                        tier: Tier::Guardian,
                        rule: None,
                        reason: "auto mode is paused after repeated guardian denials; approving this call resumes it".to_string(),
                        forced: true,
                    },
                )
                .await;
        }

        let request = adjudication::build(
            &self.state,
            call.tool,
            &cx.args,
            hook.guardian_context.clone(),
            call.annotations,
        );
        let stage = self.state.screen();
        let guardian = self.state.guardian();
        let composed = fontaine_guardian::cascade::adjudicate(
            stage.as_ref().map(|stage| ScreenInput {
                screen: &stage.screen,
                enforce: !stage.shadow,
            }),
            &guardian,
            &request,
        )
        .await;
        let mut screened = self.note_screen(stage, composed.screen);
        if let Some(screened) = &mut screened
            && let Some(input_tokens) = composed.dropped_input_tokens
            && let Some(WireIds { provider, model }) = guardian.wire_ids()
        {
            screened.dropped = Some(Dropped {
                input_tokens,
                provider,
                model,
                priced: guardian.settings().priced,
            });
        }
        let mut adjudication = match composed.decided {
            Decided::ScreenStop(scores) => {
                let screened = screened.expect("a screen stop comes from a screen");
                return self.screen_stop(cx, &screened, scores.risk).await;
            }
            Decided::ScreenAllow(scores) => {
                let screened = screened.expect("a screen allow comes from a screen");
                return self.screen_allow(cx, &screened, scores);
            }
            Decided::Guardian(adjudication) => *adjudication,
        };
        if adjudication.fail_closed == Some(FailClosed::Unavailable) && !self.headless {
            let info = GuardianUnavailable {
                model: adjudication.model.clone(),
                reason: self
                    .state
                    .guardian()
                    .unavailable_reason()
                    .unwrap_or_default()
                    .to_string(),
                tool: call.tool.to_string(),
            };
            let _waiting = self.state.wait_on_human(call.agent);
            match self.approver.guardian_unavailable(info).await {
                UnavailableChoice::Retry => {
                    adjudication = self.state.guardian().adjudicate(&request).await;
                }
                UnavailableChoice::Manual => {
                    self.state.set_mode(PermissionMode::Manual);
                    return self
                        .prompt_or_ask(
                            cx,
                            Prompt {
                                tier: Tier::Mode,
                                rule: None,
                                reason: "it needs the user's approval".to_string(),
                                forced: false,
                            },
                        )
                        .await;
                }
                UnavailableChoice::Deny => {}
            }
        }
        self.state
            .add_guardian_usage(cx.call.agent, &adjudication.usage);
        self.settle(cx, adjudication, screened.as_ref()).await
    }

    /// What the screen made of a call, for the audit record. A screen that cannot answer is
    /// reported once and the guardian decides.
    fn note_screen(
        &self,
        stage: Option<ScreenStage>,
        outcome: Option<ScreenOutcome>,
    ) -> Option<Screened> {
        let (stage, outcome) = stage.zip(outcome)?;
        if let ScreenOutcome::Unavailable { reason, .. } = &outcome
            && self.state.first_screen_warning()
        {
            self.approver.warn(&format!(
                "the screen model {} could not be asked ({reason}); the guardian decides every call",
                stage.label
            ));
        }
        Some(Screened {
            outcome,
            model: stage.label,
            dropped: None,
        })
    }

    /// The screen judged the call clearly within the user's request and free of steering. The
    /// guardian was dropped, so its breaker and cache are untouched.
    fn screen_allow(
        &self,
        cx: &CallCtx<'_>,
        screened: &Screened,
        scores: ScreenScores,
    ) -> GateDecision {
        let figure = |value: Option<f64>| value.map_or("n/a".to_string(), |v| format!("{v:.2e}"));
        let explanation = format!(
            "a local screen judged this call clearly within the user's request (allow risk {}, steering {})",
            figure(scores.allow_risk),
            figure(scores.steering)
        );
        Self::note(Tier::Screen, Decision::Allow);
        let mut record = self.record(
            cx,
            Tier::Screen,
            Decision::Allow,
            Some("screen".to_string()),
            Some(explanation),
        );
        screened.annotate(&mut record);
        self.emit(record);
        self.state.append(self.action(cx, Tier::Screen));
        GateDecision::Allow {
            args: cx.args.clone(),
        }
    }

    /// The screen judged the call outside the user's request. It escalates exactly as a guardian
    /// escalation does; the guardian, its cache and its breaker are not involved.
    async fn screen_stop(&self, cx: &CallCtx<'_>, screened: &Screened, risk: f64) -> GateDecision {
        let reason =
            format!("a local screen judged this call outside the user's request (risk {risk:.2})");
        let rule = "screen".to_string();
        let annotated = |decision, explanation| {
            let mut record =
                self.record(cx, Tier::Screen, decision, Some(rule.clone()), explanation);
            screened.annotate(&mut record);
            record
        };
        if !self.headless {
            Self::note(Tier::Screen, Decision::Prompt);
            self.emit(annotated(Decision::Prompt, None));
            return self
                .ask_human(
                    cx,
                    Prompt {
                        tier: Tier::Screen,
                        rule: Some(rule),
                        reason,
                        forced: true,
                    },
                )
                .await;
        }
        let message = denied(
            cx.call.tool,
            &format!("{reason}, and a headless run cannot ask the user"),
        );
        // The screen has nothing to retry, so a hook's retry offer is dropped.
        self.denied_hooks(cx, Tier::Screen, &reason).await;
        Self::note(Tier::Screen, Decision::Deny);
        self.emit(annotated(Decision::Deny, Some(message.clone())));
        self.state
            .append(self.denial(cx, Tier::Screen, Some(&rule), &message));
        GateDecision::Deny { message }
    }

    /// Acts on what the guardian came to.
    async fn settle(
        &self,
        cx: &CallCtx<'_>,
        adjudication: Adjudication,
        screened: Option<&Screened>,
    ) -> GateDecision {
        let call = cx.call;
        let rule = adjudication.rule.as_str().to_string();
        match adjudication.decision {
            GuardianDecision::Allow => {
                self.state.guardian_allowed();
                Self::note(Tier::Guardian, Decision::Allow);
                self.emit(Self::with_guardian(
                    self.record(cx, Tier::Guardian, Decision::Allow, Some(rule), None),
                    &adjudication,
                    screened,
                ));
                self.state.append(self.action(cx, Tier::Guardian));
                GateDecision::Allow {
                    args: cx.args.clone(),
                }
            }
            GuardianDecision::Escalate if !self.headless => {
                Self::note(Tier::Guardian, Decision::Prompt);
                self.emit(Self::with_guardian(
                    self.record(
                        cx,
                        Tier::Guardian,
                        Decision::Prompt,
                        Some(rule.clone()),
                        None,
                    ),
                    &adjudication,
                    screened,
                ));
                self.ask_human(
                    cx,
                    Prompt {
                        tier: Tier::Guardian,
                        reason: format!(
                            "the guardian wants you to decide: [{rule}] {}",
                            adjudication.explanation
                        ),
                        rule: Some(rule),
                        forced: true,
                    },
                )
                .await
            }
            GuardianDecision::Deny | GuardianDecision::Escalate => {
                let why = if adjudication.decision == GuardianDecision::Escalate {
                    format!(
                        "{}, and a headless run cannot ask the user",
                        adjudication.explanation.trim_end_matches('.')
                    )
                } else {
                    adjudication.explanation.clone()
                };
                let mut message = format!(
                    "Denied by guardian: [{rule}] {why}. Do not retry it or look for another route to the same effect; tell the user what you could not do."
                );
                // A user's `permission_denied` hook may allow exactly one retry, as `/denied`
                // then `r` does. It is asked first so the agent is told the truth.
                let retry = self
                    .denied_hooks(cx, Tier::Guardian, &adjudication.explanation)
                    .await;
                if let Some(hook_id) = &retry {
                    message = format!(
                        "Denied by guardian: [{rule}] {why}. A permission_denied hook approved one retry of exactly this call: you may run it again once, unchanged."
                    );
                    self.state.grant_retry(
                        RetrySource::Hook(hook_id.clone()),
                        call.agent,
                        call.tool,
                        &cx.digest,
                    );
                }
                Self::note(Tier::Guardian, Decision::Deny);
                let record = Self::with_guardian(
                    self.record(
                        cx,
                        Tier::Guardian,
                        Decision::Deny,
                        Some(rule.clone()),
                        Some(message.clone()),
                    ),
                    &adjudication,
                    screened,
                );
                self.state
                    .append(self.denial(cx, Tier::Guardian, Some(&rule), &message));
                self.state.record_denied(DeniedCall {
                    tool: call.tool.to_string(),
                    agent: call.agent.to_string(),
                    tool_use_id: cx.tool_use_id.clone(),
                    args: cx.args.clone(),
                    args_digest: cx.digest.clone(),
                    summary: action_summary(call.tool, &cx.args),
                    rule,
                    explanation: adjudication.explanation,
                    retried: retry.is_some(),
                });
                // The list a surface reads when it sees this event is complete before the event.
                self.emit(record);
                if self.state.guardian_denied() {
                    self.approver.notice(
                        "auto mode paused after repeated guardian denials: the next call that needs the guardian asks you, and approving it resumes auto mode",
                    );
                }
                GateDecision::Deny { message }
            }
        }
    }

    /// The ledger's approval and action records for a call a human allowed.
    fn approved(&self, cx: &CallCtx<'_>, tier: Tier, scope: &str) {
        self.state.append(Record::Approval {
            tool: cx.call.tool.to_string(),
            agent: cx.call.agent.to_string(),
            scope: scope.to_string(),
        });
        self.state.append(self.action(cx, tier));
        if self.state.auto_paused() {
            self.state.resume_auto();
        } else {
            self.state.guardian_allowed();
        }
    }

    async fn ask_human(&self, cx: &CallCtx<'_>, prompt: Prompt) -> GateDecision {
        let call = cx.call;
        if let Some(why) = self.state.agents().exhausted_of(call.agent) {
            let message = denied(call.tool, &why.describe());
            self.emit(self.record(cx, Tier::Human, Decision::Deny, None, Some(message.clone())));
            self.state
                .append(self.denial(cx, Tier::Human, None, &message));
            return GateDecision::Deny { message };
        }
        let args = cx.args.clone();
        let scope = self.state.scope();
        let proposed = if prompt.forced {
            None
        } else {
            propose(&CallContext {
                tool: call.tool,
                args: &args,
                scope: &scope,
                home: self.state.home.as_deref(),
            })
        };
        let offers = Offers {
            session: proposed.is_some(),
            always: proposed.is_some() && self.user_config.is_some(),
        };
        let request = ApprovalRequest {
            tool: call.tool.to_string(),
            agent: call.agent.to_string(),
            tool_use_id: cx.tool_use_id.clone(),
            summary: summarize(call.tool, &args),
            args: args.clone(),
            tier: prompt.tier,
            reason: prompt.reason.clone(),
            offers,
            proposed_rule: proposed.as_ref().map(ToString::to_string),
        };
        let _waiting = self.state.wait_on_human(call.agent);
        let answer = self
            .approver
            .approve(request)
            .instrument(tracing::info_span!(
                target: TARGET,
                "fontaine.tool.blocked_on_user",
                tool_name = self.state.telemetry_tool_name(call.tool),
            ))
            .await;

        match answer {
            Answer::AllowOnce => {
                self.emit(self.record(cx, Tier::Human, Decision::Allow, None, None));
                self.approved(cx, Tier::Human, "once");
                GateDecision::Allow { args }
            }
            Answer::AllowSession if offers.session => {
                let rule = proposed.expect("offered with a proposal");
                self.emit(self.record(
                    cx,
                    Tier::Human,
                    Decision::Allow,
                    Some(format!("{rule} [session]")),
                    None,
                ));
                self.state.append(Record::Grant {
                    rule: rule.to_string(),
                });
                self.state.grant(rule, RuleSource::Session);
                self.approved(cx, Tier::Human, "session");
                GateDecision::Allow { args }
            }
            Answer::AllowAlways if offers.always => {
                let rule = proposed.expect("offered with a proposal");
                let written = match &self.user_config {
                    Some(path) => {
                        user_config::append_allow_rule(path, &rule.to_string()).map_err(|e| {
                            RuntimeError::caused(format!("could not write {}", path.display()), e)
                        })
                    }
                    None => Err(RuntimeError::new("no user settings file")),
                };
                let saved = written.is_ok();
                let source = match written {
                    Ok(()) => SessionState::ALWAYS,
                    Err(e) => {
                        self.approver.notice(&format!(
                            "could not save `{rule}` to the settings file ({}); it holds for this session only",
                            chain(&e)
                        ));
                        RuleSource::Session
                    }
                };
                self.emit(self.record(
                    cx,
                    Tier::Human,
                    Decision::Allow,
                    Some(format!("{rule} [{}]", source.label())),
                    None,
                ));
                self.state.append(Record::Grant {
                    rule: rule.to_string(),
                });
                self.state.grant(rule, source);
                self.approved(cx, Tier::Human, if saved { "always" } else { "session" });
                GateDecision::Allow { args }
            }
            // A grant the prompt did not offer is only the one call.
            Answer::AllowSession | Answer::AllowAlways => {
                self.emit(self.record(cx, Tier::Human, Decision::Allow, None, None));
                self.approved(cx, Tier::Human, "once");
                GateDecision::Allow { args }
            }
            Answer::Deny { feedback } => {
                let message = match feedback.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
                    Some(feedback) => format!(
                        "The user denied this `{}` call: {feedback}. Do not retry it as it was.",
                        call.tool
                    ),
                    None => denied(call.tool, "the user declined it"),
                };
                self.emit(self.record(
                    cx,
                    Tier::Human,
                    Decision::Deny,
                    None,
                    Some(message.clone()),
                ));
                self.state
                    .append(self.denial(cx, Tier::Human, None, &message));
                GateDecision::Deny { message }
            }
        }
    }

    /// Audit log first, then the telemetry event, then whoever follows the decision feed.
    fn emit(&self, record: DecisionRecord) {
        record_decision(&self.state, &record);
        self.state.publish(record);
    }
}

/// The audit append and the `tool_decision` event for one decision, without the decision feed.
pub(crate) fn record_decision(state: &SessionState, record: &DecisionRecord) {
    if let Err(e) = state.audit.append(record) {
        state.sink_failed(crate::gate::Sink::Audit, &e);
    }
    tracing::info!(
        target: TARGET,
        tool_name = state.telemetry_tool_name(&record.tool),
        agent.id = record.agent_id.as_str(),
        agent.type = state.agents().type_of(&record.agent_id).as_deref().unwrap_or("main"),
        tool_use_id = record.tool_use_id.as_deref().unwrap_or(""),
        tier = record.tier.as_str(),
        decision = record.decision.as_str(),
        permission_mode = record.permission_mode.as_str(),
        headless = record.headless,
        latency_ms = record.latency_ms,
        guardian.model = record.guardian_model.as_deref(),
        guardian.profile = record.guardian_profile.as_deref(),
        guardian.verdict = record.guardian_verdict.as_deref(),
        guardian.confidence = record.confidence,
        guardian.cache_hit = record.guardian_cache_hit,
        guardian.latency_ms = record.guardian_latency_ms,
        fail_closed_reason = record.fail_closed_reason.as_deref(),
        screen.outcome = record.screen_outcome.as_deref(),
        screen.risk = record.screen_risk,
        screen.allow_risk = record.screen_allow_risk,
        screen.steering = record.screen_steering,
        screen.model = record.screen_model.as_deref(),
        screen.latency_ms = record.screen_latency_ms,
        guardian.dropped_input_tokens = record.guardian_dropped_input_tokens,
        guardian.dropped_provider = record.guardian_dropped_provider.as_deref(),
        guardian.dropped_model = record.guardian_dropped_model.as_deref(),
        guardian.dropped_priced = record.guardian_dropped_priced,
        "tool_decision"
    );
}

/// Read-only tools that take no path, so there is nothing for containment to check: the skill
/// loader, the question dialog and the task list.
fn names_no_path(tool: &str) -> bool {
    matches!(tool, "skill" | "ask_user_question" | "todo_write")
}

/// The call as the human is shown it: the whole command, path or URL, never cut short.
pub fn summarize(tool: &str, args: &Value) -> String {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    let line = match tool {
        "shell" => text("command").map(str::to_string),
        "read" | "write" | "edit" => text("path").map(str::to_string),
        "webfetch" => text("url").map(str::to_string),
        "glob" => text("pattern").map(str::to_string),
        "grep" => text("pattern").map(str::to_string),
        "ls" => Some(text("path").unwrap_or(".").to_string()),
        "ask_user_question" => {
            let headers: Vec<&str> = args
                .get("questions")
                .and_then(Value::as_array)
                .map(|questions| {
                    questions
                        .iter()
                        .filter_map(|q| q.get("header").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            (!headers.is_empty()).then(|| headers.join(", "))
        }
        _ => None,
    };
    line.unwrap_or_else(|| args.to_string())
}

impl ToolGate for PermissionPipeline {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        let span = tracing::info_span!(
            target: TARGET,
            "fontaine.permission",
            tool_name = self.state.telemetry_tool_name(call.tool),
            permission_mode = self.posture_of(call.agent).0.as_str(),
            tier = tracing::field::Empty,
            decision = tracing::field::Empty,
        );
        Box::pin(self.adjudicate(call).instrument(span))
    }
}
