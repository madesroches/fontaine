//! The confirmation before `/guardian-disable`: the guardian and the deterministic floor both
//! come off for the rest of the session, so Fontaine asks rather than toggling on one key.

use crate::confirm_prompt::{Block, ConfirmPrompt};

/// `Some(true)` from the prompt turns the guardian off, `Some(false)` leaves it on.
pub fn new() -> ConfirmPrompt {
    ConfirmPrompt::new(
        "Turn the guardian off for this session?".into(),
        vec![Block::Text(
            "Every tool call will run without the guardian, the permission rules and the protected paths until you type /guardian-enable. This is a development escape hatch.".into(),
        )],
        "y turns it off · n leaves it on",
    )
}

/// The confirmation before `/afk review` lets a stopped session run again. `summary` is the
/// report's headline, so the user has read what stopped it before answering.
pub fn afk_review(summary: String) -> ConfirmPrompt {
    ConfirmPrompt::new(
        "Let the stopped session continue?".into(),
        vec![
            Block::Text(summary),
            Block::Text(
                "Nothing is cleared except the stop: the denials, the temperature peak and the report stay. AFK mode stays off, and the denial breaker keeps its state. Read the report with /afk report first if you have not.".into(),
            ),
        ],
        "y lets it continue · n leaves it stopped",
    )
}
