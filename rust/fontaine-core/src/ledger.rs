//! The session ledger: the append-only record of what the human said and what the harness did,
//! kept apart from the model context that compaction rewrites. Only the harness writes it,
//! never the model, and nothing here removes or edits a record.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::afk_report::{AfkLedgerState, AfkSource, AssessmentResult, OutcomeStatus};
use crate::audit::Converted;
use crate::checkpoint::RewindWhat;
use crate::owner_only;
use crate::project_env::ProjectEnvironment;
use crate::tier::Tier;
use crate::usage_pause::PauseEnd;
use fontaine_tools::lock::lock;

pub const LEDGER_DIR: &str = "ledger";
pub const LEDGER_FILE: &str = "ledger.jsonl";

/// The trailing characters of a root turn's final answer the ledger keeps.
pub const AGENT_REPLY_CHARS: usize = 2_000;

/// Paths the pre-session file index holds before it records that it was cut short.
pub const FILE_INDEX_CAP: usize = 50_000;

/// Where a main-conversation message came from, which decides what the guardian may read into
/// it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageOrigin {
    /// Every writer but the two below, and records from ledgers that predate the field.
    #[default]
    Other,
    /// The human's answers in an `ask_user_question` dialog, recorded by the harness.
    Dialog,
    /// Typed at the idle prompt after the previous turn's answer was shown.
    Prompt,
}

impl MessageOrigin {
    fn is_other(&self) -> bool {
        *self == MessageOrigin::Other
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    SessionStart {
        session_id: String,
        permission_mode: String,
        workspace_roots: Vec<String>,
        workspace_trust: String,
        git_remotes: Vec<String>,
        /// Each remote as `<git name> <fetch url>`; `None` when the remotes were not read completely or
        /// the ledger predates the field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_remote_names: Option<Vec<String>>,
        /// Names of the `[env]` overrides every subprocess gets; never their values.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env_keys: Vec<String>,
        /// The session ran non-interactively (`-p`); absent in ledgers written before it was recorded.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        headless: bool,
        /// Project hooks, MCP servers and skill commands were allowed to load; absent means they
        /// were not, or the ledger predates the field.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        project_executables: bool,
        /// What `fontaine auto-mode setup` stored for the project, as this session loaded it:
        /// filtered to the current remotes, with an expired `public` already downgraded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_environment: Option<ProjectEnvironment>,
    },
    /// The human ran `/auto-mode-setup` and accepted its result. The latest value wins.
    ProjectEnvironment {
        project_environment: ProjectEnvironment,
    },
    FileIndex {
        paths: Vec<String>,
        truncated: bool,
    },
    /// A human message, verbatim. `agent` names the subagent it was typed to in that agent's
    /// view; `None` is the main conversation. `attachments` names the labels of any inline
    /// files it carried.
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "MessageOrigin::is_other")]
        origin: MessageOrigin,
    },
    /// The tail of a root turn's final answer, written by the harness. The user's next message
    /// replies to it, which the guardian reads as untrusted data beside that message.
    AgentReply {
        text: String,
    },
    ModeChange {
        from: String,
        to: String,
    },
    /// The human turned the guardian and the permission floor off or back on with
    /// `/guardian-disable` and `/guardian-enable`. The toggle state is session-scoped and never
    /// carried into another session; the record itself is kept in this session's ledger.
    GuardianOff {
        off: bool,
    },
    /// The human trusted a skill's `allowed-tools` at one version of its file in the TUI. The
    /// CLI's `plugin trust` writes no record.
    SkillTrust {
        name: String,
        digest: String,
    },
    /// The human installed, updated, removed, enabled or disabled a plugin, or reloaded the
    /// plugins, in the TUI. `op` is `install`, `update`, `uninstall`, `enable`, `disable` or
    /// `reload`; `plugin` and `fingerprint` are empty when the operation names none.
    PluginChange {
        op: String,
        plugin: String,
        fingerprint: String,
    },
    /// The human submitted a bug report with `/bug`. `url` is the issue URL or the prefilled-issue
    /// URL.
    BugReport {
        method: String,
        level: String,
        url: String,
    },
    Approval {
        tool: String,
        agent: String,
        scope: String,
    },
    Grant {
        rule: String,
    },
    Denial {
        tool: String,
        agent: String,
        reason: String,
        /// The audit rule label the denial carried, such as `Git Destructive` or `deny_rule`.
        #[serde(default)]
        rule: Option<String>,
        /// A compact, credential-redacted summary of the call.
        #[serde(default)]
        summary: String,
        /// The pipeline stage that refused the call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<Tier>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        /// The digest of the call's arguments, as the audit record carries it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
        /// The absolute paths the call touches.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        /// The would-prompt outcome this denial stands in for, when nobody could be asked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        converted: Option<Converted>,
        /// Why the guardian failed closed, when it did: `error`, `timeout`, `unparseable`,
        /// `low_confidence`, `unavailable` or `not_signed_in`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fail_closed: Option<String>,
        /// The rule's explanation without the advice to the model, redacted and cut short.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
    },
    Action {
        tool: String,
        agent: String,
        /// The absolute paths the call touches, as the scope validator resolved them.
        #[serde(default)]
        paths: Vec<String>,
        /// The subset of `paths` that was absent on disk when the call was allowed, so a call
        /// that touches an existing file never marks it as made by this session.
        #[serde(default)]
        created: Vec<String>,
        /// A compact, credential-redacted summary of the arguments.
        #[serde(default)]
        summary: String,
        /// The pipeline stage that allowed the call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<Tier>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        /// The digest of the call's arguments, as the audit record carries it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    /// How an allowed call ended. Written by the result pipeline after the tool returned, so a
    /// call with no outcome is one whose end was never seen.
    ToolOutcome {
        tool_use_id: String,
        agent: String,
        status: OutcomeStatus,
    },
    /// The human turned AFK mode on or off. The flag is session-scoped and never restored by a
    /// resume; the records are what the report is built from.
    Afk {
        on: bool,
        source: AfkSource,
    },
    /// Repeated denials paused `auto` while AFK was on, so nobody could resume it.
    AfkBlocked {},
    /// The session temperature reached its ceiling and the session was stopped. It stays stopped,
    /// across resume and fork, until an `AfkReviewed` follows.
    AfkStop {
        value: f64,
        ceiling: f64,
        /// The ledger positions of the denials that made up the temperature.
        contributing: Vec<u64>,
    },
    /// The human reviewed a stopped session and let it continue.
    AfkReviewed {},
    /// What the independent review of an AFK stretch found. The latest record for a stretch
    /// replaces earlier ones.
    AfkAssessment {
        /// The ledger position of the `Afk` record that started the stretch.
        stretch_seq: u64,
        result: AssessmentResult,
    },
    /// A directory added to the workspace roots after the session started.
    AddDir {
        root: String,
    },
    /// This session continues another one: `resume` and `continue` reopen it, `fork` copies it
    /// under a new id. Session grants are never carried over.
    Resumed {
        source: String,
        how: String,
    },
    /// A subagent started or ended, or two agents exchanged a message. What an agent was told
    /// to do is not recorded here as anything the human said.
    Agent {
        event: String,
        agent: String,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        /// The agent's name, set on `spawned`; records from before names existed lack it.
        #[serde(default)]
        name: Option<String>,
        /// The concrete model reference the agent runs on, set on `spawned`, so a resumed
        /// session revives it on that one; records from before it lack it.
        #[serde(default)]
        model: Option<String>,
    },
    /// A `!` command the human ran in their own shell, outside the permission pipeline. `None`
    /// when it did not exit with a code, such as when it was interrupted.
    UserShell {
        command: String,
        exit_code: Option<i32>,
    },
    /// The human emptied the model's context with `/clear`. The guardian's view of what the
    /// human asked starts again here.
    ContextCleared {},
    /// The session was named, by `--name` or `/rename`; the latest record wins. Like `Scheduled`
    /// it is not something the human asked, so it stays out of the guardian's view of intent.
    SessionNamed {
        name: String,
    },
    /// A scheduled task was created, ran, was cancelled or ran out. The prompt it runs is not
    /// recorded: a task fires as a harness note, never as something the human said, so this
    /// record is not part of the guardian's view of the user's intent.
    Scheduled {
        event: ScheduleEvent,
        id: String,
        /// `user` for `/loop`, `model` for `cron_create`.
        origin: String,
        cron: String,
        recurring: bool,
    },
    /// The human rewound to the start of an earlier prompt with `/rewind`. Nothing before it is
    /// removed: approvals, grants and denials stay. The guardian's view of what the human asked
    /// drops the `dropped_user_messages` most recent messages, which the rewind retracted.
    Rewound {
        turn: u64,
        what: RewindWhat,
        dropped_user_messages: usize,
    },
    /// A `/goal` was set, ended or paused. The condition is not recorded: it is in the
    /// `UserMessage` of the line that set it, and a goal turn is a harness note, never something
    /// the human said.
    Goal {
        event: GoalEvent,
        evaluations: u32,
        /// Why a goal paused when the pause did not come from the goal's own rules.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A model request failed and ended the agent's turn. `class` is the stable error class the
    /// `api_error` telemetry event carries; `message` is the failure as shown to the user.
    RequestFailed {
        agent: String,
        class: String,
        message: String,
    },
    /// A model list ran out of quota on its serving entry and moved on. `role` is the list's
    /// role, if it has one; `from` and `to` are `<account>/<model>`.
    ModelFellThrough {
        agent: String,
        role: Option<String>,
        from: String,
        to: String,
        class: String,
    },
    /// The run was put on hold because `account`'s usage window is spent. `reset_at` is Unix
    /// epoch seconds, when the provider said when it resets.
    UsagePaused {
        role: String,
        account: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_at: Option<u64>,
    },
    /// The hold ended, and how.
    UsageResumed {
        end: PauseEnd,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalEvent {
    Set,
    Met,
    Impossible,
    Cleared,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleEvent {
    Created,
    Fired,
    Deleted,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub record: Record,
}

struct Tail {
    file: File,
    next_seq: u64,
}

/// A path as the ledger compares it: the same file spelled with or without a Windows verbatim
/// prefix, or in another letter case on Windows, is one path.
pub fn path_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// What the ledger records about one subagent's lifecycle, oldest first: its spawn, the
/// status of its latest finished run, or `None` while no run has finished (a crash
/// mid-run, or a resume still in flight).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRecord {
    pub id: String,
    /// The id when the spawn record predates names.
    pub name: String,
    pub parent: Option<String>,
    pub agent_type: String,
    /// The model it was spawned on, when the record says.
    pub model: Option<String>,
    pub ended: Option<String>,
}

/// What the ledger's records add up to, kept beside the file so the guardian's request is built
/// from the ledger without re-reading it for every call.
#[derive(Debug)]
pub struct LedgerView {
    pub workspace_roots: Vec<String>,
    pub workspace_trust: String,
    pub git_remotes: Vec<String>,
    /// `<git name> <fetch url>` per remote; `None` when the session start did not record them.
    pub git_remote_names: Option<Vec<String>>,
    pub env_keys: Vec<String>,
    pub project_environment: Option<ProjectEnvironment>,
    index: HashSet<String>,
    pub index_truncated: bool,
    /// Every human message typed to the main conversation, verbatim, oldest first.
    pub user_messages: Vec<String>,
    /// The origin of each entry of `user_messages`, kept in step with it.
    user_origins: Vec<MessageOrigin>,
    /// The final answers of root turns, oldest first.
    agent_replies: Vec<AgentReply>,
    /// The messages typed to a subagent, oldest first.
    pub agent_messages: Vec<AgentMessage>,
    /// One entry per subagent the ledger has seen, in spawn order.
    pub agents: Vec<AgentRecord>,
    /// One past the largest `agent-<n>` ordinal the ledger recorded spawning, so a resumed
    /// session mints ids that never collide with a previous process's.
    pub next_agent: u64,
    pub actions: Vec<ActionEntry>,
    pub denials: Vec<DenialEntry>,
    afk: AfkLedgerState,
    touched: HashSet<String>,
    /// Distinct resolved paths of allowed `write`, `edit` and `notebook_edit` calls, in the order
    /// first seen. Whether the call then succeeded, or a rewind reverted it, is not known here.
    written: Vec<String>,
    written_keys: HashSet<String>,
}

/// The tools whose allowed calls name files the session was permitted to change.
const WRITING_TOOLS: [&str; 3] = ["write", "edit", "notebook_edit"];

/// What a session's ledger adds up to for `/recap`: counts and path sets, not text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LedgerRecap {
    /// The human's messages since the last `/clear` or `/rewind`.
    pub user_messages: Vec<String>,
    /// Files allowed to be written or edited over the whole session, first seen first.
    pub written: Vec<String>,
    /// Allowed calls per tool over the whole session, most used first, ties by name.
    pub tools: Vec<(String, usize)>,
    pub denials: usize,
    pub last_denial: Option<DenialEntry>,
    /// How the latest AFK stretch stands or ended; `None` when AFK was never on.
    pub afk: Option<crate::afk_report::AfkOutcome>,
    /// Subagents spawned over the whole session.
    pub agents_spawned: usize,
}

impl Default for LedgerView {
    fn default() -> LedgerView {
        LedgerView {
            workspace_roots: Vec::new(),
            workspace_trust: String::new(),
            git_remotes: Vec::new(),
            git_remote_names: None,
            env_keys: Vec::new(),
            project_environment: None,
            index: HashSet::new(),
            index_truncated: false,
            user_messages: Vec::new(),
            user_origins: Vec::new(),
            agent_replies: Vec::new(),
            agent_messages: Vec::new(),
            agents: Vec::new(),
            // No spawned record yet: the first id is agent-1.
            next_agent: 1,
            actions: Vec::new(),
            denials: Vec::new(),
            afk: AfkLedgerState::default(),
            touched: HashSet::new(),
            written: Vec::new(),
            written_keys: HashSet::new(),
        }
    }
}

/// The `n` of an `agent-<n>` id, when it is one; ids of any other shape are ignored.
fn agent_ordinal(id: &str) -> Option<u64> {
    id.strip_prefix("agent-").and_then(|rest| rest.parse().ok())
}

/// A human message typed to one subagent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMessage {
    pub agent: String,
    pub text: String,
    /// How many main-conversation messages came before it, which places it for a rewind.
    pub after: usize,
}

/// The tail of a root turn's final answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentReply {
    /// How many main-conversation messages came before it, as `AgentMessage::after`.
    after: usize,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionEntry {
    pub tool: String,
    pub agent: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenialEntry {
    pub tool: String,
    pub agent: String,
    pub rule: String,
    pub summary: String,
}

/// Whether a path was there before the session began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existence {
    /// In the file index taken at session start.
    PreExisting,
    /// Not indexed, and an earlier action of this session created it.
    CreatedThisSession,
    /// Not indexed and untouched so far.
    Unindexed,
}

impl LedgerView {
    /// The human messages that are intent for `caller`: those typed to the main conversation,
    /// and those typed to `caller` or one of its `ancestors`, in the order they were typed.
    /// The words of a sibling's or a descendant's view never count.
    pub fn intent_for(&self, ancestry: &[String]) -> Vec<String> {
        self.intent_with_origin(ancestry)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    /// `intent_for`, each message with its origin. A message typed to a subagent is `Other`.
    pub fn intent_with_origin(&self, ancestry: &[String]) -> Vec<(String, MessageOrigin)> {
        let mut merged = Vec::new();
        let mut agents = self
            .agent_messages
            .iter()
            .filter(|message| ancestry.contains(&message.agent))
            .peekable();
        for (at, text) in self.user_messages.iter().enumerate() {
            while let Some(message) = agents.next_if(|message| message.after <= at) {
                merged.push((message.text.clone(), MessageOrigin::Other));
            }
            let origin = self.user_origins.get(at).copied().unwrap_or_default();
            merged.push((text.clone(), origin));
        }
        merged.extend(agents.map(|message| (message.text.clone(), MessageOrigin::Other)));
        merged
    }

    /// The agent's last message before the user's latest main-conversation message, when that
    /// message was typed at the idle prompt in reply to it and is also the last entry of
    /// `intent_for(ancestry)`.
    ///
    /// Several replies can share a position when harness-started turns record no user message;
    /// the last is the one the user saw. A message of any other origin was not typed in reply to
    /// the last answer, so it pairs with nothing.
    pub fn reply_before_last(&self, ancestry: &[String]) -> Option<&str> {
        let last = self.user_messages.len().checked_sub(1)?;
        if self.user_origins.get(last) != Some(&MessageOrigin::Prompt) {
            return None;
        }
        // Positional, not by text: a subagent message equal to the main one must not pair.
        if self
            .agent_messages
            .iter()
            .any(|message| ancestry.contains(&message.agent) && message.after == last + 1)
        {
            return None;
        }
        self.agent_replies
            .iter()
            .rev()
            .find(|reply| reply.after == last)
            .map(|reply| reply.text.as_str())
    }

    /// The figures `/recap` reports. Only the human's messages honor `/clear` and `/rewind`;
    /// everything else covers the whole session, earlier processes of a resumed one included.
    pub fn recap(&self) -> LedgerRecap {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for action in &self.actions {
            match counts.iter_mut().find(|(tool, _)| *tool == action.tool) {
                Some((_, n)) => *n += 1,
                None => counts.push((action.tool.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        LedgerRecap {
            user_messages: self.user_messages.clone(),
            written: self.written.clone(),
            tools: counts,
            denials: self.denials.len(),
            last_denial: self.denials.last().cloned(),
            afk: self.afk.latest(),
            agents_spawned: self.agents.len(),
        }
    }

    pub fn existence(&self, path: &str) -> Existence {
        let key = path_key(path);
        if self.index.contains(&key) {
            Existence::PreExisting
        } else if self.touched.contains(&key) {
            Existence::CreatedThisSession
        } else {
            Existence::Unindexed
        }
    }

    /// The AFK state the records so far add up to: the open stretch's temperature, an unreviewed
    /// stop, and how the latest stretch ended.
    pub fn afk(&self) -> &AfkLedgerState {
        &self.afk
    }

    fn apply(&mut self, seq: u64, record: &Record) {
        self.afk.apply(seq, record);
        match record {
            Record::SessionStart {
                workspace_roots,
                workspace_trust,
                git_remotes,
                git_remote_names,
                env_keys,
                project_environment,
                ..
            } => {
                self.workspace_roots = workspace_roots.clone();
                self.workspace_trust = workspace_trust.clone();
                self.git_remotes = git_remotes.clone();
                self.git_remote_names = git_remote_names.clone();
                self.env_keys = env_keys.clone();
                // Assigned even when absent, so a resume after the file is gone clears it.
                self.project_environment = project_environment.clone();
            }
            Record::ProjectEnvironment {
                project_environment,
            } => self.project_environment = Some(project_environment.clone()),
            Record::FileIndex { paths, truncated } => {
                self.index = paths.iter().map(|p| path_key(p)).collect();
                self.index_truncated = *truncated;
            }
            Record::UserMessage {
                text,
                agent: None,
                origin,
                ..
            } => {
                self.user_messages.push(text.clone());
                self.user_origins.push(*origin);
            }
            Record::UserMessage {
                text,
                agent: Some(agent),
                ..
            } => self.agent_messages.push(AgentMessage {
                agent: agent.clone(),
                text: text.clone(),
                after: self.user_messages.len(),
            }),
            Record::AgentReply { text } => self.agent_replies.push(AgentReply {
                after: self.user_messages.len(),
                text: text.clone(),
            }),
            Record::ContextCleared {} => {
                self.user_messages.clear();
                self.user_origins.clear();
                self.agent_replies.clear();
                self.agent_messages.clear();
            }
            Record::Action {
                tool,
                agent,
                paths,
                created,
                summary,
                ..
            } => {
                self.touched.extend(created.iter().map(|p| path_key(p)));
                if WRITING_TOOLS.contains(&tool.as_str()) {
                    for path in paths {
                        if self.written_keys.insert(path_key(path)) {
                            self.written.push(path.clone());
                        }
                    }
                }
                self.actions.push(ActionEntry {
                    tool: tool.clone(),
                    agent: agent.clone(),
                    summary: summary.clone(),
                });
            }
            Record::Denial {
                tool,
                agent,
                rule,
                summary,
                reason,
                ..
            } => self.denials.push(DenialEntry {
                tool: tool.clone(),
                agent: agent.clone(),
                rule: rule.clone().unwrap_or_else(|| reason.clone()),
                summary: summary.clone(),
            }),
            Record::AddDir { root } => self.workspace_roots.push(root.clone()),
            Record::Rewound {
                dropped_user_messages,
                ..
            } => {
                let keep = self
                    .user_messages
                    .len()
                    .saturating_sub(*dropped_user_messages);
                self.user_messages.truncate(keep);
                self.user_origins.truncate(keep);
                // The answer to the last kept prompt sits before the rewound prompt and stays.
                self.agent_replies.retain(|reply| reply.after <= keep);
                // Typed after the first dropped message, a subagent's message goes with it: that
                // is stricter, never looser.
                self.agent_messages.retain(|message| message.after <= keep);
            }
            Record::Agent {
                event,
                agent,
                parent,
                detail,
                name,
                model,
            } => match event.as_str() {
                "spawned" => {
                    if let Some(ordinal) = agent_ordinal(agent) {
                        self.next_agent = self.next_agent.max(ordinal + 1);
                    }
                    self.agents.push(AgentRecord {
                        id: agent.clone(),
                        name: name.clone().unwrap_or_else(|| agent.clone()),
                        parent: parent.clone(),
                        agent_type: detail.clone().unwrap_or_default(),
                        model: model.clone(),
                        ended: None,
                    });
                }
                "finished" => {
                    if let Some(entry) = self.agents.iter_mut().rev().find(|a| a.id == *agent) {
                        entry.ended = detail.clone();
                    }
                }
                // A resumed agent is unfinished again until a new finished record lands.
                "resumed" => {
                    if let Some(entry) = self.agents.iter_mut().rev().find(|a| a.id == *agent) {
                        entry.ended = None;
                    }
                }
                _ => {}
            },
            Record::ModeChange { .. }
            | Record::GuardianOff { .. }
            | Record::SkillTrust { .. }
            | Record::PluginChange { .. }
            | Record::BugReport { .. }
            | Record::Approval { .. }
            | Record::Grant { .. }
            | Record::Resumed { .. }
            | Record::UserShell { .. }
            | Record::Scheduled { .. }
            | Record::SessionNamed { .. }
            | Record::ToolOutcome { .. }
            | Record::Afk { .. }
            | Record::AfkBlocked {}
            | Record::AfkStop { .. }
            | Record::AfkReviewed {}
            | Record::AfkAssessment { .. }
            | Record::Goal { .. }
            | Record::RequestFailed { .. }
            | Record::ModelFellThrough { .. }
            | Record::UsagePaused { .. }
            | Record::UsageResumed { .. } => {}
        }
    }
}

pub struct Ledger {
    path: PathBuf,
    tail: Mutex<Tail>,
    view: Mutex<LedgerView>,
}

impl Ledger {
    /// Starts the ledger of the session whose directory is `session_dir`.
    pub fn create(session_dir: &Path) -> io::Result<Ledger> {
        let dir = session_dir.join(LEDGER_DIR);
        owner_only::create_dir(&dir)?;
        let path = dir.join(LEDGER_FILE);
        let file = owner_only::open(&path, true)?;
        Ok(Ledger {
            path,
            tail: Mutex::new(Tail { file, next_seq: 0 }),
            view: Mutex::new(LedgerView::default()),
        })
    }

    /// Reopens the ledger of an earlier session: its records rebuild the view, and new ones
    /// continue the sequence.
    pub fn open(session_dir: &Path) -> io::Result<Ledger> {
        let path = session_dir.join(LEDGER_DIR).join(LEDGER_FILE);
        let text = std::fs::read_to_string(&path)?;
        let mut view = LedgerView::default();
        let mut next_seq = 0;
        let mut good_end = 0;
        let mut offset = 0;
        let mut lines = text.split_inclusive('\n').peekable();
        while let Some(raw) = lines.next() {
            offset += raw.len();
            if raw.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(raw) {
                Ok(entry) => {
                    view.apply(entry.seq, &entry.record);
                    next_seq = entry.seq + 1;
                    good_end = offset;
                }
                // A write cut short (full disk, power loss) can only damage the end, and leaves no
                // newline; a complete line that fails to parse is something else (say, a newer
                // build's record) and is not ours to cut.
                Err(e) if lines.peek().is_none() && !raw.ends_with('\n') => {
                    tracing::warn!(error = %e, "dropping the torn last line of the session ledger");
                    let file = std::fs::OpenOptions::new().write(true).open(&path)?;
                    file.set_len(good_end as u64)?;
                }
                Err(e) => return Err(io::Error::other(e)),
            }
        }
        view.afk.close_open_stretch();
        let mut file = owner_only::open(&path, true)?;
        // A complete last record can still lack its newline; the next append would join it.
        if good_end == text.len() && !text.is_empty() && !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        Ok(Ledger {
            path,
            tail: Mutex::new(Tail { file, next_seq }),
            view: Mutex::new(view),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `record` and returns its position in the ledger.
    pub fn append(&self, record: Record) -> io::Result<u64> {
        let mut tail = lock(&self.tail, "ledger tail");
        let entry = Entry {
            seq: tail.next_seq,
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            record,
        };
        let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        line.push(b'\n');
        tail.file.write_all(&line)?;
        tail.file.flush()?;
        tail.next_seq += 1;
        lock(&self.view, "ledger view").apply(entry.seq, &entry.record);
        Ok(entry.seq)
    }

    /// What the records so far add up to.
    pub fn view(&self) -> std::sync::MutexGuard<'_, LedgerView> {
        lock(&self.view, "ledger view")
    }

    /// Every record written so far, in order.
    pub fn read(&self) -> io::Result<Vec<Entry>> {
        std::fs::read_to_string(&self.path)?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(io::Error::other))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileIndex {
    pub paths: Vec<String>,
    /// The directories met before the walk stopped, without a trailing separator. Kept apart
    /// from `paths` so the ledger's file index and its cap count files only.
    pub dirs: Vec<String>,
    pub truncated: bool,
}

impl FileIndex {
    /// The files under `roots` that git would not ignore, up to `cap` of them, and the
    /// directories met before the walk stops.
    pub fn build(roots: &[PathBuf], cap: usize) -> FileIndex {
        let mut index = FileIndex::default();
        for root in roots {
            let walk = WalkBuilder::new(root)
                .hidden(false)
                .git_global(false)
                .require_git(false)
                .filter_entry(|entry| entry.file_name() != ".git")
                .build();
            for entry in walk.flatten() {
                let kind = entry.file_type();
                if kind.is_some_and(|kind| kind.is_dir()) {
                    if entry.depth() > 0 {
                        index.dirs.push(entry.path().to_string_lossy().into_owned());
                    }
                    continue;
                }
                if !kind.is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                if index.paths.len() == cap {
                    index.truncated = true;
                    return index;
                }
                index
                    .paths
                    .push(entry.path().to_string_lossy().into_owned());
            }
        }
        index
    }

    /// What `@` completion offers: the files, then the directories marked with a trailing `/`
    /// on every platform (the string is only matched and inserted, never opened).
    pub fn completion_paths(&self) -> Vec<String> {
        self.paths
            .iter()
            .cloned()
            .chain(self.dirs.iter().map(|dir| format!("{dir}/")))
            .collect()
    }
}

/// The fetch URLs of the repository around `root`, read from its own config without following
/// `include` or `includeIf`. Empty when there is no repository or gix cannot read it; Fontaine
/// never spawns `git`. A remote without a fetch URL is skipped.
pub fn git_remotes(root: &Path) -> Vec<String> {
    let mut urls: Vec<String> = named_remotes(root)
        .into_iter()
        .filter_map(|(_, url)| url)
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// The fetch URL of the repository's `origin` remote, read as `git_remotes` reads.
pub fn git_origin(root: &Path) -> Option<String> {
    named_remotes(root)
        .into_iter()
        .find_map(|(name, url)| if name == "origin" { url } else { None })
}

fn open_repo(root: &Path) -> Option<gix::Repository> {
    let options = gix::open::Options::isolated().strict_config(true);
    gix::discover_opts(root, gix::discover::upwards::Options::default(), options).ok()
}

/// Each remote with its fetch URL, None for one whose URL is missing or unreadable.
fn named_remotes(root: &Path) -> Vec<(String, Option<String>)> {
    open_repo(root)
        .map(|repo| named_remotes_of(&repo))
        .unwrap_or_default()
}

fn named_remotes_of(repo: &gix::Repository) -> Vec<(String, Option<String>)> {
    repo.remote_names()
        .iter()
        .map(|name| {
            let url = repo
                .try_find_remote(name.as_bstr())
                .and_then(Result::ok)
                .and_then(|remote| {
                    let url = remote.url(gix::remote::Direction::Fetch)?;
                    Some(without_credentials(&url.to_bstring().to_string()))
                });
            (name.to_string(), url)
        })
        .collect()
}

/// The remotes of `root` only when they are known completely: a repository whose config gix
/// reads, with no `include` section (remotes set there are not followed) and a fetch URL on
/// every remote. The outer None means a repository that could not be read completely; the inner
/// None means no repository at `root`.
fn complete_named_remotes(root: &Path) -> Option<Option<Vec<(String, String)>>> {
    let Some(repo) = open_repo(root) else {
        return Some(None);
    };
    let config = repo.config_snapshot();
    let file = config.plumbing();
    if file.sections_by_name("include").is_some() || file.sections_by_name("includeIf").is_some() {
        return None;
    }
    named_remotes_of(&repo)
        .into_iter()
        .map(|(name, url)| Some((name, url?)))
        .collect::<Option<Vec<_>>>()
        .map(Some)
}

/// The remotes of every root, sorted and deduplicated: what a session records at start.
pub fn git_remotes_of(roots: &[PathBuf]) -> Vec<String> {
    let mut remotes: Vec<String> = roots.iter().flat_map(|root| git_remotes(root)).collect();
    remotes.sort();
    remotes.dedup();
    remotes
}

/// The remotes of every root as `<git name> <fetch url>`, sorted and deduplicated: what a session
/// shows the guardian, which judges a call by the git name it uses. A root with no repository
/// contributes nothing. None when a repository's remotes cannot be read completely (an
/// unreadable config, an `include` section, a remote without a fetch URL), or when no root has a
/// repository, so the guardian is not told a gap in what Fontaine knows is "no remotes".
pub fn git_remote_names_of(roots: &[PathBuf]) -> Option<Vec<String>> {
    let mut remotes = Vec::new();
    let mut any_repo = false;
    for root in roots {
        let Some(named) = complete_named_remotes(root)? else {
            continue;
        };
        any_repo = true;
        for (name, url) in named {
            remotes.push(format!("{name} {url}"));
        }
    }
    if !any_repo {
        return None;
    }
    remotes.sort();
    remotes.dedup();
    Some(remotes)
}

/// `https://user:token@host/x` without its `user:token@`; an scp-style `git@host:x` keeps its
/// user, which names no secret.
fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, None), |(a, p)| (a, Some(p)));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    match path {
        Some(path) => format!("{scheme}://{host}/{path}"),
        None => format!("{scheme}://{host}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_stripped_from_remote_urls() {
        assert_eq!(
            without_credentials("https://me:ghp_secret@github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
        assert_eq!(
            without_credentials("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
        assert_eq!(without_credentials("ssh://git@host"), "ssh://host");
    }

    #[test]
    fn a_root_outside_any_repository_records_no_remote_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_remote_names_of(&[dir.path().to_path_buf()]), None);
    }

    #[test]
    fn a_remote_without_a_fetch_url_loses_only_the_complete_name_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join(".git/objects")).unwrap();
        std::fs::create_dir(dir.path().join(".git/refs")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\
             [remote \"origin\"]\n\turl = https://github.com/acme/widget.git\n\
             [remote \"push-only\"]\n\tpushurl = https://github.com/acme/other.git\n",
        )
        .unwrap();
        let root = dir.path();
        assert_eq!(
            git_remotes(root),
            vec!["https://github.com/acme/widget.git".to_string()]
        );
        assert_eq!(
            git_origin(root).as_deref(),
            Some("https://github.com/acme/widget.git")
        );
        assert_eq!(git_remote_names_of(&[root.to_path_buf()]), None);
    }

    #[test]
    fn an_include_section_leaves_the_remote_names_unrecorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        std::fs::create_dir(dir.path().join(".git/refs")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\
             [include]\n\tpath = extra.config\n\
             [remote \"origin\"]\n\turl = https://github.com/acme/widget.git\n",
        )
        .unwrap();
        assert_eq!(git_remote_names_of(&[dir.path().to_path_buf()]), None);
        assert_eq!(git_remotes(dir.path()).len(), 1);
    }

    #[test]
    fn a_non_repository_root_does_not_drop_a_repository_roots_names() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git/objects")).unwrap();
        std::fs::create_dir(repo.path().join(".git/refs")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            repo.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\
             [remote \"origin\"]\n\turl = https://github.com/acme/widget.git\n",
        )
        .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        assert_eq!(
            git_remote_names_of(&[repo.path().to_path_buf(), scratch.path().to_path_buf()]),
            Some(vec![
                "origin https://github.com/acme/widget.git".to_string()
            ])
        );
    }

    fn ledger_with_two_messages(dir: &Path) -> PathBuf {
        let ledger = Ledger::create(dir).unwrap();
        for text in ["one", "two"] {
            ledger
                .append(Record::UserMessage {
                    origin: Default::default(),
                    text: text.into(),
                    attachments: Vec::new(),
                    agent: None,
                })
                .unwrap();
        }
        ledger.path().to_path_buf()
    }

    #[test]
    fn a_torn_last_line_is_cut_off_and_appends_continue_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"seq\":2,\"ts_ms\":1,\"rec");
        std::fs::write(&path, &text).unwrap();

        let ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append(Record::UserMessage {
                origin: Default::default(),
                text: "three".into(),
                attachments: Vec::new(),
                agent: None,
            })
            .unwrap();
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        let lines = std::fs::read_to_string(reopened.path()).unwrap();
        assert_eq!(lines.lines().count(), 3);
        assert!(lines.lines().last().unwrap().contains("three"));
    }

    #[test]
    fn a_complete_last_record_without_its_newline_does_not_join_the_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.trim_end_matches('\n')).unwrap();

        let ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append(Record::UserMessage {
                origin: Default::default(),
                text: "three".into(),
                attachments: Vec::new(),
                agent: None,
            })
            .unwrap();
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        let lines = std::fs::read_to_string(reopened.path()).unwrap();
        assert_eq!(lines.lines().count(), 3);
    }

    #[test]
    fn a_bad_line_before_the_last_still_fails_the_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let text = std::fs::read_to_string(&path).unwrap();
        let (first, rest) = text.split_once('\n').unwrap();
        std::fs::write(&path, format!("{first}\nnot json\n{rest}")).unwrap();
        assert!(Ledger::open(dir.path()).is_err());
    }

    #[test]
    fn a_complete_unparseable_last_line_fails_the_open_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"seq\":2,\"ts_ms\":1,\"record\":{\"kind\":\"from_the_future\"}}\n");
        std::fs::write(&path, &text).unwrap();
        assert!(Ledger::open(dir.path()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_user_shell_record_round_trips_and_is_not_user_intent() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        for exit_code in [Some(2), None] {
            ledger
                .append(Record::UserShell {
                    command: "make".into(),
                    exit_code,
                })
                .unwrap();
        }
        let entries = ledger.read().unwrap();
        assert_eq!(
            entries[0].record,
            Record::UserShell {
                command: "make".into(),
                exit_code: Some(2)
            }
        );
        assert_eq!(
            entries[1].record,
            Record::UserShell {
                command: "make".into(),
                exit_code: None
            }
        );
        assert!(ledger.view().user_messages.is_empty());
    }

    #[test]
    fn a_clear_restarts_the_human_messages_but_keeps_the_workspace_facts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        ledger
            .append(Record::FileIndex {
                paths: vec!["/w/old.txt".into()],
                truncated: false,
            })
            .unwrap();
        ledger
            .append(Record::UserMessage {
                origin: Default::default(),
                text: "before".into(),
                attachments: Vec::new(),
                agent: None,
            })
            .unwrap();
        ledger
            .append(Record::Action {
                tool: "write".into(),
                agent: "main".into(),
                paths: vec!["/w/new.txt".into()],
                created: vec!["/w/new.txt".into()],
                summary: "s".into(),
                tier: None,
                tool_use_id: None,
                digest: None,
            })
            .unwrap();
        ledger
            .append(Record::Denial {
                tool: "shell".into(),
                agent: "main".into(),
                reason: "r".into(),
                rule: None,
                summary: String::new(),
                tier: None,
                tool_use_id: None,
                digest: None,
                paths: Vec::new(),
                converted: None,
                fail_closed: None,
                explanation: None,
            })
            .unwrap();
        ledger.append(Record::ContextCleared {}).unwrap();
        ledger
            .append(Record::UserMessage {
                origin: Default::default(),
                text: "after".into(),
                attachments: Vec::new(),
                agent: None,
            })
            .unwrap();
        let check = |ledger: &Ledger| {
            let view = ledger.view();
            assert_eq!(view.user_messages, ["after"]);
            assert_eq!(view.actions.len(), 1);
            assert_eq!(view.denials.len(), 1);
            assert_eq!(view.existence("/w/old.txt"), Existence::PreExisting);
            assert_eq!(view.existence("/w/new.txt"), Existence::CreatedThisSession);
        };
        check(&ledger);
        drop(ledger);
        check(&Ledger::open(dir.path()).unwrap());
    }

    fn say(ledger: &Ledger, text: &str) {
        ledger
            .append(Record::UserMessage {
                origin: Default::default(),
                text: text.into(),
                attachments: Vec::new(),
                agent: None,
            })
            .unwrap();
    }

    #[test]
    fn a_rewind_trims_the_human_messages_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        say(&ledger, "one");
        say(&ledger, "two");
        ledger
            .append(Record::Approval {
                tool: "shell".into(),
                agent: "main".into(),
                scope: "once".into(),
            })
            .unwrap();
        ledger
            .append(Record::Grant {
                rule: "shell(ls)".into(),
            })
            .unwrap();
        ledger
            .append(Record::Denial {
                tool: "shell".into(),
                agent: "main".into(),
                reason: "r".into(),
                rule: None,
                summary: "rm".into(),
                tier: None,
                tool_use_id: None,
                digest: None,
                paths: Vec::new(),
                converted: None,
                fail_closed: None,
                explanation: None,
            })
            .unwrap();
        say(&ledger, "three");
        ledger
            .append(Record::Rewound {
                turn: 2,
                what: RewindWhat::Both,
                dropped_user_messages: 2,
            })
            .unwrap();
        let check = |ledger: &Ledger| {
            let view = ledger.view();
            assert_eq!(view.user_messages, ["one"]);
            assert_eq!(view.denials.len(), 1);
            // Nothing was removed from the file itself.
            assert_eq!(ledger.read().unwrap().len(), 7);
        };
        check(&ledger);
        drop(ledger);
        check(&Ledger::open(dir.path()).unwrap());
    }

    #[test]
    fn a_code_only_rewind_trims_nothing_and_more_than_held_does_not_underflow() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        say(&ledger, "one");
        ledger
            .append(Record::Rewound {
                turn: 1,
                what: RewindWhat::Code,
                dropped_user_messages: 0,
            })
            .unwrap();
        assert_eq!(ledger.view().user_messages, ["one"]);
        ledger
            .append(Record::Rewound {
                turn: 1,
                what: RewindWhat::Both,
                dropped_user_messages: 9,
            })
            .unwrap();
        assert!(ledger.view().user_messages.is_empty());
    }

    fn act(ledger: &Ledger, tool: &str, paths: &[&str]) {
        ledger
            .append(Record::Action {
                tool: tool.into(),
                agent: "main".into(),
                paths: paths.iter().map(|p| p.to_string()).collect(),
                created: Vec::new(),
                summary: String::new(),
                tier: None,
                tool_use_id: None,
                digest: None,
            })
            .unwrap();
    }

    #[test]
    fn a_recap_counts_the_whole_session_but_the_messages_since_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        assert_eq!(ledger.view().recap(), LedgerRecap::default());
        say(&ledger, "one");
        act(&ledger, "write", &["/w/a.rs"]);
        act(&ledger, "edit", &["/w/a.rs", "/w/b.rs"]);
        act(&ledger, "read", &["/w/c.rs"]);
        act(&ledger, "notebook_edit", &["/w/n.ipynb"]);
        act(&ledger, "shell", &[]);
        act(&ledger, "shell", &[]);
        ledger
            .append(Record::Agent {
                event: "spawned".into(),
                agent: "agent-1".into(),
                parent: None,
                detail: Some("general".into()),
                name: None,
                model: None,
            })
            .unwrap();
        ledger
            .append(Record::Denial {
                tool: "shell".into(),
                agent: "main".into(),
                reason: "r".into(),
                rule: Some("deny(shell)".into()),
                summary: "rm".into(),
                tier: None,
                tool_use_id: None,
                digest: None,
                paths: Vec::new(),
                converted: None,
                fail_closed: None,
                explanation: None,
            })
            .unwrap();
        let check = |ledger: &Ledger, messages: &[&str]| {
            let recap = ledger.view().recap();
            assert_eq!(recap.user_messages, messages);
            assert_eq!(recap.written, ["/w/a.rs", "/w/b.rs", "/w/n.ipynb"]);
            assert_eq!(
                recap.tools,
                [
                    ("shell".to_string(), 2),
                    ("edit".to_string(), 1),
                    ("notebook_edit".to_string(), 1),
                    ("read".to_string(), 1),
                    ("write".to_string(), 1),
                ]
            );
            assert_eq!(recap.denials, 1);
            assert_eq!(recap.last_denial.unwrap().rule, "deny(shell)");
            assert_eq!(recap.agents_spawned, 1);
        };
        check(&ledger, &["one"]);
        say(&ledger, "two");
        ledger
            .append(Record::Rewound {
                turn: 2,
                what: RewindWhat::Both,
                dropped_user_messages: 1,
            })
            .unwrap();
        check(&ledger, &["one"]);
        ledger.append(Record::ContextCleared {}).unwrap();
        check(&ledger, &[]);
        drop(ledger);
        check(&Ledger::open(dir.path()).unwrap(), &[]);
    }

    fn said(ledger: &Ledger, text: &str, agent: Option<&str>) {
        ledger
            .append(Record::UserMessage {
                origin: Default::default(),
                text: text.into(),
                attachments: Vec::new(),
                agent: agent.map(str::to_string),
            })
            .unwrap();
    }

    #[test]
    fn a_message_typed_to_an_agent_is_intent_for_it_and_its_descendants_only() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said(&ledger, "main one", None);
        said(&ledger, "to agent-3", Some("agent-3"));
        said(&ledger, "main two", None);
        said(&ledger, "to agent-4", Some("agent-4"));
        let view = ledger.view();
        assert_eq!(view.user_messages, ["main one", "main two"]);
        let ancestry =
            |ids: &[&str]| -> Vec<String> { ids.iter().map(|s| s.to_string()).collect() };
        // The root hears only the main conversation.
        assert_eq!(view.intent_for(&[]), ["main one", "main two"]);
        // agent-3 hears main and itself, in the order typed; its sibling's words are not its own.
        assert_eq!(
            view.intent_for(&ancestry(&["agent-3"])),
            ["main one", "to agent-3", "main two"]
        );
        // A child of agent-3 inherits what was typed to its ancestor.
        assert_eq!(
            view.intent_for(&ancestry(&["agent-5", "agent-3"])),
            ["main one", "to agent-3", "main two"]
        );
        assert_eq!(
            view.intent_for(&ancestry(&["agent-4"])),
            ["main one", "main two", "to agent-4"]
        );
    }

    #[test]
    fn a_record_without_an_agent_loads_as_the_main_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said(&ledger, "old", None);
        let text = std::fs::read_to_string(ledger.path()).unwrap();
        assert!(!text.contains("\"agent\""), "{text}");
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        assert_eq!(reopened.view().user_messages, ["old"]);
        assert!(reopened.view().agent_messages.is_empty());
        let legacy: Record =
            serde_json::from_str(r#"{"kind":"user_message","text":"legacy"}"#).unwrap();
        assert_eq!(
            legacy,
            Record::UserMessage {
                origin: Default::default(),
                text: "legacy".into(),
                attachments: Vec::new(),
                agent: None
            }
        );
    }

    #[test]
    fn a_rewind_drops_agent_messages_typed_after_the_first_dropped_main_one() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said(&ledger, "one", None);
        said(&ledger, "kept for agent", Some("agent-1"));
        said(&ledger, "two", None);
        said(&ledger, "dropped for agent", Some("agent-1"));
        said(&ledger, "three", None);
        ledger
            .append(Record::Rewound {
                turn: 2,
                what: crate::checkpoint::RewindWhat::Conversation,
                dropped_user_messages: 2,
            })
            .unwrap();
        let view = ledger.view();
        assert_eq!(view.user_messages, ["one"]);
        let kept: Vec<&str> = view
            .agent_messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(kept, ["kept for agent"]);
    }

    #[test]
    fn a_clear_forgets_what_was_typed_to_agents_too() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said(&ledger, "to agent", Some("agent-1"));
        ledger.append(Record::ContextCleared {}).unwrap();
        assert!(ledger.view().agent_messages.is_empty());
    }

    fn said_as(ledger: &Ledger, text: &str, origin: MessageOrigin) {
        ledger
            .append(Record::UserMessage {
                origin,
                text: text.into(),
                attachments: Vec::new(),
                agent: None,
            })
            .unwrap();
    }

    fn replied(ledger: &Ledger, text: &str) {
        ledger
            .append(Record::AgentReply { text: text.into() })
            .unwrap();
    }

    fn pair(ledger: &Ledger, ancestry: &[&str]) -> Option<String> {
        let ancestry: Vec<String> = ancestry.iter().map(|s| s.to_string()).collect();
        ledger
            .view()
            .reply_before_last(&ancestry)
            .map(str::to_string)
    }

    #[test]
    fn the_reply_directly_before_the_latest_prompt_pairs_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "rebase it", MessageOrigin::Prompt);
        assert_eq!(pair(&ledger, &[]), None, "no reply yet");
        replied(&ledger, "Shall I force-push?");
        assert_eq!(pair(&ledger, &[]), None, "the latest message came first");
        said_as(&ledger, "yes", MessageOrigin::Prompt);
        assert_eq!(pair(&ledger, &[]).as_deref(), Some("Shall I force-push?"));
        replied(&ledger, "Pushed.");
        said_as(&ledger, "thanks", MessageOrigin::Prompt);
        assert_eq!(pair(&ledger, &[]).as_deref(), Some("Pushed."));
    }

    #[test]
    fn another_user_message_in_between_leaves_nothing_to_pair() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "task", MessageOrigin::Prompt);
        replied(&ledger, "Shall I?");
        said_as(&ledger, "wait", MessageOrigin::Other);
        said_as(&ledger, "yes", MessageOrigin::Prompt);
        assert_eq!(pair(&ledger, &[]), None);
    }

    #[test]
    fn a_message_typed_to_a_subagent_after_it_unpairs_even_when_the_text_matches() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "task", MessageOrigin::Prompt);
        replied(&ledger, "Shall I?");
        said_as(&ledger, "yes", MessageOrigin::Prompt);
        assert_eq!(pair(&ledger, &["agent-1"]).as_deref(), Some("Shall I?"));
        said(&ledger, "yes", Some("agent-1"));
        assert_eq!(pair(&ledger, &["agent-1"]), None);
        assert_eq!(pair(&ledger, &[]).as_deref(), Some("Shall I?"));
        assert_eq!(pair(&ledger, &["agent-2"]).as_deref(), Some("Shall I?"));
    }

    #[test]
    fn several_replies_at_one_position_pair_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "task", MessageOrigin::Prompt);
        replied(&ledger, "first answer");
        replied(&ledger, "answer to a harness-started turn");
        said_as(&ledger, "yes", MessageOrigin::Prompt);
        assert_eq!(
            pair(&ledger, &[]).as_deref(),
            Some("answer to a harness-started turn")
        );
    }

    #[test]
    fn a_dialog_answer_or_steered_message_never_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "task", MessageOrigin::Prompt);
        replied(&ledger, "Shall I?");
        // A harness-started turn asks a question, and the answer lands while the older reply is
        // the last one recorded.
        said_as(
            &ledger,
            "Answered the agent's question \"Q\": picked \"a\"",
            MessageOrigin::Dialog,
        );
        assert_eq!(pair(&ledger, &[]), None);
        said_as(&ledger, "yes, steered mid-turn", MessageOrigin::Other);
        assert_eq!(pair(&ledger, &[]), None);
    }

    #[test]
    fn a_message_from_a_ledger_without_origins_never_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "task", MessageOrigin::Prompt);
        replied(&ledger, "Shall I?");
        let legacy: Record =
            serde_json::from_str(r#"{"kind":"user_message","text":"yes"}"#).unwrap();
        ledger.append(legacy).unwrap();
        assert_eq!(pair(&ledger, &[]), None);
    }

    #[test]
    fn a_clear_drops_the_replies_and_a_rewind_keeps_those_at_or_before_the_kept_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "one", MessageOrigin::Prompt);
        replied(&ledger, "answer one");
        said_as(&ledger, "two", MessageOrigin::Prompt);
        replied(&ledger, "answer two");
        said_as(&ledger, "three", MessageOrigin::Prompt);
        replied(&ledger, "answer three");
        ledger
            .append(Record::Rewound {
                turn: 2,
                what: crate::checkpoint::RewindWhat::Conversation,
                dropped_user_messages: 2,
            })
            .unwrap();
        {
            let view = ledger.view();
            assert_eq!(view.user_messages, ["one"]);
            let kept: Vec<&str> = view.agent_replies.iter().map(|r| r.text.as_str()).collect();
            assert_eq!(kept, ["answer one"]);
        }
        said_as(&ledger, "retry", MessageOrigin::Prompt);
        assert_eq!(pair(&ledger, &[]).as_deref(), Some("answer one"));
        ledger.append(Record::ContextCleared {}).unwrap();
        assert!(ledger.view().agent_replies.is_empty());
        assert_eq!(pair(&ledger, &[]), None);
    }

    #[test]
    fn replies_and_origins_replay_from_a_persisted_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        said_as(&ledger, "task", MessageOrigin::Prompt);
        replied(&ledger, "Shall I?");
        said_as(&ledger, "yes", MessageOrigin::Prompt);
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        assert_eq!(pair(&reopened, &[]).as_deref(), Some("Shall I?"));
    }

    fn agent_event(event: &str, agent: &str, parent: Option<&str>, detail: Option<&str>) -> Record {
        Record::Agent {
            event: event.into(),
            agent: agent.into(),
            parent: parent.map(str::to_string),
            detail: detail.map(str::to_string),
            name: None,
            model: None,
        }
    }

    #[test]
    fn agent_records_fold_into_spawn_order_with_the_finished_status() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        ledger
            .append(agent_event(
                "spawned",
                "agent-1",
                Some("main"),
                Some("worker"),
            ))
            .unwrap();
        ledger
            .append(agent_event(
                "spawned",
                "agent-2",
                Some("agent-1"),
                Some("explore"),
            ))
            .unwrap();
        ledger
            .append(agent_event("finished", "agent-1", None, Some("done")))
            .unwrap();
        let check = |ledger: &Ledger| {
            let view = ledger.view();
            assert_eq!(
                view.agents,
                [
                    AgentRecord {
                        id: "agent-1".into(),
                        name: "agent-1".into(),
                        parent: Some("main".into()),
                        agent_type: "worker".into(),
                        model: None,
                        ended: Some("done".into()),
                    },
                    AgentRecord {
                        id: "agent-2".into(),
                        name: "agent-2".into(),
                        parent: Some("agent-1".into()),
                        agent_type: "explore".into(),
                        model: None,
                        ended: None,
                    },
                ]
            );
            assert_eq!(view.next_agent, 3);
        };
        check(&ledger);
        drop(ledger);
        check(&Ledger::open(dir.path()).unwrap());
    }

    #[test]
    fn a_spawn_record_carries_the_model_and_a_retired_end_marks_the_agent_final() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        ledger
            .append(Record::Agent {
                event: "spawned".into(),
                agent: "agent-1".into(),
                parent: Some("root".into()),
                detail: Some("worker".into()),
                name: Some("w".into()),
                model: Some("acct/model".into()),
            })
            .unwrap();
        ledger
            .append(agent_event("finished", "agent-1", None, Some("done")))
            .unwrap();
        assert_eq!(ledger.view().agents[0].model.as_deref(), Some("acct/model"));
        assert_eq!(ledger.view().agents[0].ended.as_deref(), Some("done"));
        ledger
            .append(agent_event("finished", "agent-1", None, Some("retired")))
            .unwrap();
        assert_eq!(ledger.view().agents[0].ended.as_deref(), Some("retired"));
    }

    #[test]
    fn a_spawn_record_carries_its_name_and_a_legacy_one_falls_back_to_the_id() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        ledger
            .append(Record::Agent {
                event: "spawned".into(),
                agent: "agent-1".into(),
                parent: Some("root".into()),
                detail: Some("worker".into()),
                name: Some("survey-tech-debt".into()),
                model: None,
            })
            .unwrap();
        let legacy: Record = serde_json::from_str(
            r#"{"kind":"agent","event":"spawned","agent":"agent-2","parent":"root","detail":"worker"}"#,
        )
        .unwrap();
        ledger.append(legacy).unwrap();
        let check = |ledger: &Ledger| {
            let names: Vec<String> = ledger
                .view()
                .agents
                .iter()
                .map(|a| a.name.clone())
                .collect();
            assert_eq!(names, ["survey-tech-debt", "agent-2"]);
        };
        check(&ledger);
        drop(ledger);
        check(&Ledger::open(dir.path()).unwrap());
    }

    #[test]
    fn next_agent_ignores_non_conforming_ids_and_an_empty_ledger_starts_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        assert_eq!(ledger.view().next_agent, 1);
        ledger
            .append(agent_event("spawned", "custom-x", None, Some("worker")))
            .unwrap();
        ledger
            .append(agent_event(
                "spawned",
                "agent-7",
                Some("main"),
                Some("worker"),
            ))
            .unwrap();
        ledger
            .append(agent_event(
                "spawned",
                "agent-2",
                Some("main"),
                Some("worker"),
            ))
            .unwrap();
        assert_eq!(ledger.view().next_agent, 8);
        assert_eq!(ledger.view().agents.len(), 3);
    }

    #[test]
    fn a_finished_then_resumed_agent_is_unfinished_until_a_new_finished_record() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::create(dir.path()).unwrap();
        ledger
            .append(agent_event(
                "spawned",
                "agent-1",
                Some("main"),
                Some("worker"),
            ))
            .unwrap();
        ledger
            .append(agent_event("finished", "agent-1", None, Some("done")))
            .unwrap();
        ledger
            .append(agent_event("resumed", "agent-1", None, None))
            .unwrap();
        assert_eq!(ledger.view().agents[0].ended, None);
        // The session crashed mid-second-run: still unfinished, not the stale done.
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        assert_eq!(reopened.view().agents[0].ended, None);
        assert_eq!(reopened.view().next_agent, 2);
    }
}
