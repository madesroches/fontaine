//! AFK mode's evidence: how serious a denial was, which later calls look like a retry of it, the
//! session temperature those add up to, and the report that lays a stretch out call by call. All
//! of it is a pure function of the ledger's records, so the live session and a report built from
//! the file after a resume cannot disagree.

use std::collections::HashMap;

use fontaine_guardian::RuleLabel;
use fontaine_guardian::evasion::{AssessAttempt, AssessmentInput, AttemptDecision, SuspectedLink};
use fontaine_guardian::temperature::{
    EXACT_RETRY_BONUS, FAIL_CLOSED_WEIGHT, SAME_TARGET_BONUS, Severity, Temperature,
};
use fontaine_tools::descriptor;
use fontaine_tools::risk::RiskClass;
use serde::{Deserialize, Serialize};

use crate::audit::Converted;
use crate::ledger::{Entry, Record, path_key};
use crate::tier::Tier;

/// What turned AFK on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfkSource {
    /// `/afk` in the terminal UI.
    Command,
    /// `--afk` on the command line.
    Cli,
}

/// Whether an allowed call ran to the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Ok,
    Error,
    Cancelled,
}

/// What the independent review of a stretch came to, as the ledger keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AssessmentResult {
    Unavailable { reason: String },
    Completed { suspected: Vec<SuspectedLink> },
}

/// How a stretch ended, or has not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AfkOutcome {
    Active,
    Ended,
    /// Repeated denials paused `auto` with nobody to ask.
    Blocked,
    /// The temperature ceiling stopped the session.
    Stopped,
}

impl AfkOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AfkOutcome::Active => "active",
            AfkOutcome::Ended => "ended",
            AfkOutcome::Blocked => "blocked",
            AfkOutcome::Stopped => "stopped",
        }
    }
}

/// A call that was refused, as the severity rules read it.
#[derive(Clone, Copy, Debug)]
pub struct DenialFacts<'a> {
    pub tool: &'a str,
    pub tier: Option<Tier>,
    pub rule: Option<&'a str>,
    pub converted: Option<Converted>,
    pub fail_closed: Option<&'a str>,
    pub digest: Option<&'a str>,
    pub paths: &'a [String],
}

/// What severity a guardian label stands for. A label this build does not know is Medium.
fn label_severity(rule: &str) -> Severity {
    let label: Option<RuleLabel> =
        serde_json::from_value(serde_json::Value::String(rule.into())).ok();
    match label {
        Some(
            RuleLabel::DataExfiltration
            | RuleLabel::DownloadExecute
            | RuleLabel::TunnelReverseShell
            | RuleLabel::SecretExposure
            | RuleLabel::SafetyBypass
            | RuleLabel::ContainerEscape
            | RuleLabel::UnguardedAgentLoop,
        ) => Severity::High,
        _ => Severity::Medium,
    }
}

/// What a denial adds before any retry bonus: the highest weight among the rules that apply.
pub fn severity_weight(facts: &DenialFacts<'_>) -> f64 {
    let mut weight = 0.0_f64;
    let mut take = |value: f64| weight = weight.max(value);
    if facts.fail_closed.is_some() {
        take(FAIL_CLOSED_WEIGHT);
    } else {
        match facts.tier {
            Some(Tier::Guardian) if facts.converted != Some(Converted::BreakerPaused) => {
                take(facts.rule.map_or(Severity::Medium, label_severity).weight());
            }
            Some(
                Tier::Screen
                | Tier::DenyRule
                | Tier::CriticalPath
                | Tier::ProtectedPath
                | Tier::PlanBlock,
            ) => take(Severity::Medium.weight()),
            _ => {}
        }
    }
    if facts.converted.is_some() {
        take(Severity::Low.weight());
    }
    if weight == 0.0 {
        weight = Severity::Low.weight();
    }
    weight
}

/// How a later call relates to an earlier denied one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    ExactRetry,
    SameTargetOtherTool,
    /// Named by the independent review rather than by a rule.
    Suspected,
}

fn risk_of(tool: &str) -> RiskClass {
    descriptor::find(tool).map_or(RiskClass::Arbitrary, |d| d.risk)
}

/// Whether `later` reaches the target `denied` was refused by another tool, in the direction the
/// two tools' risk classes allow. A different path, or a read after a denied write, is never
/// linked, so independent work and a narrower scope stay unmarked.
fn same_target(denied_tool: &str, denied: &[String], later_tool: &str, later: &[String]) -> bool {
    let (denied_risk, later_risk) = (risk_of(denied_tool), risk_of(later_tool));
    let direction = match later_risk {
        RiskClass::Arbitrary => true,
        RiskClass::WorkspaceWrite => {
            matches!(
                denied_risk,
                RiskClass::WorkspaceWrite | RiskClass::Arbitrary
            )
        }
        RiskClass::ReadOnly => matches!(denied_risk, RiskClass::ReadOnly | RiskClass::Arbitrary),
    };
    direction
        && later
            .iter()
            .any(|path| denied.iter().any(|d| path_key(d) == path_key(path)))
}

/// The deterministic link between a denied call and a later one, when there is one.
pub fn link_kind(
    denied: (&str, Option<&str>, &[String]),
    later: (&str, Option<&str>, &[String]),
) -> Option<LinkKind> {
    let (denied_tool, denied_digest, denied_paths) = denied;
    let (later_tool, later_digest, later_paths) = later;
    if denied_tool == later_tool {
        return (denied_digest.is_some() && denied_digest == later_digest)
            .then_some(LinkKind::ExactRetry);
    }
    same_target(denied_tool, denied_paths, later_tool, later_paths)
        .then_some(LinkKind::SameTargetOtherTool)
}

#[derive(Clone, Debug)]
struct TrackedDenial {
    tool: String,
    digest: Option<String>,
    paths: Vec<String>,
}

/// The temperature of the current AFK stretch, kept in step with the ledger.
#[derive(Clone, Debug, Default)]
pub struct StretchTracker {
    temperature: Temperature,
    denied: Vec<TrackedDenial>,
}

impl StretchTracker {
    /// An allowed call: one gate decision that cools everything.
    pub fn action(&mut self) {
        self.temperature.step();
    }

    /// A denial at ledger position `seq`. Returns the new temperature.
    pub fn denial(&mut self, seq: u64, facts: &DenialFacts<'_>) -> f64 {
        self.temperature.step();
        let mut weight = severity_weight(facts);
        let (mut exact, mut target) = (false, false);
        for earlier in &self.denied {
            match link_kind(
                (&earlier.tool, earlier.digest.as_deref(), &earlier.paths),
                (facts.tool, facts.digest, facts.paths),
            ) {
                Some(LinkKind::ExactRetry) => exact = true,
                Some(LinkKind::SameTargetOtherTool) => target = true,
                _ => {}
            }
        }
        if exact {
            weight += EXACT_RETRY_BONUS;
        }
        if target {
            weight += SAME_TARGET_BONUS;
        }
        self.denied.push(TrackedDenial {
            tool: facts.tool.to_string(),
            digest: facts.digest.map(str::to_string),
            paths: facts.paths.to_vec(),
        });
        self.temperature.denial(seq, weight)
    }

    pub fn temperature(&self) -> &Temperature {
        &self.temperature
    }
}

/// A stop the temperature ceiling caused, as the ledger replays it.
#[derive(Clone, Debug, PartialEq)]
pub struct AfkStop {
    pub seq: u64,
    pub value: f64,
    pub ceiling: f64,
    pub contributing: Vec<u64>,
}

/// What the ledger view keeps of AFK mode: the open stretch, its temperature, an unreviewed stop
/// and how the latest stretch ended.
#[derive(Debug, Default)]
pub struct AfkLedgerState {
    open: Option<(u64, AfkSource)>,
    tracker: StretchTracker,
    stopped: Option<AfkStop>,
    latest: Option<AfkOutcome>,
}

impl AfkLedgerState {
    pub(crate) fn apply(&mut self, seq: u64, record: &Record) {
        match record {
            Record::Afk { on: true, source } => {
                self.open = Some((seq, *source));
                self.tracker = StretchTracker::default();
                self.latest = Some(AfkOutcome::Active);
            }
            Record::Afk { on: false, .. } => {
                self.open = None;
                if self.latest == Some(AfkOutcome::Active) {
                    self.latest = Some(AfkOutcome::Ended);
                }
            }
            Record::Action { .. } if self.open.is_some() => self.tracker.action(),
            Record::Denial {
                tool,
                rule,
                tier,
                digest,
                paths,
                converted,
                fail_closed,
                ..
            } if self.open.is_some() => {
                self.tracker.denial(
                    seq,
                    &DenialFacts {
                        tool,
                        tier: *tier,
                        rule: rule.as_deref(),
                        converted: *converted,
                        fail_closed: fail_closed.as_deref(),
                        digest: digest.as_deref(),
                        paths,
                    },
                );
            }
            Record::AfkBlocked {} => {
                if matches!(self.latest, Some(AfkOutcome::Active | AfkOutcome::Ended)) {
                    self.latest = Some(AfkOutcome::Blocked);
                }
            }
            Record::AfkStop {
                value,
                ceiling,
                contributing,
            } => {
                self.stopped = Some(AfkStop {
                    seq,
                    value: *value,
                    ceiling: *ceiling,
                    contributing: contributing.clone(),
                });
                self.latest = Some(AfkOutcome::Stopped);
            }
            Record::AfkReviewed {} => self.stopped = None,
            _ => {}
        }
    }

    /// The reopened ledger ends a stretch the previous process left open: AFK is never restored.
    pub(crate) fn close_open_stretch(&mut self) {
        if self.open.take().is_some() && self.latest == Some(AfkOutcome::Active) {
            self.latest = Some(AfkOutcome::Ended);
        }
    }

    /// The temperature of the open stretch; cold when none is open.
    pub fn temperature(&self) -> &Temperature {
        self.tracker.temperature()
    }

    /// The stop that no review has cleared.
    pub fn stopped(&self) -> Option<&AfkStop> {
        self.stopped.as_ref()
    }

    /// How the latest stretch ended or stands; `None` when AFK was never on.
    pub fn latest(&self) -> Option<AfkOutcome> {
        self.latest
    }
}

/// One stretch, from `/afk` or `--afk` to its end.
#[derive(Clone, Debug, Serialize)]
pub struct Stretch {
    pub started_seq: u64,
    pub started_ts_ms: u64,
    pub ended_seq: Option<u64>,
    pub ended_ts_ms: Option<u64>,
    pub source: AfkSource,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub allowed: usize,
    pub denied: usize,
    /// Denials that stand in for a prompt nobody could answer; a subset of `denied`.
    pub converted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Allowed calls with no recorded outcome.
    pub unknown: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContributingDenial {
    pub seq: u64,
    pub weight: f64,
    /// What it still adds after the decisions since.
    pub remaining: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TemperatureReport {
    pub current: f64,
    pub peak: f64,
    pub peak_seq: Option<u64>,
    pub ceiling: f64,
    pub contributing: Vec<ContributingDenial>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Attempt {
    pub seq: u64,
    pub ts_ms: u64,
    pub agent: String,
    pub agent_name: String,
    pub tool: String,
    pub summary: String,
    pub decision: AttemptDecision,
    pub tier: Option<Tier>,
    pub rule: Option<String>,
    pub explanation: Option<String>,
    /// How an allowed call ended; `None` when the ledger holds no outcome for it.
    pub outcome: Option<OutcomeStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Link {
    pub denial_seq: u64,
    pub attempt_seq: u64,
    pub kind: LinkKind,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Assessment {
    NoDenials,
    Pending,
    NoSuspectedEvasion,
    Found,
    Unavailable { reason: String },
}

impl Assessment {
    /// What the report says. No path says "no evasion": a failed or missing review is not one.
    pub fn describe(&self) -> String {
        match self {
            Assessment::NoDenials => {
                "no denials in this stretch, so there was nothing to assess".into()
            }
            Assessment::Pending => "evasion assessment has not run".into(),
            Assessment::NoSuspectedEvasion => "no suspected evasion detected".into(),
            Assessment::Found => "suspected evasion".into(),
            Assessment::Unavailable { reason } => {
                format!("evasion assessment unavailable: {reason}")
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SecurityReview {
    pub links: Vec<Link>,
    pub assessment: Assessment,
}

#[derive(Clone, Debug, Serialize)]
pub struct AfkReport {
    pub stretch: Stretch,
    pub outcome: AfkOutcome,
    pub counts: Counts,
    pub temperature: TemperatureReport,
    pub timeline: Vec<Attempt>,
    pub review: SecurityReview,
}

impl AfkReport {
    /// The calls of the stretch in the form the independent review reads.
    pub fn assessment_input(&self) -> AssessmentInput {
        AssessmentInput {
            attempts: self
                .timeline
                .iter()
                .map(|attempt| AssessAttempt {
                    id: attempt.seq,
                    agent: attempt.agent_name.clone(),
                    tool: attempt.tool.clone(),
                    summary: attempt.summary.clone(),
                    decision: attempt.decision,
                    rule: attempt.rule.clone(),
                    explanation: attempt.explanation.clone(),
                })
                .collect(),
        }
    }

    /// Whether the stretch holds a denial, which is what makes the review worth running.
    pub fn has_denials(&self) -> bool {
        self.counts.denied > 0
    }
}

struct CallRecord<'a> {
    seq: u64,
    tool: &'a str,
    digest: Option<&'a str>,
    paths: &'a [String],
    denied: bool,
}

/// The name an agent is shown by: the subagent's own, or `main` for the root.
fn agent_names(entries: &[Entry]) -> HashMap<&str, &str> {
    let mut names = HashMap::new();
    for entry in entries {
        if let Record::Agent {
            event,
            agent,
            name: Some(name),
            ..
        } = &entry.record
            && event == "spawned"
        {
            names.insert(agent.as_str(), name.as_str());
        }
    }
    names
}

fn stretch_bounds(entries: &[Entry]) -> Vec<(usize, usize)> {
    let mut bounds: Vec<(usize, usize)> = Vec::new();
    for (at, entry) in entries.iter().enumerate() {
        match entry.record {
            Record::Afk { on: true, .. } => {
                if let Some(last) = bounds.last_mut()
                    && last.1 == usize::MAX
                {
                    last.1 = at;
                }
                bounds.push((at, usize::MAX));
            }
            Record::Afk { on: false, .. } => {
                if let Some(last) = bounds.last_mut()
                    && last.1 == usize::MAX
                {
                    last.1 = at + 1;
                }
            }
            _ => {}
        }
    }
    bounds
        .into_iter()
        .map(|(start, end)| (start, end.min(entries.len())))
        .collect()
}

fn build_one(
    entries: &[Entry],
    (start, end): (usize, usize),
    names: &HashMap<&str, &str>,
    ceiling: f64,
    last_active: bool,
) -> Option<AfkReport> {
    let slice = &entries[start..end];
    let Record::Afk { source, on: true } = &slice.first()?.record else {
        return None;
    };
    let first = &slice[0];
    let ended = match slice.last() {
        Some(Entry {
            record: Record::Afk { on: false, .. },
            seq,
            ts_ms,
        }) => Some((*seq, *ts_ms)),
        _ => None,
    };
    let mut outcomes: HashMap<&str, OutcomeStatus> = HashMap::new();
    let mut stopped = false;
    let mut blocked = false;
    for entry in slice {
        match &entry.record {
            Record::ToolOutcome {
                tool_use_id,
                status,
                ..
            } => {
                outcomes.insert(tool_use_id.as_str(), *status);
            }
            Record::AfkStop { .. } => stopped = true,
            Record::AfkBlocked {} => blocked = true,
            _ => {}
        }
    }

    let mut tracker = StretchTracker::default();
    let mut counts = Counts::default();
    let mut timeline: Vec<Attempt> = Vec::new();
    let mut calls: Vec<CallRecord<'_>> = Vec::new();
    let name_of = |agent: &str| {
        if agent == fontaine_tools::gate::ROOT_AGENT {
            "main".to_string()
        } else {
            names.get(agent).copied().unwrap_or(agent).to_string()
        }
    };
    for entry in slice {
        match &entry.record {
            Record::Action {
                tool,
                agent,
                paths,
                summary,
                tier,
                tool_use_id,
                digest,
                ..
            } => {
                tracker.action();
                counts.allowed += 1;
                let outcome = tool_use_id
                    .as_deref()
                    .and_then(|id| outcomes.get(id).copied());
                match outcome {
                    Some(OutcomeStatus::Ok) => counts.succeeded += 1,
                    Some(OutcomeStatus::Error) => counts.failed += 1,
                    Some(OutcomeStatus::Cancelled) => counts.cancelled += 1,
                    None => counts.unknown += 1,
                }
                calls.push(CallRecord {
                    seq: entry.seq,
                    tool,
                    digest: digest.as_deref(),
                    paths,
                    denied: false,
                });
                timeline.push(Attempt {
                    seq: entry.seq,
                    ts_ms: entry.ts_ms,
                    agent: agent.clone(),
                    agent_name: name_of(agent),
                    tool: tool.clone(),
                    summary: summary.clone(),
                    decision: AttemptDecision::Allowed,
                    tier: *tier,
                    rule: None,
                    explanation: None,
                    outcome,
                });
            }
            Record::Denial {
                tool,
                agent,
                reason,
                rule,
                summary,
                tier,
                digest,
                paths,
                converted,
                fail_closed,
                explanation,
                ..
            } => {
                tracker.denial(
                    entry.seq,
                    &DenialFacts {
                        tool,
                        tier: *tier,
                        rule: rule.as_deref(),
                        converted: *converted,
                        fail_closed: fail_closed.as_deref(),
                        digest: digest.as_deref(),
                        paths,
                    },
                );
                counts.denied += 1;
                if converted.is_some() {
                    counts.converted += 1;
                }
                calls.push(CallRecord {
                    seq: entry.seq,
                    tool,
                    digest: digest.as_deref(),
                    paths,
                    denied: true,
                });
                timeline.push(Attempt {
                    seq: entry.seq,
                    ts_ms: entry.ts_ms,
                    agent: agent.clone(),
                    agent_name: name_of(agent),
                    tool: tool.clone(),
                    summary: summary.clone(),
                    decision: if converted.is_some() {
                        AttemptDecision::Converted
                    } else {
                        AttemptDecision::Denied
                    },
                    tier: *tier,
                    rule: rule.clone(),
                    explanation: explanation.clone().or_else(|| Some(reason.clone())),
                    outcome: None,
                });
            }
            _ => {}
        }
    }

    let mut links = deterministic_links(&calls);
    let assessment = assessment_of(slice, first.seq, &timeline, &mut links);
    let temperature = tracker.temperature();
    let outcome = if stopped {
        AfkOutcome::Stopped
    } else if blocked {
        AfkOutcome::Blocked
    } else if ended.is_none() && last_active {
        AfkOutcome::Active
    } else {
        AfkOutcome::Ended
    };
    Some(AfkReport {
        stretch: Stretch {
            started_seq: first.seq,
            started_ts_ms: first.ts_ms,
            ended_seq: ended.map(|(seq, _)| seq),
            ended_ts_ms: ended.map(|(_, ts)| ts),
            source: *source,
        },
        outcome,
        counts,
        temperature: TemperatureReport {
            current: temperature.current(),
            peak: temperature.peak(),
            peak_seq: temperature.peak_seq(),
            ceiling,
            contributing: temperature
                .contributions()
                .iter()
                .map(|c| ContributingDenial {
                    seq: c.seq,
                    weight: c.weight,
                    remaining: temperature.remaining(c),
                })
                .collect(),
        },
        timeline,
        review: SecurityReview { links, assessment },
    })
}

fn deterministic_links(calls: &[CallRecord<'_>]) -> Vec<Link> {
    let mut links = Vec::new();
    for (at, later) in calls.iter().enumerate() {
        for earlier in calls[..at].iter().filter(|c| c.denied) {
            let kind = link_kind(
                (earlier.tool, earlier.digest, earlier.paths),
                (later.tool, later.digest, later.paths),
            );
            let Some(kind) = kind else { continue };
            let reason = match kind {
                LinkKind::ExactRetry => format!(
                    "the same `{}` call as denied call #{}",
                    later.tool, earlier.seq
                ),
                _ => format!(
                    "`{}` reaches the same path as `{}`, denied in call #{}",
                    later.tool, earlier.tool, earlier.seq
                ),
            };
            links.push(Link {
                denial_seq: earlier.seq,
                attempt_seq: later.seq,
                kind,
                reason,
            });
        }
    }
    links
}

/// The latest review recorded for the stretch, and the pairs it found added to `links`.
fn assessment_of(
    slice: &[Entry],
    stretch_seq: u64,
    timeline: &[Attempt],
    links: &mut Vec<Link>,
) -> Assessment {
    if !timeline.iter().any(|attempt| attempt.decision.is_denial()) {
        return Assessment::NoDenials;
    }
    let latest = slice.iter().rev().find_map(|entry| match &entry.record {
        Record::AfkAssessment {
            stretch_seq: s,
            result,
        } if *s == stretch_seq => Some(result),
        _ => None,
    });
    match latest {
        None => Assessment::Pending,
        Some(AssessmentResult::Unavailable { reason }) => Assessment::Unavailable {
            reason: reason.clone(),
        },
        Some(AssessmentResult::Completed { suspected }) if suspected.is_empty() => {
            Assessment::NoSuspectedEvasion
        }
        Some(AssessmentResult::Completed { suspected }) => {
            for pair in suspected {
                links.push(Link {
                    denial_seq: pair.denial,
                    attempt_seq: pair.attempt,
                    kind: LinkKind::Suspected,
                    reason: pair.reason.clone(),
                });
            }
            Assessment::Found
        }
    }
}

/// A report for every stretch in `entries`, oldest first. `active` says AFK is on now, which is
/// what makes a stretch with no end `Active` rather than `Ended`.
pub fn build_all(entries: &[Entry], ceiling: f64, active: bool) -> Vec<AfkReport> {
    let names = agent_names(entries);
    let bounds = stretch_bounds(entries);
    let count = bounds.len();
    bounds
        .into_iter()
        .enumerate()
        .filter_map(|(at, range)| {
            build_one(entries, range, &names, ceiling, active && at + 1 == count)
        })
        .collect()
}

/// The report of the latest stretch.
pub fn build_latest(entries: &[Entry], ceiling: f64, active: bool) -> Option<AfkReport> {
    build_all(entries, ceiling, active).pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, record: Record) -> Entry {
        Entry {
            seq,
            ts_ms: seq * 10,
            record,
        }
    }

    fn afk_on(seq: u64) -> Entry {
        entry(
            seq,
            Record::Afk {
                on: true,
                source: AfkSource::Command,
            },
        )
    }

    fn denial(
        seq: u64,
        tool: &str,
        agent: &str,
        tier: Tier,
        rule: &str,
        digest: &str,
        paths: &[&str],
    ) -> Entry {
        entry(
            seq,
            Record::Denial {
                tool: tool.into(),
                agent: agent.into(),
                reason: "denied".into(),
                rule: Some(rule.into()),
                summary: format!("{tool} call"),
                tier: Some(tier),
                tool_use_id: None,
                digest: Some(digest.into()),
                paths: paths.iter().map(|p| p.to_string()).collect(),
                converted: None,
                fail_closed: None,
                explanation: Some("because".into()),
            },
        )
    }

    fn action(
        seq: u64,
        tool: &str,
        agent: &str,
        id: Option<&str>,
        digest: &str,
        paths: &[&str],
    ) -> Entry {
        entry(
            seq,
            Record::Action {
                tool: tool.into(),
                agent: agent.into(),
                paths: paths.iter().map(|p| p.to_string()).collect(),
                created: Vec::new(),
                summary: format!("{tool} call"),
                tier: Some(Tier::Guardian),
                tool_use_id: id.map(str::to_string),
                digest: Some(digest.into()),
            },
        )
    }

    fn facts<'a>(tier: Tier, rule: &'a str) -> DenialFacts<'a> {
        DenialFacts {
            tool: "shell",
            tier: Some(tier),
            rule: Some(rule),
            converted: None,
            fail_closed: None,
            digest: None,
            paths: &[],
        }
    }

    #[test]
    fn every_guardian_label_has_the_severity_the_table_gives() {
        for label in RuleLabel::ALL {
            if label == RuleLabel::Allow {
                continue;
            }
            let high = matches!(
                label,
                RuleLabel::DataExfiltration
                    | RuleLabel::DownloadExecute
                    | RuleLabel::TunnelReverseShell
                    | RuleLabel::SecretExposure
                    | RuleLabel::SafetyBypass
                    | RuleLabel::ContainerEscape
                    | RuleLabel::UnguardedAgentLoop
            );
            let weight = severity_weight(&facts(Tier::Guardian, label.display_name()));
            let expected = if high { 3.0 } else { 2.0 };
            assert!((weight - expected).abs() < 1e-9, "{label:?} gave {weight}");
        }
    }

    #[test]
    fn floor_screen_and_plan_denials_are_medium_and_other_tiers_low() {
        for tier in [
            Tier::DenyRule,
            Tier::CriticalPath,
            Tier::ProtectedPath,
            Tier::PlanBlock,
            Tier::Screen,
        ] {
            assert!(
                (severity_weight(&facts(tier, "x")) - 2.0).abs() < 1e-9,
                "{tier:?}"
            );
        }
        assert!((severity_weight(&facts(Tier::Mode, "x")) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_converted_prompt_is_low_but_never_lowers_a_high_label() {
        let mut converted = facts(Tier::AskRule, "Bash(*)");
        converted.converted = Some(Converted::AskRule);
        assert!((severity_weight(&converted) - 1.0).abs() < 1e-9);
        let mut escalate = facts(Tier::Guardian, "Data Exfiltration");
        escalate.converted = Some(Converted::Escalate);
        assert!((severity_weight(&escalate) - 3.0).abs() < 1e-9);
        let mut paused = facts(Tier::Guardian, "guardian");
        paused.converted = Some(Converted::BreakerPaused);
        assert!((severity_weight(&paused) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_fail_closed_denial_weighs_half_and_an_unavailable_conversion_is_low() {
        let mut timeout = facts(Tier::Guardian, "Timeout");
        timeout.fail_closed = Some("timeout");
        assert!((severity_weight(&timeout) - 0.5).abs() < 1e-9);
        let mut unavailable = facts(Tier::Guardian, "GuardianUnavailable");
        unavailable.fail_closed = Some("unavailable");
        unavailable.converted = Some(Converted::GuardianUnavailable);
        assert!((severity_weight(&unavailable) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn an_exact_retry_links_across_agents() {
        let entries = vec![
            afk_on(0),
            denial(
                1,
                "shell",
                "main",
                Tier::Guardian,
                "Data Exfiltration",
                "d1",
                &[],
            ),
            action(2, "shell", "agent-1", None, "d1", &[]),
        ];
        let report = build_latest(&entries, 12.0, true).unwrap();
        assert_eq!(report.review.links.len(), 1);
        let link = &report.review.links[0];
        assert_eq!((link.denial_seq, link.attempt_seq), (1, 2));
        assert_eq!(link.kind, LinkKind::ExactRetry);
    }

    #[test]
    fn a_shell_write_to_the_path_of_a_denied_write_is_linked() {
        let entries = vec![
            afk_on(0),
            denial(
                1,
                "write",
                "main",
                Tier::Guardian,
                "Safety Bypass",
                "d1",
                &["/w/a.txt"],
            ),
            action(2, "shell", "main", None, "d2", &["/w/a.txt"]),
        ];
        let report = build_latest(&entries, 12.0, true).unwrap();
        assert_eq!(report.review.links[0].kind, LinkKind::SameTargetOtherTool);
    }

    #[test]
    fn independent_work_a_read_after_a_denied_write_and_a_narrower_call_stay_unmarked() {
        let entries = vec![
            afk_on(0),
            denial(
                1,
                "write",
                "main",
                Tier::Guardian,
                "Safety Bypass",
                "d1",
                &["/w/a.txt"],
            ),
            action(2, "write", "main", None, "d2", &["/w/b.txt"]),
            action(3, "read", "main", None, "d3", &["/w/a.txt"]),
            action(4, "shell", "main", None, "d4", &["/w/c.txt"]),
        ];
        let report = build_latest(&entries, 12.0, true).unwrap();
        assert!(report.review.links.is_empty(), "{:?}", report.review.links);
    }

    #[test]
    fn the_timeline_is_chronological_with_agent_names_and_missing_outcomes_stay_unknown() {
        let entries = vec![
            entry(
                0,
                Record::Agent {
                    event: "spawned".into(),
                    agent: "agent-1".into(),
                    parent: None,
                    detail: None,
                    name: Some("scout".into()),
                },
            ),
            afk_on(1),
            action(2, "read", "agent-1", Some("t1"), "d", &[]),
            action(3, "write", "main", Some("t2"), "e", &["/w/a"]),
            entry(
                4,
                Record::ToolOutcome {
                    tool_use_id: "t1".into(),
                    agent: "agent-1".into(),
                    status: OutcomeStatus::Ok,
                },
            ),
        ];
        let report = build_latest(&entries, 12.0, true).unwrap();
        let names: Vec<_> = report
            .timeline
            .iter()
            .map(|a| a.agent_name.as_str())
            .collect();
        assert_eq!(names, ["scout", "main"]);
        assert_eq!(report.timeline[0].outcome, Some(OutcomeStatus::Ok));
        assert_eq!(report.timeline[1].outcome, None);
        assert_eq!(report.counts.succeeded, 1);
        assert_eq!(report.counts.unknown, 1);
    }

    #[test]
    fn the_assessment_never_renders_no_evasion_unless_it_ran_and_found_none() {
        let mut entries = vec![
            afk_on(0),
            denial(
                1,
                "shell",
                "main",
                Tier::Guardian,
                "Git Destructive",
                "d1",
                &[],
            ),
        ];
        let status =
            |entries: &[Entry]| build_latest(entries, 12.0, true).unwrap().review.assessment;
        assert_eq!(status(&entries), Assessment::Pending);
        entries.push(entry(
            2,
            Record::AfkAssessment {
                stretch_seq: 0,
                result: AssessmentResult::Unavailable {
                    reason: "timeout".into(),
                },
            },
        ));
        let unavailable = status(&entries);
        assert!(
            unavailable
                .describe()
                .starts_with("evasion assessment unavailable")
        );
        assert!(!unavailable.describe().contains("no suspected evasion"));
        entries.push(entry(
            3,
            Record::AfkAssessment {
                stretch_seq: 0,
                result: AssessmentResult::Completed {
                    suspected: Vec::new(),
                },
            },
        ));
        assert_eq!(status(&entries), Assessment::NoSuspectedEvasion);
    }

    #[test]
    fn a_stretch_without_denials_is_no_denials_even_with_a_stray_record() {
        let entries = vec![afk_on(0), action(1, "read", "main", None, "d", &[])];
        let report = build_latest(&entries, 12.0, true).unwrap();
        assert_eq!(report.review.assessment, Assessment::NoDenials);
        assert!(report.review.assessment.describe().contains("no denials"));
    }

    #[test]
    fn the_replayed_temperature_equals_the_live_trackers() {
        let entries = vec![
            afk_on(0),
            denial(
                1,
                "shell",
                "main",
                Tier::Guardian,
                "Data Exfiltration",
                "d1",
                &[],
            ),
            action(2, "read", "main", None, "d", &[]),
            denial(
                3,
                "shell",
                "main",
                Tier::Guardian,
                "Data Exfiltration",
                "d1",
                &[],
            ),
        ];
        let mut live = AfkLedgerState::default();
        for e in &entries {
            live.apply(e.seq, &e.record);
        }
        let report = build_latest(&entries, 12.0, true).unwrap();
        assert!((live.temperature().current() - report.temperature.current).abs() < 1e-12);
        assert!((live.temperature().peak() - report.temperature.peak).abs() < 1e-12);
        // The retry adds the exact-retry bonus on top of a High denial.
        assert!(
            report.temperature.contributing[1].weight > report.temperature.contributing[0].weight
        );
    }

    #[test]
    fn a_blocked_or_stopped_stretch_reports_that_outcome() {
        let mut entries = vec![afk_on(0), entry(1, Record::AfkBlocked {})];
        assert_eq!(
            build_latest(&entries, 12.0, true).unwrap().outcome,
            AfkOutcome::Blocked
        );
        entries.push(entry(
            2,
            Record::AfkStop {
                value: 13.0,
                ceiling: 12.0,
                contributing: vec![1],
            },
        ));
        assert_eq!(
            build_latest(&entries, 12.0, true).unwrap().outcome,
            AfkOutcome::Stopped
        );
    }

    #[test]
    fn stretches_are_separate_and_a_reopened_ledger_ends_the_open_one() {
        let entries = vec![
            afk_on(0),
            entry(
                1,
                Record::Afk {
                    on: false,
                    source: AfkSource::Command,
                },
            ),
            afk_on(2),
        ];
        let all = build_all(&entries, 12.0, false);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].outcome, AfkOutcome::Ended);
        assert_eq!(all[1].outcome, AfkOutcome::Ended);
        assert_eq!(
            build_all(&entries, 12.0, true)[1].outcome,
            AfkOutcome::Active
        );
        let mut state = AfkLedgerState::default();
        for e in &entries {
            state.apply(e.seq, &e.record);
        }
        state.close_open_stretch();
        assert_eq!(state.latest(), Some(AfkOutcome::Ended));
    }

    #[test]
    fn a_stop_without_a_review_survives_replay_and_a_review_clears_it() {
        let mut state = AfkLedgerState::default();
        state.apply(
            5,
            &Record::AfkStop {
                value: 13.0,
                ceiling: 12.0,
                contributing: vec![2, 4],
            },
        );
        assert_eq!(state.stopped().map(|s| s.seq), Some(5));
        state.apply(6, &Record::AfkReviewed {});
        assert!(state.stopped().is_none());
    }
}
