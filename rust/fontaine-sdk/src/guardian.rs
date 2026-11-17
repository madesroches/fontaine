//! The guardian: a separate, tool-less model conversation that judges one tool call at a time
//! and answers with a typed verdict. Every outcome that is not a verdict is a denial. This
//! module imports nothing from `core`, `tools` or `agents` (a test enforces it), so it stays
//! liftable into another harness.

mod breaker;
mod cache;
pub mod composition;
pub mod critique;
pub mod env_scan;
pub mod evasion;
#[allow(clippy::module_inception)] // the type keeps the module's name
mod guardian;
pub mod models;
mod profile;
pub mod prompt_builder;
mod request;
mod rule_label;
pub mod rules;
pub mod scanner;
mod screen;
pub mod structured;
pub mod systemone;
pub mod temperature;
mod verdict;

pub use breaker::{CONSECUTIVE_LIMIT, LoopBreaker, TOTAL_LIMIT};
pub use cache::TTL as CACHE_TTL;
pub use guardian::{Adjudication, Cause, EXTRACTOR_RETRIES, Guardian, Settings, WireIds};
pub use profile::{FULL_PROMPT, MINIMAL_PROMPT, Profile};
pub use request::{
    ACTIONS_TAKEN, AGENT_TASK_CHARS, ActionTaken, ActiveSkill, AdjudicationRequest, DenialRecord,
    EnvFileFacts, PreExistence, ProjectEnvironmentFacts, Protection, RECENT_USER_MESSAGES,
    RemoteEnvironment, ScanFindings, ScreenIntent, SessionFacts, TASK_INTENT_CHARS,
    ToolCallSummary, TouchedPath, Visibility, leading_chars, task_intent, trailing_chars,
};
pub use rule_label::RuleLabel;
pub use rules::{EnvironmentConfig, List, RuleConfig};
pub use scanner::{CommandScanner, LlmScanner, Scan, Scanner};
pub use screen::{
    BackendKind, DEFAULT_STEERING_VETO, Prompt, Question, Read, ReadOutcome, Reader,
    ReaderSettings, SystemOneSettings, check_backend, check_pairing,
};
pub use structured::{
    FixedProbe, ModeNotice, Notifier, OutputMode, OutputProbe, PROBE_TIMEOUT, ProbeOutcome,
};
pub use verdict::{AuditLabel, Decision, FailClosed, Verdict};
