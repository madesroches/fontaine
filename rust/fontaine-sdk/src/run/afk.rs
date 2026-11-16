//! What `-p --afk` adds to the result: the `afk` object, built from the same report the
//! terminal shows, and the outcomes that make a run fail.

use crate::core::afk_report::{AfkOutcome, AfkReport};
use serde_json::Value;

use super::{Outcome, Subtype};

/// Why a run that ended this way exits 1 whatever its turns answered.
pub(super) fn failure(report: &AfkReport) -> Option<String> {
    match report.outcome {
        AfkOutcome::Blocked => Some(
            "AFK blocked: repeated denials paused auto mode while the user was away".to_string(),
        ),
        AfkOutcome::Stopped => Some(stopped_message().to_string()),
        AfkOutcome::Active | AfkOutcome::Ended => None,
    }
}

pub(super) fn stopped_message() -> &'static str {
    "the session was stopped by the AFK temperature ceiling"
}

/// A turn the temperature stop ended, or a message a stopped session refused to start.
pub(super) fn stopped_outcome(started: std::time::Instant, denials: Vec<Value>) -> Outcome {
    Outcome {
        subtype: Subtype::Execution,
        text: String::new(),
        structured: None,
        error: Some(stopped_message().to_string()),
        started,
        denials,
    }
}
