//! The guardian profiles: `full`, and the container-mode `minimal` that narrows what the
//! guardian is asked to judge. A profile never turns the guardian off.

use crate::request::{AdjudicationRequest, SessionFacts, TouchedPath, trailing_chars};
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
                        env_keys: request.session_facts.env_keys.clone(),
                        touched_paths: request
                            .session_facts
                            .touched_paths
                            .iter()
                            .map(|touched| TouchedPath {
                                in_scratch: false,
                                ..touched.clone()
                            })
                            .collect(),
                        actions_taken: Vec::new(),
                        memory_dir: request.session_facts.memory_dir.clone(),
                        scratch_dir: None,
                        spawn: None,
                    },
                    hook_context: Vec::new(),
                    prior_denials: Vec::new(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{PreExistence, SpawnFacts, ToolCallSummary};
    use crate::rules::EnvironmentConfig;

    #[test]
    fn the_minimal_shape_keeps_the_memory_directory() {
        let request = AdjudicationRequest {
            task_intent: String::new(),
            recent_user_messages: Vec::new(),
            project_instructions: None,
            environment: EnvironmentConfig::default(),
            rules: RuleConfig::default(),
            tool_call: ToolCallSummary {
                tool: "write".into(),
                args: serde_json::json!({}),
                args_digest: String::new(),
                destructive: false,
                annotations: None,
            },
            session_facts: SessionFacts {
                memory_dir: Some("/d/memory/k".into()),
                ..SessionFacts::default()
            },
            hook_context: Vec::new(),
            prior_denials: Vec::new(),
        };
        let shaped = Profile::Minimal.shape(&request);
        assert_eq!(
            shaped.session_facts.memory_dir.as_deref(),
            Some("/d/memory/k")
        );
    }

    #[test]
    fn the_minimal_shape_drops_the_scratch_and_spawn_facts() {
        let mut request = AdjudicationRequest {
            task_intent: String::new(),
            recent_user_messages: Vec::new(),
            project_instructions: None,
            environment: EnvironmentConfig::default(),
            rules: RuleConfig::default(),
            tool_call: ToolCallSummary {
                tool: "agent".into(),
                args: serde_json::json!({}),
                args_digest: String::new(),
                destructive: true,
                annotations: None,
            },
            session_facts: SessionFacts::default(),
            hook_context: Vec::new(),
            prior_denials: Vec::new(),
        };
        request.session_facts.scratch_dir = Some("/s".into());
        request.session_facts.spawn = Some(SpawnFacts {
            agent_type: "explore".into(),
            tools: vec!["read".into()],
            changes_state: false,
        });
        request.session_facts.touched_paths.push(TouchedPath {
            path: "/s/a".into(),
            status: PreExistence::CreatedThisSession,
            in_scratch: true,
        });
        let shaped = Profile::Minimal.shape(&request).session_facts;
        assert_eq!(shaped.scratch_dir, None);
        assert_eq!(shaped.spawn, None);
        assert_eq!(shaped.touched_paths.len(), 1);
        assert!(!shaped.touched_paths[0].in_scratch);
        assert_eq!(
            shaped.touched_paths[0].status,
            PreExistence::CreatedThisSession
        );
    }
}
