//! The event stream of a session: the `headless-v1` objects as typed values. `fontaine -p` writes
//! them as JSON lines and an embedded [`Session`](crate::Session) yields them, so both carry the
//! same bytes. The schema is `docs/schemas/headless-v1.schema.json`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::core::afk_report::AfkReport;
use crate::core::agent::AgentRuntime;
use crate::core::audit::{Converted, DecisionRecord};
use crate::core::config::PermissionMode;
use crate::core::goal::Judgment;
use crate::core::trust;
use crate::core::turn::SessionEvent;
use crate::core::usage_ledger::{Bucket, UsageSource};
use crate::core::usage_pause::{PauseEnd, PauseEvent, PauseInfo};

pub const SCHEMA_VERSION: u32 = 1;

/// A payload followed by the three keys every object of the stream ends with.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Line<P> {
    #[serde(flatten)]
    pub payload: P,
    pub r#type: &'static str,
    pub schema_version: u32,
    pub session_id: String,
}

impl<P> Line<P> {
    fn new(kind: &'static str, session_id: &str, payload: P) -> Line<P> {
        Line {
            payload,
            r#type: kind,
            schema_version: SCHEMA_VERSION,
            session_id: session_id.to_string(),
        }
    }
}

/// One object of the `headless-v1` stream, except the `user_input` line a consumer sends.
/// Serializes to the exact JSON `fontaine -p --output-format stream-json` prints.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)] // events are short-lived values, and boxing would leak into the public variants
pub enum Event {
    Init(Line<Init>),
    Assistant(Line<Text>),
    AssistantDelta(Line<Text>),
    ToolCall(Line<ToolCall>),
    ToolResult(Line<ToolResult>),
    ModelCall(Line<ModelCall>),
    GateDecision(Line<GateDecision>),
    Goal(Line<GoalEvaluation>),
    UsagePaused(Line<UsagePaused>),
    UsageResumed(Line<UsageResumed>),
    Result(Line<ResultEvent>),
    AfkReport(Line<AfkReportEvent>),
}

impl Event {
    /// The `type` of the object.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Init(l) => l.r#type,
            Event::Assistant(l) | Event::AssistantDelta(l) => l.r#type,
            Event::ToolCall(l) => l.r#type,
            Event::ToolResult(l) => l.r#type,
            Event::ModelCall(l) => l.r#type,
            Event::GateDecision(l) => l.r#type,
            Event::Goal(l) => l.r#type,
            Event::UsagePaused(l) => l.r#type,
            Event::UsageResumed(l) => l.r#type,
            Event::Result(l) => l.r#type,
            Event::AfkReport(l) => l.r#type,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Event::Init(l) => &l.session_id,
            Event::Assistant(l) | Event::AssistantDelta(l) => &l.session_id,
            Event::ToolCall(l) => &l.session_id,
            Event::ToolResult(l) => &l.session_id,
            Event::ModelCall(l) => &l.session_id,
            Event::GateDecision(l) => &l.session_id,
            Event::Goal(l) => &l.session_id,
            Event::UsagePaused(l) => &l.session_id,
            Event::UsageResumed(l) => &l.session_id,
            Event::Result(l) => &l.session_id,
            Event::AfkReport(l) => &l.session_id,
        }
    }

    /// The `result` object, which ends every turn.
    pub fn as_result(&self) -> Option<&ResultEvent> {
        match self {
            Event::Result(line) => Some(&line.payload),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConfigSource {
    pub scope: &'static str,
    pub path: Option<String>,
    pub loaded: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Init {
    pub model: String,
    pub guardian_model: Option<String>,
    pub guardian_available: Option<bool>,
    pub guardian_profile: &'static str,
    pub permission_mode: &'static str,
    pub workspace_roots: Vec<String>,
    pub config_sources: Vec<ConfigSource>,
    pub workspace_trust: &'static str,
    pub project_executables: bool,
    pub suspended_allow_rules: Vec<String>,
    pub tools: Vec<String>,
}

/// An `assistant` or `assistant_delta` payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Text {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCall {
    pub tool_use_id: String,
    pub tool: String,
    pub args: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub tool: String,
    pub success: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelCall {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub calls_tool: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GateDecision {
    pub tool_use_id: String,
    pub agent_id: String,
    pub agent_name: Option<String>,
    pub tool: String,
    pub tier: &'static str,
    pub decision: &'static str,
    pub rule: Option<String>,
    pub explanation: Option<String>,
    pub confidence: Option<f64>,
    pub guardian_model: Option<String>,
    pub fail_closed_reason: Option<String>,
    pub screen_risk: Option<f64>,
    pub screen_allow_risk: Option<f64>,
    pub screen_steering: Option<f64>,
    pub screen_scores: Option<BTreeMap<String, f64>>,
    pub screen_outcome: Option<String>,
    pub afk: bool,
    pub converted: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GoalEvaluation {
    pub verdict: &'static str,
    pub reason: String,
    pub evaluations: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsagePaused {
    pub account: String,
    pub role: &'static str,
    pub reset_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageResumed {
    pub account: String,
    pub role: &'static str,
    pub end: &'static str,
    pub waited_secs: u64,
}

/// One model's tokens and cost, as the `usage` object of a `result` holds them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BucketUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cost_usd: Option<f64>,
}

impl From<&Bucket> for BucketUsage {
    fn from(bucket: &Bucket) -> BucketUsage {
        BucketUsage {
            input_tokens: bucket.input_tokens,
            output_tokens: bucket.output_tokens,
            cache_read_input_tokens: bucket.cache_read_input_tokens,
            cache_creation_input_tokens: bucket.cache_creation_input_tokens,
            cost_usd: bucket.cost_usd,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageByRole {
    pub main: BucketUsage,
    pub guardian: BucketUsage,
    pub subagents: BucketUsage,
    pub goal: BucketUsage,
}

/// How a goal stood when a message ended: the `goal` object of the `result` event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GoalReport {
    pub condition: String,
    pub status: &'static str,
    pub evaluations: u32,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResultEvent {
    pub subtype: &'static str,
    pub is_error: bool,
    pub result: String,
    pub error: Option<String>,
    pub num_turns: u64,
    pub duration_ms: u64,
    pub total_cost_usd: Option<f64>,
    pub usage: UsageByRole,
    pub denials: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub afk: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub account_usage: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_limit: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AfkReportEvent {
    pub afk: Value,
}

/// What a message's turn came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subtype {
    Success,
    MaxTurns,
    MaxBudget,
    Execution,
    UsageLimit,
}

impl Subtype {
    pub fn name(self) -> &'static str {
        match self {
            Subtype::Success => "success",
            Subtype::MaxTurns => "error_max_turns",
            Subtype::MaxBudget => "error_max_budget_usd",
            Subtype::Execution => "error_during_execution",
            Subtype::UsageLimit => "error_usage_limit",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub subtype: Subtype,
    pub text: String,
    pub structured: Option<Value>,
    pub error: Option<String>,
    pub started: std::time::Instant,
    /// Every call the pipeline denied during the turn.
    pub denials: Vec<Value>,
}

impl Outcome {
    /// A turn the host cut short.
    pub fn interrupted(started: std::time::Instant, denials: Vec<Value>) -> Outcome {
        Outcome {
            subtype: Subtype::Execution,
            text: String::new(),
            structured: None,
            error: Some("the turn was interrupted".to_string()),
            started,
            denials,
        }
    }
}

/// What the main model has cost, with the summarizer that keeps its context in bounds and the
/// advisor it consults.
pub fn main_usage(runtime: &AgentRuntime) -> Bucket {
    let ledger = runtime.state.usage_ledger();
    let mut main = ledger.bucket(UsageSource::Main);
    main.merge(&ledger.bucket(UsageSource::Compaction));
    main.merge(&ledger.bucket(UsageSource::Advisor));
    main
}

/// Session spend: the main model, the guardian, the goal evaluator and the subagents' models.
/// Null only when none is priced.
pub fn session_cost(
    main: Option<f64>,
    guardian: Option<f64>,
    goal: Option<f64>,
    subagents: Option<f64>,
) -> Option<f64> {
    match (main, guardian, goal, subagents) {
        (None, None, None, None) => None,
        (m, g, e, s) => {
            Some(m.unwrap_or(0.0) + g.unwrap_or(0.0) + e.unwrap_or(0.0) + s.unwrap_or(0.0))
        }
    }
}

/// What each model of the session has cost.
#[derive(Clone, Debug)]
pub struct Usage {
    pub main: Bucket,
    pub guardian: Bucket,
    pub goal: Bucket,
    pub subagents: Bucket,
}

impl Usage {
    pub fn of(runtime: &AgentRuntime) -> Usage {
        Usage {
            main: main_usage(runtime),
            guardian: runtime.state.guardian_usage(),
            goal: runtime.state.goal_usage(),
            subagents: runtime.state.subagent_usage(),
        }
    }
}

/// The `result` object of a message.
pub fn result_event(
    session_id: &str,
    outcome: &Outcome,
    usage: &Usage,
    goal: Option<&GoalReport>,
    account_usage: &[Value],
    afk: Option<&AfkReport>,
    usage_limit: Option<&Value>,
) -> Event {
    let (totals, guardian, subagents) = (&usage.main, &usage.guardian, &usage.subagents);
    Event::Result(Line::new(
        "result",
        session_id,
        ResultEvent {
            subtype: outcome.subtype.name(),
            is_error: outcome.subtype != Subtype::Success,
            result: outcome.text.clone(),
            error: outcome.error.clone(),
            num_turns: totals.calls,
            duration_ms: outcome.started.elapsed().as_millis() as u64,
            total_cost_usd: session_cost(
                totals.cost_usd,
                guardian.cost_usd,
                usage.goal.cost_usd,
                subagents.cost_usd,
            ),
            usage: UsageByRole {
                main: totals.into(),
                guardian: guardian.into(),
                subagents: subagents.into(),
                goal: (&usage.goal).into(),
            },
            denials: outcome.denials.clone(),
            structured_output: outcome.structured.clone(),
            goal: goal.cloned(),
            afk: afk.map(afk_object),
            account_usage: account_usage.to_vec(),
            usage_limit: usage_limit.cloned(),
        },
    ))
}

/// The `afk` object of the `result` and `afk_report` events.
pub fn afk_object(report: &AfkReport) -> Value {
    let temperature = &report.temperature;
    json!({
        "outcome": report.outcome.as_str(),
        "counts": report.counts,
        "temperature": {
            "current": temperature.current,
            "peak": temperature.peak,
            "ceiling": temperature.ceiling,
            "contributing": temperature.contributing,
        },
        "review": {
            "links": report.review.links,
            "assessment": report.review.assessment,
        },
        "timeline": report.timeline,
    })
}

/// The assessed report of a stream that ended after its last `result`.
pub fn afk_report_event(session_id: &str, report: &AfkReport) -> Event {
    Event::AfkReport(Line::new(
        "afk_report",
        session_id,
        AfkReportEvent {
            afk: afk_object(report),
        },
    ))
}

/// The `goal` event of one evaluation.
pub fn goal_event(
    session_id: &str,
    judgment: Option<Judgment>,
    reason: &str,
    evaluations: u32,
) -> Event {
    Event::Goal(Line::new(
        "goal",
        session_id,
        GoalEvaluation {
            verdict: judgment.map_or("failed", Judgment::as_str),
            reason: reason.to_string(),
            evaluations,
        },
    ))
}

pub fn epoch_secs(at: Option<std::time::SystemTime>) -> Option<u64> {
    at.and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// The account and reset time of a usage window that stayed spent, as `result.usage_limit`.
pub fn usage_limit_object(end: PauseEnd, info: &PauseInfo) -> Value {
    json!({
        "account": info.account,
        "role": info.role.as_str(),
        "reset_at": epoch_secs(info.reset_at),
        "end": end.as_str(),
    })
}

/// The `usage_paused` or `usage_resumed` event of a pause.
pub fn usage_event(session_id: &str, pause: &PauseEvent) -> Event {
    match pause {
        PauseEvent::Paused(info) => Event::UsagePaused(Line::new(
            "usage_paused",
            session_id,
            UsagePaused {
                account: info.account.clone(),
                role: info.role.as_str(),
                reset_at: epoch_secs(info.reset_at),
            },
        )),
        PauseEvent::Resumed { end, info, waited } => Event::UsageResumed(Line::new(
            "usage_resumed",
            session_id,
            UsageResumed {
                account: info.account.clone(),
                role: info.role.as_str(),
                end: end.as_str(),
                waited_secs: waited.as_secs(),
            },
        )),
    }
}

/// The `init` event, from what is in force, not what was configured: the guardian may be the
/// default pick or the main model, and the profile may have fallen back to `full`.
pub fn init_event(runtime: &AgentRuntime, data_dir: &Path) -> Event {
    let config = &runtime.config;
    let guardian = runtime.state.guardian();
    let (guardian_model, guardian_available) = if runtime.models.guardian.is_none() {
        (None, None)
    } else {
        (
            Some(guardian.label().to_string()),
            Some(guardian.unavailable_reason().is_none()),
        )
    };
    let trusted = trust::is_trusted(data_dir, &config.workspace);
    Event::Init(Line::new(
        "init",
        &runtime.session.id,
        Init {
            model: runtime.models.main.reference.to_string(),
            guardian_model,
            guardian_available,
            guardian_profile: runtime.guardian_profile().as_str(),
            permission_mode: runtime.start_mode().as_str(),
            workspace_roots: runtime
                .roots()
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
            config_sources: config
                .scopes
                .iter()
                .map(|scope| ConfigSource {
                    scope: scope.slot.label(),
                    path: scope.path.as_ref().map(|p| p.display().to_string()),
                    loaded: scope.loaded,
                })
                .collect(),
            workspace_trust: if trusted { "trusted" } else { "untrusted" },
            project_executables: runtime.project_executables(),
            suspended_allow_rules: if runtime.start_mode().rules() == PermissionMode::Auto {
                runtime.state.suspended_allow_rules()
            } else {
                Vec::new()
            },
            tools: runtime.tool_names(),
        },
    ))
}

pub fn gate_decision_event(session_id: &str, record: &DecisionRecord) -> Event {
    Event::GateDecision(Line::new(
        "gate_decision",
        session_id,
        GateDecision {
            tool_use_id: record.tool_use_id.clone().unwrap_or_default(),
            agent_id: record.agent_id.clone(),
            agent_name: record.agent_name.clone(),
            tool: record.tool.clone(),
            tier: record.tier.as_str(),
            decision: record.decision.as_str(),
            rule: record.rule.clone(),
            explanation: record.explanation.clone(),
            confidence: record.confidence,
            guardian_model: record.guardian_model.clone(),
            fail_closed_reason: record.fail_closed_reason.clone(),
            screen_risk: record.screen_risk,
            screen_allow_risk: record.screen_allow_risk,
            screen_steering: record.screen_steering,
            screen_scores: record.screen_scores.clone(),
            screen_outcome: record.screen_outcome.clone(),
            afk: record.afk,
            converted: record.converted.map(Converted::as_str),
        },
    ))
}

/// The event a session event becomes, or `None` for what the stream does not carry (reasoning and
/// diff previews, and `assistant_delta` unless `include_partial_messages`).
pub fn session_event(
    session_id: &str,
    event: &SessionEvent,
    include_partial_messages: bool,
) -> Option<Event> {
    Some(match event {
        SessionEvent::AssistantDelta(text) => {
            if !include_partial_messages {
                return None;
            }
            Event::AssistantDelta(Line::new(
                "assistant_delta",
                session_id,
                Text { text: text.clone() },
            ))
        }
        SessionEvent::Assistant(text) => Event::Assistant(Line::new(
            "assistant",
            session_id,
            Text { text: text.clone() },
        )),
        // Not in the closed stream-json schema; text output prints no reasoning either.
        SessionEvent::ReasoningDelta(_)
        | SessionEvent::Reasoning(_)
        | SessionEvent::ToolPreview { .. } => return None,
        SessionEvent::ToolCall { id, tool, args } => Event::ToolCall(Line::new(
            "tool_call",
            session_id,
            ToolCall {
                tool_use_id: id.clone(),
                tool: tool.clone(),
                args: args.clone(),
            },
        )),
        SessionEvent::Decision(record) => gate_decision_event(session_id, record),
        SessionEvent::ToolResult {
            id,
            tool,
            success,
            text,
        } => Event::ToolResult(Line::new(
            "tool_result",
            session_id,
            ToolResult {
                tool_use_id: id.clone(),
                tool: tool.clone(),
                success: *success,
                text: text.clone(),
            },
        )),
        SessionEvent::Usage { usage, calls_tool } => Event::ModelCall(Line::new(
            "model_call",
            session_id,
            ModelCall {
                input_tokens: usage.input_tokens.unwrap_or(0),
                output_tokens: usage.output_tokens.unwrap_or(0),
                cache_read_input_tokens: usage.cached_input_tokens.unwrap_or(0),
                cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
                calls_tool: *calls_tool,
            },
        )),
    })
}

/// Writes one line to stdout. A closed pipe is the consumer's decision; the run's exit code is
/// unaffected.
pub fn write_stdout_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::audit::Decision;
    use crate::core::quota::UsageRole;
    use crate::core::tier::Tier;

    const SID: &str = "sid-1";

    fn json_of(event: &Event) -> String {
        serde_json::to_string(event).expect("an event serializes")
    }

    fn bucket(n: u64, cost: Option<f64>) -> Bucket {
        Bucket {
            calls: n,
            input_tokens: n * 10,
            output_tokens: n * 5,
            cache_read_input_tokens: n * 2,
            cache_creation_input_tokens: n,
            cost_usd: cost,
            ..Bucket::default()
        }
    }

    fn usage() -> Usage {
        Usage {
            main: bucket(3, Some(0.5)),
            guardian: bucket(1, Some(0.25)),
            goal: bucket(1, None),
            subagents: bucket(2, Some(0.125)),
        }
    }

    fn outcome(subtype: Subtype) -> Outcome {
        Outcome {
            subtype,
            text: "done".into(),
            structured: None,
            error: None,
            started: std::time::Instant::now(),
            denials: Vec::new(),
        }
    }

    // The literals below were captured from the `json!` builders these types replaced.
    #[test]
    fn simple_events_serialize_to_the_literal_lines_of_the_old_builders() {
        let call = SessionEvent::ToolCall {
            id: "t1".into(),
            tool: "shell".into(),
            args: json!({"command": "ls"}),
        };
        let result = SessionEvent::ToolResult {
            id: "t1".into(),
            tool: "shell".into(),
            success: true,
            text: "a\nb".into(),
        };
        let mut usage = rig_core::completion::Usage::new();
        usage.input_tokens = Some(10);
        usage.output_tokens = Some(5);
        usage.cached_input_tokens = Some(3);
        usage.cache_creation_input_tokens = Some(2);
        let model = SessionEvent::Usage {
            usage,
            calls_tool: true,
        };
        let cases = [
            (
                SessionEvent::Assistant("hello".into()),
                r#"{"text":"hello","type":"assistant","schema_version":1,"session_id":"sid-1"}"#,
            ),
            (
                SessionEvent::AssistantDelta("hel".into()),
                r#"{"text":"hel","type":"assistant_delta","schema_version":1,"session_id":"sid-1"}"#,
            ),
            (
                call,
                r#"{"tool_use_id":"t1","tool":"shell","args":{"command":"ls"},"type":"tool_call","schema_version":1,"session_id":"sid-1"}"#,
            ),
            (
                result,
                r#"{"tool_use_id":"t1","tool":"shell","success":true,"text":"a\nb","type":"tool_result","schema_version":1,"session_id":"sid-1"}"#,
            ),
            (
                model,
                r#"{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":3,"cache_creation_input_tokens":2,"calls_tool":true,"type":"model_call","schema_version":1,"session_id":"sid-1"}"#,
            ),
        ];
        for (event, expected) in cases {
            let event = session_event(SID, &event, true).expect("the stream carries it");
            assert_eq!(json_of(&event), expected);
        }
    }

    #[test]
    fn assistant_delta_needs_include_partial_messages_and_reasoning_never_shows() {
        let delta = SessionEvent::AssistantDelta("x".into());
        assert!(session_event(SID, &delta, false).is_none());
        assert!(session_event(SID, &delta, true).is_some());
        let reasoning = SessionEvent::Reasoning("r".into());
        assert!(session_event(SID, &reasoning, true).is_none());
    }

    #[test]
    fn gate_decisions_serialize_to_the_literal_lines_of_the_old_builder() {
        let mut record = DecisionRecord::new(
            SID.into(),
            "root".into(),
            "shell".into(),
            "dg".into(),
            Tier::DenyRule,
            Decision::Deny,
            "auto".into(),
            true,
        );
        record.tool_use_id = Some("t1".into());
        record.rule = Some("deny: shell(rm:*)".into());
        record.explanation = Some("no".into());
        record.confidence = Some(0.5);
        record.guardian_model = Some("a/g".into());
        record.fail_closed_reason = Some("timeout".into());
        record.screen_risk = Some(0.25);
        record.screen_allow_risk = Some(0.125);
        record.screen_steering = Some(0.0625);
        record.screen_scores = Some(BTreeMap::from([("x".to_string(), 0.5)]));
        record.screen_outcome = Some("stop".into());
        record.afk = true;
        record.converted = Some(Converted::Floor);
        assert_eq!(
            json_of(&gate_decision_event(SID, &record)),
            r#"{"tool_use_id":"t1","agent_id":"root","agent_name":null,"tool":"shell","tier":"deny_rule","decision":"deny","rule":"deny: shell(rm:*)","explanation":"no","confidence":0.5,"guardian_model":"a/g","fail_closed_reason":"timeout","screen_risk":0.25,"screen_allow_risk":0.125,"screen_steering":0.0625,"screen_scores":{"x":0.5},"screen_outcome":"stop","afk":true,"converted":"floor","type":"gate_decision","schema_version":1,"session_id":"sid-1"}"#
        );
        let bare = DecisionRecord::new(
            SID.into(),
            "root".into(),
            "read".into(),
            "dg".into(),
            Tier::DenyRule,
            Decision::Allow,
            "auto".into(),
            true,
        );
        assert_eq!(
            json_of(&gate_decision_event(SID, &bare)),
            r#"{"tool_use_id":"","agent_id":"root","agent_name":null,"tool":"read","tier":"deny_rule","decision":"allow","rule":null,"explanation":null,"confidence":null,"guardian_model":null,"fail_closed_reason":null,"screen_risk":null,"screen_allow_risk":null,"screen_steering":null,"screen_scores":null,"screen_outcome":null,"afk":false,"converted":null,"type":"gate_decision","schema_version":1,"session_id":"sid-1"}"#
        );
    }

    #[test]
    fn goal_and_usage_events_serialize_to_the_literal_lines_of_the_old_builders() {
        assert_eq!(
            json_of(&goal_event(SID, Some(Judgment::NotMet), "why", 2)),
            r#"{"verdict":"not_met","reason":"why","evaluations":2,"type":"goal","schema_version":1,"session_id":"sid-1"}"#
        );
        assert_eq!(
            json_of(&goal_event(SID, None, "why", 3)),
            r#"{"verdict":"failed","reason":"why","evaluations":3,"type":"goal","schema_version":1,"session_id":"sid-1"}"#
        );
        let info = PauseInfo::new(
            UsageRole::Main,
            "acct",
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000)),
        );
        assert_eq!(
            json_of(&usage_event(SID, &PauseEvent::Paused(info.clone()))),
            r#"{"account":"acct","role":"main","reset_at":1000,"type":"usage_paused","schema_version":1,"session_id":"sid-1"}"#
        );
        let resumed = PauseEvent::Resumed {
            end: PauseEnd::Reset,
            info,
            waited: std::time::Duration::from_secs(7),
        };
        assert_eq!(
            json_of(&usage_event(SID, &resumed)),
            r#"{"account":"acct","role":"main","end":"reset","waited_secs":7,"type":"usage_resumed","schema_version":1,"session_id":"sid-1"}"#
        );
        let unknown_reset = PauseInfo::new(UsageRole::Guardian, "g", None);
        assert_eq!(
            json_of(&usage_event(SID, &PauseEvent::Paused(unknown_reset))),
            r#"{"account":"g","role":"guardian","reset_at":null,"type":"usage_paused","schema_version":1,"session_id":"sid-1"}"#
        );
    }

    #[test]
    fn results_serialize_to_the_literal_lines_of_the_old_builder() {
        let mut done = outcome(Subtype::Success);
        let plain = result_event(SID, &done, &usage(), None, &[], None, None);
        // `duration_ms` is the one live value.
        let plain = json_of(&plain).replacen(
            &format!("\"duration_ms\":{}", done.started.elapsed().as_millis()),
            "\"duration_ms\":0",
            1,
        );
        assert_eq!(
            plain,
            r#"{"subtype":"success","is_error":false,"result":"done","error":null,"num_turns":3,"duration_ms":0,"total_cost_usd":0.875,"usage":{"main":{"input_tokens":30,"output_tokens":15,"cache_read_input_tokens":6,"cache_creation_input_tokens":3,"cost_usd":0.5},"guardian":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"cost_usd":0.25},"subagents":{"input_tokens":20,"output_tokens":10,"cache_read_input_tokens":4,"cache_creation_input_tokens":2,"cost_usd":0.125},"goal":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"cost_usd":null}},"denials":[],"type":"result","schema_version":1,"session_id":"sid-1"}"#
        );

        done.subtype = Subtype::MaxTurns;
        done.error = Some("too many".into());
        done.structured = Some(json!({"a": 1}));
        done.denials =
            vec![json!({"tool": "shell", "tier": "deny_rule", "rule": "r", "explanation": "e"})];
        let goal = GoalReport {
            condition: "c".into(),
            status: "met",
            evaluations: 2,
            reason: Some("r".into()),
        };
        let limit = json!({"account": "a", "role": "main", "reset_at": 5, "end": "reset"});
        let full = result_event(
            SID,
            &done,
            &usage(),
            Some(&goal),
            &[json!({"role": "main", "account": "a", "label": "l"})],
            None,
            Some(&limit),
        );
        let full: Value = serde_json::from_str(&json_of(&full)).unwrap();
        let keys: Vec<&str> = full
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            &keys[keys.len() - 8..],
            [
                "denials",
                "structured_output",
                "goal",
                "account_usage",
                "usage_limit",
                "type",
                "schema_version",
                "session_id",
            ]
        );
        assert_eq!(full["subtype"], "error_max_turns");
        assert_eq!(full["goal"]["status"], "met");
        assert_eq!(full["usage_limit"]["end"], "reset");
    }

    #[test]
    fn afk_report_events_serialize_to_the_literal_line_of_the_old_builder() {
        use crate::core::afk_report::*;
        let report = AfkReport {
            stretch: Stretch {
                started_seq: 1,
                started_ts_ms: 2,
                ended_seq: None,
                ended_ts_ms: None,
                source: AfkSource::Cli,
            },
            outcome: AfkOutcome::Ended,
            counts: Counts {
                allowed: 1,
                denied: 2,
                converted: 1,
                succeeded: 1,
                failed: 0,
                cancelled: 0,
                unknown: 0,
            },
            temperature: TemperatureReport {
                current: 0.5,
                peak: 1.5,
                peak_seq: Some(3),
                ceiling: 4.0,
                contributing: vec![ContributingDenial {
                    seq: 3,
                    weight: 1.0,
                    remaining: 0.5,
                }],
            },
            timeline: vec![],
            review: SecurityReview {
                links: vec![],
                assessment: Assessment::Unavailable { reason: "x".into() },
            },
        };
        assert_eq!(
            json_of(&afk_report_event(SID, &report)),
            r#"{"afk":{"outcome":"ended","counts":{"allowed":1,"denied":2,"converted":1,"succeeded":1,"failed":0,"cancelled":0,"unknown":0},"temperature":{"current":0.5,"peak":1.5,"ceiling":4.0,"contributing":[{"seq":3,"weight":1.0,"remaining":0.5}]},"review":{"links":[],"assessment":{"status":"unavailable","reason":"x"}},"timeline":[]},"type":"afk_report","schema_version":1,"session_id":"sid-1"}"#
        );
    }

    #[test]
    fn init_serializes_its_payload_first_and_the_envelope_last() {
        let init = Event::Init(Line::new(
            "init",
            SID,
            Init {
                model: "a/m".into(),
                guardian_model: None,
                guardian_available: None,
                guardian_profile: "full",
                permission_mode: "manual",
                workspace_roots: vec!["/w".into()],
                config_sources: vec![ConfigSource {
                    scope: "user",
                    path: Some("/c/settings.toml".into()),
                    loaded: true,
                }],
                workspace_trust: "trusted",
                project_executables: true,
                suspended_allow_rules: Vec::new(),
                tools: vec!["read".into()],
            },
        ));
        assert_eq!(
            json_of(&init),
            r#"{"model":"a/m","guardian_model":null,"guardian_available":null,"guardian_profile":"full","permission_mode":"manual","workspace_roots":["/w"],"config_sources":[{"scope":"user","path":"/c/settings.toml","loaded":true}],"workspace_trust":"trusted","project_executables":true,"suspended_allow_rules":[],"tools":["read"],"type":"init","schema_version":1,"session_id":"sid-1"}"#
        );
    }

    #[test]
    fn session_cost_adds_the_guardian_and_subagents_and_is_null_only_when_none_is_priced() {
        assert_eq!(session_cost(None, None, None, None), None);
        assert_eq!(session_cost(Some(0.5), None, None, None), Some(0.5));
        assert_eq!(session_cost(None, Some(0.25), None, None), Some(0.25));
        assert_eq!(session_cost(Some(0.5), Some(0.25), None, None), Some(0.75));
        assert_eq!(session_cost(None, None, None, Some(0.125)), Some(0.125));
        assert_eq!(session_cost(None, None, Some(0.0625), None), Some(0.0625));
        assert_eq!(
            session_cost(Some(0.5), Some(0.25), Some(0.0625), Some(0.125)),
            Some(0.9375)
        );
    }

    #[test]
    fn cache_tokens_reach_the_result_usage() {
        use crate::core::usage_ledger::UsageLedger;
        let ledger = UsageLedger::default();
        for (read, written) in [(Some(800), Some(100)), (Some(900), None), (None, None)] {
            let mut usage = rig_core::completion::Usage::new();
            usage.input_tokens = Some(1000);
            usage.cached_input_tokens = read;
            usage.cache_creation_input_tokens = written;
            ledger.record(UsageSource::Main, &usage, None, true, &Default::default());
        }
        let bucket = |read, written| Bucket {
            cache_read_input_tokens: read,
            cache_creation_input_tokens: written,
            ..Bucket::default()
        };
        let usage = Usage {
            main: ledger.bucket(UsageSource::Main),
            guardian: bucket(10, 11),
            goal: bucket(30, 31),
            subagents: bucket(20, 21),
        };
        let event = result_event(
            SID,
            &outcome(Subtype::Success),
            &usage,
            None,
            &[],
            None,
            None,
        );
        let result: Value = serde_json::from_str(&json_of(&event)).unwrap();
        assert_eq!(result["num_turns"], 3);
        assert_eq!(result["usage"]["main"]["input_tokens"], 3000);
        assert_eq!(result["usage"]["main"]["cache_read_input_tokens"], 1700);
        assert_eq!(result["usage"]["main"]["cache_creation_input_tokens"], 100);
        for (name, read, written) in [
            ("guardian", 10, 11),
            ("subagents", 20, 21),
            ("goal", 30, 31),
        ] {
            let bucket = &result["usage"][name];
            assert_eq!(bucket["cache_read_input_tokens"], read, "{name}");
            assert_eq!(bucket["cache_creation_input_tokens"], written, "{name}");
        }
    }
}
