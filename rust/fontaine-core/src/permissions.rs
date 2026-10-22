//! The permission pipeline: the one place that decides whether a tool call runs. Every wrapped
//! tool reaches it through the `ToolGate` trait. The order is the one in the design: user
//! pre-tool-use hooks, the deterministic floor (deny rules, ask rules, critical paths, protected
//! paths) in every mode, then the read-only fast path, the shell pre-classifier, allow rules and
//! finally what the mode does with a call nobody pre-approved.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fontaine_tools::gate::{CallView, GateDecision, ToolGate};
use fontaine_tools::preclassify::{Environment, Preclassifier};
use fontaine_tools::risk::RiskClass;
use futures::future::BoxFuture;
use serde_json::Value;
use tracing::Instrument;

use crate::audit::{Decision, DecisionRecord, args_digest, now_rfc3339};
use crate::config::PermissionMode;
use crate::floor::{self, CriticalEnv};
use crate::gate::SessionState;
use crate::ledger::Record;
use crate::permission_rules::{CallContext, Posture, RuleSource, matches, propose};
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
}

/// An approver that denies everything, for runs where nobody can answer.
pub struct DenyAll;

impl Approver for DenyAll {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
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
}

#[derive(Debug)]
pub enum HookError {
    Failed(String),
    Malformed(String),
}

pub struct PreToolUseEvent<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub agent: &'a str,
    pub tool_use_id: Option<&'a str>,
}

/// The slot for user `pre_tool_use` hooks. A hook that errors, times out or answers something
/// unreadable contributes no decision; it never loosens anything.
pub trait PreToolUseHook: Send + Sync {
    fn id(&self) -> String;

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn run<'a>(
        &'a self,
        event: PreToolUseEvent<'a>,
    ) -> BoxFuture<'a, Result<HookDecision, HookError>>;
}

pub struct PipelineParams {
    pub state: Arc<SessionState>,
    pub approver: Arc<dyn Approver>,
    /// Nothing can be asked: every would-prompt outcome becomes a denial.
    pub headless: bool,
    pub hooks: Vec<Arc<dyn PreToolUseHook>>,
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
    preclassifier: Preclassifier,
    user_config: Option<PathBuf>,
}

#[derive(Default)]
struct HookOutcome {
    deny: Option<(String, Option<String>)>,
    ask: Option<String>,
    allow: Option<String>,
    updated_input: Option<Value>,
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
}

fn denied(tool: &str, why: &str) -> String {
    format!(
        "`{tool}` was denied: {why}. Do not retry it or look for another route to the same \
         effect; tell the user what you could not do."
    )
}

impl PermissionPipeline {
    pub fn new(params: PipelineParams) -> PermissionPipeline {
        PermissionPipeline {
            preclassifier: Preclassifier::new(&params.allow_extra),
            state: params.state,
            approver: params.approver,
            headless: params.headless,
            hooks: params.hooks,
            user_config: params.user_config,
        }
    }

    pub fn state(&self) -> &Arc<SessionState> {
        &self.state
    }

    async fn run_hooks(&self, call: &CallView<'_>, tool_use_id: Option<&str>) -> HookOutcome {
        let mut outcome = HookOutcome::default();
        let mut args = call.args.clone();
        for hook in &self.hooks {
            let event = PreToolUseEvent {
                tool: call.tool,
                args: &args,
                agent: call.agent,
                tool_use_id,
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
                Some(HookPermission::Allow) => {
                    outcome.allow.get_or_insert(id);
                }
                None => {}
            }
        }
        outcome
    }

    /// Steps 1a to 9 for one call, with no side effects.
    fn evaluate(&self, call: &CallView<'_>, args: &Value, hook: &HookOutcome) -> Verdict {
        let mode = self.state.mode();
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
        if mode == PermissionMode::Plan && edit_class {
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
            && let Some(found) = floor::protected_path(&scope, home, args)
        {
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
        if call.risk == RiskClass::ReadOnly && (located.is_ok() || tool == "skill") {
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
            if self.preclassifier.is_read_only(command, &env) {
                return Verdict::Allow {
                    tier: Tier::Tier05,
                    rule: None,
                };
            }
        }
        if let Some(rule) = rules
            .allow
            .iter()
            .find(|r| matches(&r.rule, &ctx, Posture::Allow))
        {
            return Verdict::Allow {
                tier: Tier::AllowRule,
                rule: Some(rule.label()),
            };
        }
        if let Some(id) = &hook.allow {
            return Verdict::Allow {
                tier: Tier::Hook,
                rule: Some(id.clone()),
            };
        }

        let outside = match &located {
            Err(e) if call.risk != RiskClass::Arbitrary => Some(format!(
                "{e}; file tools work inside the workspace roots only (add one with /add-dir)"
            )),
            _ => None,
        };
        let needs_approval = || Prompt {
            tier: Tier::Mode,
            rule: None,
            reason: outside
                .clone()
                .unwrap_or_else(|| "it needs the user's approval".to_string()),
            forced: false,
        };
        match mode {
            PermissionMode::DontAsk => Verdict::Deny {
                tier: Tier::DontAsk,
                rule: None,
                why: "dontAsk mode only runs calls that are pre-approved".to_string(),
            },
            PermissionMode::Manual | PermissionMode::Plan => {
                self.would_prompt(mode, needs_approval())
            }
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
            PermissionMode::Auto => Verdict::Deny {
                tier: Tier::Guardian,
                rule: None,
                why: "auto mode is not available because no guardian is wired".to_string(),
            },
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

    async fn adjudicate(&self, call: CallView<'_>) -> GateDecision {
        let started = Instant::now();
        let tool_use_id = self.state.current_call(call.agent);
        let hook = self.run_hooks(&call, tool_use_id.as_deref()).await;
        let args = hook
            .updated_input
            .clone()
            .unwrap_or_else(|| call.args.clone());
        let original_digest = hook.updated_input.is_some().then(|| args_digest(call.args));
        let digest = args_digest(&args);
        let mode = self.state.mode();

        let make =
            |tier: Tier, decision: Decision, rule: Option<String>, explanation: Option<String>| {
                DecisionRecord {
                    ts: now_rfc3339(),
                    session_id: self.state.session_id.clone(),
                    agent_id: call.agent.to_string(),
                    tool_use_id: tool_use_id.clone(),
                    tool: call.tool.to_string(),
                    args_digest: digest.clone(),
                    original_args_digest: original_digest.clone(),
                    tier,
                    decision,
                    rule,
                    confidence: None,
                    guardian_model: None,
                    latency_ms: started.elapsed().as_millis() as u64,
                    permission_mode: mode.as_str().to_string(),
                    headless: self.headless,
                    explanation,
                }
            };
        let span = tracing::Span::current();

        match self.evaluate(&call, &args, &hook) {
            Verdict::Allow { tier, rule } => {
                span.record("tier", tier.as_str());
                span.record("decision", Decision::Allow.as_str());
                self.emit(make(tier, Decision::Allow, rule, None));
                self.state.append(Record::Action {
                    tool: call.tool.to_string(),
                    agent: call.agent.to_string(),
                });
                GateDecision::Allow { args }
            }
            Verdict::Deny { tier, rule, why } => {
                let message = denied(call.tool, &why);
                span.record("tier", tier.as_str());
                span.record("decision", Decision::Deny.as_str());
                self.emit(make(tier, Decision::Deny, rule, Some(message.clone())));
                self.state.append(Record::Denial {
                    tool: call.tool.to_string(),
                    agent: call.agent.to_string(),
                    reason: message.clone(),
                });
                GateDecision::Deny { message }
            }
            Verdict::Prompt(prompt) => {
                span.record("tier", prompt.tier.as_str());
                span.record("decision", Decision::Prompt.as_str());
                self.emit(make(
                    prompt.tier,
                    Decision::Prompt,
                    prompt.rule.clone(),
                    None,
                ));
                self.ask_human(&call, args, &tool_use_id, prompt, make)
                    .await
            }
        }
    }

    async fn ask_human(
        &self,
        call: &CallView<'_>,
        args: Value,
        tool_use_id: &Option<String>,
        prompt: Prompt,
        make: impl Fn(Tier, Decision, Option<String>, Option<String>) -> DecisionRecord,
    ) -> GateDecision {
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
            tool_use_id: tool_use_id.clone(),
            summary: summarize(call.tool, &args),
            args: args.clone(),
            tier: prompt.tier,
            reason: prompt.reason.clone(),
            offers,
            proposed_rule: proposed.as_ref().map(ToString::to_string),
        };
        let answer = self
            .approver
            .approve(request)
            .instrument(tracing::info_span!(
                target: TARGET,
                "fontaine.tool.blocked_on_user",
                tool_name = call.tool,
            ))
            .await;

        let tool = call.tool.to_string();
        let agent = call.agent.to_string();
        let approved = |scope: &str| {
            self.state.append(Record::Approval {
                tool: tool.clone(),
                agent: agent.clone(),
                scope: scope.to_string(),
            });
            self.state.append(Record::Action {
                tool: tool.clone(),
                agent: agent.clone(),
            });
        };
        match answer {
            Answer::AllowOnce => {
                self.emit(make(Tier::Human, Decision::Allow, None, None));
                approved("once");
                GateDecision::Allow { args }
            }
            Answer::AllowSession if offers.session => {
                let rule = proposed.expect("offered with a proposal");
                self.emit(make(
                    Tier::Human,
                    Decision::Allow,
                    Some(format!("{rule} [session]")),
                    None,
                ));
                self.state.append(Record::Grant {
                    rule: rule.to_string(),
                });
                self.state.grant(rule, RuleSource::Session);
                approved("session");
                GateDecision::Allow { args }
            }
            Answer::AllowAlways if offers.always => {
                let rule = proposed.expect("offered with a proposal");
                let written = match &self.user_config {
                    Some(path) => user_config::append_allow_rule(path, &rule.to_string())
                        .map_err(|e| format!("{}: {e}", path.display())),
                    None => Err("no user settings file".to_string()),
                };
                let source = match written {
                    Ok(()) => SessionState::ALWAYS,
                    Err(e) => {
                        self.approver.notice(&format!(
                            "could not save `{rule}` to the settings file ({e}); it holds for this session only"
                        ));
                        RuleSource::Session
                    }
                };
                self.emit(make(
                    Tier::Human,
                    Decision::Allow,
                    Some(format!("{rule} [{}]", source.label())),
                    None,
                ));
                self.state.append(Record::Grant {
                    rule: rule.to_string(),
                });
                self.state.grant(rule, source);
                approved("always");
                GateDecision::Allow { args }
            }
            // A grant the prompt did not offer is only the one call.
            Answer::AllowSession | Answer::AllowAlways => {
                self.emit(make(Tier::Human, Decision::Allow, None, None));
                approved("once");
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
                self.emit(make(
                    Tier::Human,
                    Decision::Deny,
                    None,
                    Some(message.clone()),
                ));
                self.state.append(Record::Denial {
                    tool: call.tool.to_string(),
                    agent: call.agent.to_string(),
                    reason: message.clone(),
                });
                GateDecision::Deny { message }
            }
        }
    }

    /// Audit log first, then the telemetry event, then whoever follows the decision feed.
    fn emit(&self, record: DecisionRecord) {
        if let Err(e) = self.state.audit.append(&record) {
            tracing::warn!(error = %e, "the audit log could not be written");
        }
        tracing::info!(
            target: TARGET,
            tool_name = record.tool.as_str(),
            tool_use_id = record.tool_use_id.as_deref().unwrap_or(""),
            tier = record.tier.as_str(),
            decision = record.decision.as_str(),
            permission_mode = record.permission_mode.as_str(),
            headless = record.headless,
            latency_ms = record.latency_ms,
            "tool_decision"
        );
        self.state.publish(record);
    }
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
        _ => None,
    };
    line.unwrap_or_else(|| args.to_string())
}

impl ToolGate for PermissionPipeline {
    fn check<'a>(&'a self, call: CallView<'a>) -> BoxFuture<'a, GateDecision> {
        let span = tracing::info_span!(
            target: TARGET,
            "fontaine.permission",
            tool_name = call.tool,
            permission_mode = self.state.mode().as_str(),
            tier = tracing::field::Empty,
            decision = tracing::field::Empty,
        );
        Box::pin(self.adjudicate(call).instrument(span))
    }
}
