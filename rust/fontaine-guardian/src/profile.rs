//! The guardian profiles: `full`, and the container-mode `minimal` that narrows what the
//! guardian is asked to judge. A profile never turns the guardian off.

use crate::request::{
    AdjudicationRequest, ScreenIntent, SessionFacts, TouchedPath, trailing_chars,
};
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
                    screen_intent: request.screen_intent.as_ref().map(|screen| {
                        let recent = &screen.recent_user_messages;
                        let from = recent.len().saturating_sub(MINIMAL_RECENT_USER_MESSAGES);
                        ScreenIntent {
                            task_intent: trailing_chars(&screen.task_intent, MINIMAL_INTENT_CHARS)
                                .to_string(),
                            recent_user_messages: recent[from..].to_vec(),
                        }
                    }),
                    // A reply is credit the minimal prompt does not give.
                    agent_message_before_reply: None,
                    project_instructions: None,
                    environment: request.environment.clone(),
                    rules: RuleConfig::minimal(),
                    tool_call: request.tool_call.clone(),
                    session_facts: SessionFacts {
                        workspace_roots: request.session_facts.workspace_roots.clone(),
                        workspace_trust: String::new(),
                        git_remotes: request.session_facts.git_remotes.clone(),
                        git_remote_names: request.session_facts.git_remote_names.clone(),
                        home_directory: request.session_facts.home_directory.clone(),
                        env_keys: request.session_facts.env_keys.clone(),
                        touched_paths: request
                            .session_facts
                            .touched_paths
                            .iter()
                            .map(|touched| TouchedPath {
                                in_scratch: false,
                                absent: false,
                                git_tracked: false,
                                ..touched.clone()
                            })
                            .collect(),
                        actions_taken: Vec::new(),
                        memory_dir: request.session_facts.memory_dir.clone(),
                        scratch_dir: None,
                        project_environment: request.session_facts.project_environment.clone(),
                        active_skills: request.session_facts.active_skills.clone(),
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
    use crate::request::{PreExistence, ToolCallSummary};
    use crate::rules::EnvironmentConfig;

    #[test]
    fn the_minimal_shape_keeps_the_memory_directory() {
        let request = AdjudicationRequest {
            task_intent: String::new(),
            recent_user_messages: Vec::new(),
            screen_intent: None,
            agent_message_before_reply: None,
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
    fn the_minimal_shape_keeps_the_remote_names_and_the_home_directory() {
        let request = AdjudicationRequest {
            task_intent: String::new(),
            recent_user_messages: Vec::new(),
            screen_intent: None,
            agent_message_before_reply: None,
            project_instructions: None,
            environment: EnvironmentConfig::default(),
            rules: RuleConfig::default(),
            tool_call: ToolCallSummary {
                tool: "shell".into(),
                args: serde_json::json!({}),
                args_digest: String::new(),
                destructive: false,
                annotations: None,
            },
            session_facts: SessionFacts {
                git_remote_names: Some(vec!["origin https://x/y.git".into()]),
                home_directory: Some("/home/me".into()),
                ..SessionFacts::default()
            },
            hook_context: Vec::new(),
            prior_denials: Vec::new(),
        };
        let shaped = Profile::Minimal.shape(&request);
        assert_eq!(
            shaped.session_facts.git_remote_names,
            Some(vec!["origin https://x/y.git".to_string()])
        );
        assert_eq!(
            shaped.session_facts.home_directory.as_deref(),
            Some("/home/me")
        );
    }

    #[test]
    fn the_minimal_shape_trims_the_screen_intent_and_drops_the_agent_message() {
        let mut request = AdjudicationRequest {
            task_intent: String::new(),
            recent_user_messages: Vec::new(),
            screen_intent: Some(ScreenIntent {
                task_intent: "x".repeat(MINIMAL_INTENT_CHARS + 10),
                recent_user_messages: (0..6).map(|n| n.to_string()).collect(),
            }),
            agent_message_before_reply: Some("Shall I push?".into()),
            project_instructions: None,
            environment: EnvironmentConfig::default(),
            rules: RuleConfig::default(),
            tool_call: ToolCallSummary {
                tool: "shell".into(),
                args: serde_json::json!({}),
                args_digest: String::new(),
                destructive: false,
                annotations: None,
            },
            session_facts: SessionFacts::default(),
            hook_context: Vec::new(),
            prior_denials: Vec::new(),
        };
        let shaped = Profile::Minimal.shape(&request);
        let screen = shaped.screen_intent.expect("the screen intent is kept");
        assert_eq!(screen.task_intent.chars().count(), MINIMAL_INTENT_CHARS);
        assert_eq!(screen.recent_user_messages, vec!["3", "4", "5"]);
        assert_eq!(shaped.agent_message_before_reply, None);
        request.screen_intent = None;
        assert_eq!(Profile::Minimal.shape(&request).screen_intent, None);
    }

    #[test]
    fn the_minimal_shape_drops_the_scratch_facts() {
        let mut request = AdjudicationRequest {
            task_intent: String::new(),
            recent_user_messages: Vec::new(),
            screen_intent: None,
            agent_message_before_reply: None,
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
        request.session_facts.touched_paths.push(TouchedPath {
            path: "/s/a".into(),
            status: PreExistence::CreatedThisSession,
            in_scratch: true,
            absent: true,
            git_tracked: true,
        });
        let shaped = Profile::Minimal.shape(&request).session_facts;
        assert_eq!(shaped.scratch_dir, None);
        assert_eq!(shaped.touched_paths.len(), 1);
        assert!(!shaped.touched_paths[0].in_scratch);
        assert!(!shaped.touched_paths[0].absent);
        assert!(!shaped.touched_paths[0].git_tracked);
        assert_eq!(
            shaped.touched_paths[0].status,
            PreExistence::CreatedThisSession
        );
    }
}
