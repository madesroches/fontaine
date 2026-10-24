//! The guardian as a composition: a cheap screen stage beside the LLM stage. The two start
//! together, and the screen settles the call only while the LLM stage is still pending.
//! Only a screen allow and an LLM-stage allow ever allow a call; every other screen outcome
//! leaves the call to the LLM stage, which is itself fail-closed.

use crate::guardian::{Adjudication, Guardian};
use crate::request::AdjudicationRequest;
use crate::screen::{Screen, ScreenOutcome, ScreenScores};

/// The screen as a stage of the guardian. `enforce = false` is shadow mode: it scores, never
/// decides.
pub struct ScreenInput<'a> {
    pub screen: &'a Screen,
    pub enforce: bool,
}

pub enum Decided {
    /// The screen stage stopped the call (escalate); the LLM stage was dropped.
    ScreenStop(ScreenScores),
    /// The screen stage allowed the call; the LLM stage was dropped.
    ScreenAllow(ScreenScores),
    /// The LLM stage decided, with or without a screen note.
    Guardian(Box<Adjudication>),
}

pub struct Composed {
    /// What the screen made of the call; `None` when there is no screen stage.
    pub screen: Option<ScreenOutcome>,
    pub decided: Decided,
}

pub async fn adjudicate(
    screen: Option<ScreenInput<'_>>,
    guardian: &Guardian,
    request: &AdjudicationRequest,
) -> Composed {
    let Some(input) = screen else {
        return Composed {
            screen: None,
            decided: Decided::Guardian(Box::new(guardian.adjudicate(request).await)),
        };
    };
    let guardian_call = guardian.adjudicate(request);
    let screening = input.screen.screen(request);
    tokio::pin!(guardian_call);
    tokio::pin!(screening);
    // Biased, guardian first: a verdict that is ready on the same poll as the screen wins, so
    // a computed denial, its usage and its breaker effect are never discarded.
    tokio::select! {
        biased;
        adjudication = &mut guardian_call => {
            // The screen's own timeout bounds this wait; its scores are kept for the record.
            let outcome = screening.await;
            Composed {
                screen: Some(outcome),
                decided: Decided::Guardian(Box::new(adjudication)),
            }
        }
        outcome = &mut screening => {
            let decided = match &outcome {
                ScreenOutcome::Stop { scores, .. } if input.enforce => {
                    Some(Decided::ScreenStop(*scores))
                }
                ScreenOutcome::Allow { scores, .. } if input.enforce => {
                    Some(Decided::ScreenAllow(*scores))
                }
                _ => None,
            };
            let decided = match decided {
                // Dropping the LLM stage's future cancels its request.
                Some(decided) => decided,
                None => Decided::Guardian(Box::new(guardian_call.await)),
            };
            Composed {
                screen: Some(outcome),
                decided,
            }
        }
    }
}
