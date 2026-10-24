//! Commands the human runs with `!`. They are the user's own action: they run as the user,
//! outside the permission pipeline and the guardian, and are written to the ledger and the audit
//! log so the session still shows they happened.

use fontaine_tools::builtins::{ShellSettings, shell_for};
use rig_core::tool::ExitStatus;
use rig_tools::{ShellArgs, ShellRun};
use serde_json::json;

use crate::agent::AgentRuntime;
use crate::audit::{Decision, DecisionRecord, args_digest, now_rfc3339};
use crate::ledger::Record;
use crate::permissions::record_decision;
use crate::tier::Tier;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserShellRun {
    pub command: String,
    pub run: ShellRun,
}

impl UserShellRun {
    /// The exit code; `None` when the command did not exit with one.
    pub fn exit_code(&self) -> Option<i32> {
        match self.run.status {
            ExitStatus::Exited(code) => Some(code),
            _ => None,
        }
    }
}

impl AgentRuntime {
    /// Runs `command` in the primary root with the agent's shell defaults (killed after 120 s, or
    /// 60 s without output; no stdin), the scrubbed environment and the same output cut, because
    /// the output goes to the model. Dropping the future kills the process group. Writes no
    /// record: the caller does, so an interrupted command is recorded too.
    pub async fn run_user_shell(&self, command: &str) -> Result<UserShellRun, String> {
        let shell = shell_for(
            &self.roots[0],
            &ShellSettings {
                scrub_env: self.setup.scrub_env.clone(),
                git_bash: self.setup.git_bash.clone(),
                ..ShellSettings::default()
            },
        );
        let run = shell
            .run(ShellArgs::new(command))
            .await
            .map_err(|e| e.to_string())?;
        Ok(UserShellRun {
            command: command.to_string(),
            run,
        })
    }

    /// The ledger record and the audit record of a `!` command, for one that finished
    /// (`exit_code` its code) and for one that was interrupted (`None`).
    pub fn record_user_shell(&self, command: &str, exit_code: Option<i32>, latency_ms: u64) {
        self.append(Record::UserShell {
            command: command.to_string(),
            exit_code,
        });
        record_decision(
            &self.state,
            &DecisionRecord {
                ts: now_rfc3339(),
                session_id: self.session.id.clone(),
                agent_id: "user".to_string(),
                tool_use_id: None,
                tool: "shell".to_string(),
                args_digest: args_digest(&json!({ "command": command })),
                original_args_digest: None,
                tier: Tier::Human,
                decision: Decision::Allow,
                rule: Some("user_shell".to_string()),
                confidence: None,
                guardian_model: None,
                latency_ms,
                guardian_profile: None,
                guardian_verdict: None,
                guardian_cache_hit: None,
                guardian_latency_ms: None,
                fail_closed_reason: None,
                screen_risk: None,
                screen_model: None,
                screen_latency_ms: None,
                screen_outcome: None,
                permission_mode: self.state.mode().as_str().to_string(),
                headless: false,
                explanation: None,
            },
        );
    }
}
