//! What the guardian sees for one tool call. The core builds it from the session ledger only,
//! never from the model context, so nothing a compaction summary or a tool result says can
//! reach the guardian as the user's intent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rules::{EnvironmentConfig, RuleConfig};

/// The `task_intent` budget of the full profile, in characters.
pub const TASK_INTENT_CHARS: usize = 2_000;
/// How many of the user's most recent turns the request carries.
pub const RECENT_USER_MESSAGES: usize = 5;
/// How many of the latest actions the session facts list.
pub const ACTIONS_TAKEN: usize = 20;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCallSummary {
    pub tool: String,
    /// The full arguments the agent supplied.
    pub args: Value,
    /// Filled by the core, so this crate needs no hashing dependency.
    pub args_digest: String,
    /// Derived by the core from the call itself, never from the guardian's own label.
    pub destructive: bool,
    /// What an MCP server declared about the tool. The server is outside the harness, so this
    /// is shown as untrusted text and never as a fact.
    pub annotations: Option<String>,
}

/// Whether a path the call touches was there before the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreExistence {
    /// In the file index taken at session start.
    PreExisting,
    /// Not indexed, and an earlier action of this session touched it.
    CreatedThisSession,
    /// Not indexed and not touched by this session: possibly pre-existing.
    Unindexed,
}

impl PreExistence {
    pub fn as_str(self) -> &'static str {
        match self {
            PreExistence::PreExisting => "pre_existing",
            PreExistence::CreatedThisSession => "created_this_session",
            PreExistence::Unindexed => "unindexed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TouchedPath {
    pub path: String,
    pub status: PreExistence,
    /// Set by the harness when the resolved path lies inside the session scratch directory.
    pub in_scratch: bool,
    /// Set by the harness when nothing exists at the path (not even a dangling symlink) as the
    /// request is built.
    pub absent: bool,
    /// Set by the harness when git's index holds the path, or for a directory something under it.
    pub git_tracked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActionTaken {
    pub tool: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionFacts {
    pub workspace_roots: Vec<String>,
    pub workspace_trust: String,
    /// Fetch URLs of the repositories around the roots, read when the session started.
    pub git_remotes: Vec<String>,
    /// Names of the variables the user's config sets on every subprocess; values are never
    /// carried.
    pub env_keys: Vec<String>,
    /// Only the paths this call touches; the file index itself is never shown.
    pub touched_paths: Vec<TouchedPath>,
    /// The last [`ACTIONS_TAKEN`] actions, oldest first.
    pub actions_taken: Vec<ActionTaken>,
    /// The agent's persistent memory directory, set by the harness from the session and never
    /// from a tool argument. Future sessions load what is written there as notes.
    pub memory_dir: Option<String>,
    /// The session scratch directory, set by the harness from the session and never from a tool
    /// argument. It is created empty when the session starts.
    pub scratch_dir: Option<String>,
    /// What the agent a call would start can do. Set only for an `agent` call.
    pub spawn: Option<SpawnFacts>,
    /// What `fontaine auto-mode setup` stored for this project. Only the guardian sees it.
    pub project_environment: Option<ProjectEnvironmentFacts>,
}

/// The facts of a project's setup record that the guardian reads. The core builds it from its
/// own record, so this crate needs no discovery code.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEnvironmentFacts {
    /// The day the facts were collected, `YYYY-MM-DD`.
    pub collected_on: String,
    #[serde(default)]
    pub remotes: Vec<RemoteEnvironment>,
    #[serde(default)]
    pub tag_release_workflows: Vec<String>,
    #[serde(default)]
    pub workflow_environments: Vec<String>,
    #[serde(default)]
    pub package_registries: Vec<String>,
    #[serde(default)]
    pub env_files: Vec<EnvFileFacts>,
    #[serde(default)]
    pub stack: Vec<String>,
    /// Entries a repository scan proposed and the user accepted.
    pub scan: Option<ScanFindings>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEnvironment {
    pub url: String,
    pub visibility: Visibility,
    pub default_branch: Option<String>,
    pub protection: Protection,
    #[serde(default)]
    pub branch_rules: Vec<String>,
    #[serde(default)]
    pub deploy_environments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Internal,
    Unknown,
    /// An org policy barred the check: counts as possibly public.
    Unverified,
    /// Public when last checked, on the day given, but too long ago to rely on.
    Expired(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    Protected,
    Unprotected,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvFileFacts {
    pub name: String,
    pub gitignored: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanFindings {
    #[serde(default)]
    pub sensitive_locations: Vec<String>,
    #[serde(default)]
    pub deploy_targets: Vec<String>,
    #[serde(default)]
    pub protected_targets: Vec<String>,
    #[serde(default)]
    pub trusted_destinations: Vec<String>,
}

/// The toolset of the agent an `agent` call starts, computed by the harness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SpawnFacts {
    pub agent_type: String,
    pub tools: Vec<String>,
    /// False only when every tool in `tools` is read-only, ignoring the `send_message` exemption
    /// (see `fontaine_core::adjudication::spawn_facts`).
    pub changes_state: bool,
}

/// A denial earlier in the session, reason-coded, for loop detection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DenialRecord {
    pub tool: String,
    pub rule: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AdjudicationRequest {
    /// The user's latest messages, verbatim, oldest dropped first to fit the budget.
    pub task_intent: String,
    pub recent_user_messages: Vec<String>,
    pub project_instructions: Option<String>,
    pub environment: EnvironmentConfig,
    pub rules: RuleConfig,
    pub tool_call: ToolCallSummary,
    pub session_facts: SessionFacts,
    /// `guardian_context` notes from `pre_tool_use` hooks.
    pub hook_context: Vec<String>,
    pub prior_denials: Vec<DenialRecord>,
}

/// The last `budget` characters of `text`.
pub fn trailing_chars(text: &str, budget: usize) -> &str {
    let count = text.chars().count();
    if count <= budget {
        return text;
    }
    let skip = count - budget;
    match text.char_indices().nth(skip) {
        Some((at, _)) => &text[at..],
        None => text,
    }
}

/// The task intent for `messages` (oldest first): the user's words joined, never summarized,
/// cut from the old end to fit `budget` characters.
pub fn task_intent(messages: &[String], budget: usize) -> String {
    let joined = messages.join("\n\n");
    trailing_chars(&joined, budget).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oldest_text_is_dropped_first() {
        assert_eq!(trailing_chars("abcdef", 3), "def");
        assert_eq!(trailing_chars("abc", 10), "abc");
        assert_eq!(trailing_chars("héllo", 3), "llo");
        assert_eq!(
            task_intent(&["first".into(), "second".into()], 8),
            "\n\nsecond"
        );
    }

    #[test]
    fn a_short_history_is_kept_whole() {
        assert_eq!(
            task_intent(&["one".into(), "two".into()], 2_000),
            "one\n\ntwo"
        );
    }
}
