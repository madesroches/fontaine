//! The guardian: a separate, tool-less model conversation that judges one tool call at a time
//! and answers with a typed verdict. Every outcome that is not a verdict is a denial. This
//! crate has no knowledge of the harness around it, so another harness can embed it.

mod breaker;
mod cache;
mod guardian;
pub mod models;
mod profile;
pub mod prompt_builder;
mod request;
mod rule_label;
pub mod rules;
pub mod scanner;
mod verdict;

pub use breaker::{CONSECUTIVE_LIMIT, LoopBreaker, TOTAL_LIMIT};
pub use cache::TTL as CACHE_TTL;
pub use guardian::{Adjudication, EXTRACTOR_RETRIES, Guardian, Settings};
pub use profile::{FULL_PROMPT, MINIMAL_PROMPT, Profile};
pub use request::{
    ACTIONS_TAKEN, ActionTaken, AdjudicationRequest, DenialRecord, PreExistence,
    RECENT_USER_MESSAGES, SessionFacts, TASK_INTENT_CHARS, ToolCallSummary, TouchedPath,
    task_intent, trailing_chars,
};
pub use rule_label::RuleLabel;
pub use rules::{EnvironmentConfig, RuleConfig};
pub use scanner::{CommandScanner, LlmScanner, Scan, Scanner};
pub use verdict::{AuditLabel, Decision, FailClosed, Verdict};
