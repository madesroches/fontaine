//! `-p "/goal ..."`: the one slash command headless understands. The loop that runs the goal's
//! turns is in the parent module; this holds what describes a goal to the consumer.

use super::{GoalReport, Outcome, Subtype};
use crate::core::agent::AgentRuntime;
use crate::core::goal::{GoalArg, Summary, parse_goal_arg};
use crate::core::ledger::GoalEvent;
use crate::core::turn::TurnOutcome;

/// The argument of `/goal` when `text` is that command: `/goal` followed by whitespace or the
/// end. Any other slash command is an ordinary prompt here.
pub(super) fn command(text: &str) -> Option<GoalArg> {
    let rest = text.trim_start().strip_prefix("/goal")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(parse_goal_arg(rest))
}

/// Whether the run did not finish what it was asked: the goal ended impossible or paused.
pub(super) fn failure(report: &GoalReport) -> Option<String> {
    match report.status {
        "impossible" | "paused" => Some(format!(
            "goal {}: {}",
            report.status,
            report.reason.as_deref().unwrap_or("no reason given")
        )),
        _ => None,
    }
}

/// A goal that met its end in this message.
pub(super) struct Ended {
    pub(super) event: GoalEvent,
    pub(super) summary: Summary,
    pub(super) reason: String,
}

/// The goal as it stands after a message's turns.
pub(super) fn report(runtime: &AgentRuntime, ended: Option<Ended>) -> Option<GoalReport> {
    let status = runtime.session.goal().status(runtime.tree_spend());
    if let Some(active) = status.active {
        let (name, reason) = match &active.paused {
            Some(why) => ("paused", Some(why.to_string())),
            None => ("active", active.last_reason),
        };
        return Some(GoalReport {
            condition: active.condition,
            status: name,
            evaluations: active.evaluations,
            reason,
        });
    }
    let ended = ended?;
    Some(GoalReport {
        condition: ended.summary.condition,
        status: if ended.event == GoalEvent::Met {
            "met"
        } else {
            "impossible"
        },
        evaluations: ended.summary.evaluations,
        reason: Some(ended.reason),
    })
}

/// What `-p "/goal"` answers: no model turn.
pub(super) fn status_outcome(runtime: &AgentRuntime) -> (Outcome, Option<GoalReport>) {
    let status = runtime.session.goal().status(runtime.tree_spend());
    (answer(status.describe()), report(runtime, None))
}

/// What `-p "/goal clear"` answers: no model turn.
pub(super) fn clear_outcome(runtime: &AgentRuntime) -> (Outcome, Option<GoalReport>) {
    match runtime.clear_goal() {
        Some(cleared) => {
            let report = GoalReport {
                condition: cleared.condition.clone(),
                status: "cleared",
                evaluations: cleared.evaluations,
                reason: None,
            };
            (
                answer(format!("Goal cleared: {}", cleared.condition)),
                Some(report),
            )
        }
        None => (answer("No goal set".to_string()), None),
    }
}

fn answer(text: String) -> Outcome {
    Outcome {
        subtype: Subtype::Success,
        text,
        structured: None,
        error: None,
        started: std::time::Instant::now(),
        denials: Vec::new(),
    }
}

/// How the goal sees a turn that ended as `outcome`.
pub(super) fn turn_end(outcome: &Outcome) -> TurnOutcome {
    let message = outcome.error.clone().unwrap_or_default();
    match outcome.subtype {
        Subtype::Success => TurnOutcome::Answer(outcome.text.clone()),
        Subtype::MaxTurns => TurnOutcome::MaxTurns(message),
        Subtype::MaxBudget => TurnOutcome::Stopped,
        Subtype::Execution | Subtype::UsageLimit => TurnOutcome::Failed(message.into()),
    }
}
