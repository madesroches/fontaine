//! The guardian profiles: `full`, and the container-mode `minimal` that narrows what the
//! guardian is asked to judge. A profile never turns the guardian off.

use crate::request::{AdjudicationRequest, SessionFacts, trailing_chars};
use crate::rules::RuleConfig;

/// `task_intent` characters the minimal profile keeps.
pub const MINIMAL_INTENT_CHARS: usize = 500;
/// Recent user messages the minimal profile keeps.
pub const MINIMAL_RECENT_USER_MESSAGES: usize = 3;

pub const FULL_PROMPT: &str = include_str!("../prompts/guardian_prompt.md");
pub const MINIMAL_PROMPT: &str = include_str!("../prompts/guardian_minimal.md");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    #[default]
    Full,
    Minimal,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Full => "full",
            Profile::Minimal => "minimal",
        }
    }

    /// The versioned system prompt asset this profile renders from.
    pub fn prompt(self) -> &'static str {
        match self {
            Profile::Full => FULL_PROMPT,
            Profile::Minimal => MINIMAL_PROMPT,
        }
    }

    /// The request this profile adjudicates. Under `minimal` the call, its roots and the
    /// touched paths' status stay, the user's words are cut short, and project instructions,
    /// hook notes and earlier denials are dropped; the rule lists are carried by the minimal
    /// prompt itself.
    pub fn shape(self, request: &AdjudicationRequest) -> AdjudicationRequest {
        match self {
            Profile::Full => request.clone(),
            Profile::Minimal => {
                let recent = &request.recent_user_messages;
                let from = recent.len().saturating_sub(MINIMAL_RECENT_USER_MESSAGES);
                AdjudicationRequest {
                    task_intent: trailing_chars(&request.task_intent, MINIMAL_INTENT_CHARS)
                        .to_string(),
                    recent_user_messages: recent[from..].to_vec(),
                    project_instructions: None,
                    environment: request.environment.clone(),
                    rules: RuleConfig::minimal(),
                    tool_call: request.tool_call.clone(),
                    session_facts: SessionFacts {
                        workspace_roots: request.session_facts.workspace_roots.clone(),
                        workspace_trust: String::new(),
                        git_remotes: request.session_facts.git_remotes.clone(),
                        touched_paths: request.session_facts.touched_paths.clone(),
                        actions_taken: Vec::new(),
                    },
                    hook_context: Vec::new(),
                    prior_denials: Vec::new(),
                }
            }
        }
    }
}
