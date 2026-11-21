//! The status line: the Rune script that renders the bar (`script`, `snapshot`) and the optional
//! user command that receives JSON session data on stdin and whose first stdout line the script
//! can place (`StatuslineCommand`). The command is off when none is configured.

mod script;
mod snapshot;

pub use script::{DEFAULT_SCRIPT, STATUSLINE_BUDGET, StatuslineError, StatuslineProgram};
pub use snapshot::{Role, SEGMENTS, STYLE_NAMES, StatuslineSnapshot, StyledText, VALUES, Values};

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::tools::command_shell::CommandShell;
use crate::tools::procgroup::{TreeGuard, collect_output};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::core::audit::DecisionRecord;
use crate::core::config::PermissionMode;
use crate::core::quota::AccountUsage;

/// How long a status-line command may run before it is killed. Not configurable in this
/// iteration; a hung script must not stall the bar.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// `FONTAINE_PROJECT_DIR`, set on the child so a script can locate the project it runs for.
pub const PROJECT_DIR_VAR: &str = "FONTAINE_PROJECT_DIR";

/// The status line as last rendered: the styled runs the bar draws after its safety badges.
#[derive(Clone, Debug, Default)]
pub struct StatuslineState {
    pub runs: Vec<StyledText>,
}

/// One recent gate decision carrying a guardian verdict, as exposed to the script.
#[derive(Clone, Debug, Serialize)]
pub struct RecentVerdict {
    pub tool: String,
    pub decision: String,
    pub rule: Option<String>,
    pub guardian_verdict: String,
}

impl From<&DecisionRecord> for RecentVerdict {
    fn from(record: &DecisionRecord) -> Self {
        RecentVerdict {
            tool: record.tool.clone(),
            decision: record.decision.as_str().to_string(),
            rule: record.rule.clone(),
            guardian_verdict: record
                .guardian_verdict
                .clone()
                .expect("only records with a guardian_verdict are kept"),
        }
    }
}

/// The JSON payload the custom status line command receives on stdin.
#[derive(Clone, Debug, Serialize)]
pub struct Payload {
    pub model: ModelInfo,
    pub cwd: String,
    pub workspace: WorkspaceInfo,
    pub session_id: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortInfo>,
    pub mode: String,
    pub guardian: GuardianInfo,
    pub recent_verdicts: Vec<RecentVerdict>,
    /// AFK mode: whether it is on, and whether the temperature ceiling has stopped the session.
    pub afk: AfkInfo,
    pub account_usage: Vec<serde_json::Value>,
    /// The session's activity, the object the `activity_changed` hook carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindowInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceInfo {
    pub current_dir: String,
    pub project_dir: String,
    pub added_dirs: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffortInfo {
    pub level: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct GuardianInfo {
    pub model: Option<String>,
    pub off: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct AfkInfo {
    pub on: bool,
    pub stopped: bool,
}

impl Payload {
    /// The payload with the AFK fields set; `build_payload` leaves them off.
    pub fn with_afk(mut self, on: bool, stopped: bool) -> Payload {
        self.afk = AfkInfo { on, stopped };
        self
    }

    /// The payload with the session's activity set; `build_payload` leaves it off.
    pub fn with_activity(mut self, activity: &crate::core::activity::SessionActivity) -> Payload {
        self.activity = Some(activity.to_json());
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ContextWindowInfo {
    pub used_percentage: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct CostInfo {
    /// The session's spend. For a run whose model is priced by estimate, this is a list-price
    /// estimate of what the tokens would have cost on the served vendor's own API.
    pub total_cost_usd: f64,
}

/// A configured status-line command and the environment it runs in.
#[derive(Clone)]
pub struct StatuslineCommand {
    command: String,
    scrub_env: Vec<OsString>,
    set_env: Vec<(OsString, OsString)>,
    shell: CommandShell,
    project_dir: PathBuf,
    timeout: Duration,
}

impl StatuslineCommand {
    /// Builds a command runner from the runtime's configuration, matching the environment posture
    /// of hooks: scrubbed env, `[env]`, `FONTAINE_PROJECT_DIR`, and the same shell.
    pub fn new(
        command: impl Into<String>,
        scrub_env: Vec<OsString>,
        set_env: Vec<(OsString, OsString)>,
        shell: CommandShell,
        project_dir: PathBuf,
    ) -> StatuslineCommand {
        StatuslineCommand {
            command: command.into(),
            scrub_env,
            set_env,
            shell,
            project_dir,
            timeout: TIMEOUT,
        }
    }

    /// Overrides the timeout for tests that need a faster ceiling.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs the command with the payload on stdin. Returns the first line of stdout on exit 0
    /// with non-empty output; otherwise returns `None`. Stderr is captured for the telemetry log
    /// but never shown in the UI.
    pub async fn run(&self, payload: &Payload) -> Option<String> {
        match self.run_inner(payload).await {
            Ok(line) => line,
            Err(e) => {
                tracing::debug!(error = ?e, "status line command failed");
                None
            }
        }
    }

    async fn run_inner(&self, payload: &Payload) -> Result<Option<String>, std::io::Error> {
        let invocation = self.shell.invocation(&self.command, &self.set_env);
        let mut child = Command::new(&invocation.program);
        child
            .args(&invocation.args)
            .current_dir(&self.project_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in &self.scrub_env {
            child.env_remove(name);
        }
        for (name, value) in &self.set_env {
            child.env(name, value);
        }
        child.env(PROJECT_DIR_VAR, &self.project_dir);
        for (name, value) in &invocation.env {
            child.env(name, value);
        }

        // A group kill must never reach our own group.
        #[cfg(unix)]
        child.process_group(0);

        let mut guard = TreeGuard::new(child.spawn()?);
        let process = guard.child();
        if let Some(mut input) = process.stdin.take() {
            let bytes =
                serde_json::to_vec(payload).expect("the payload is a serializable JSON object");
            tokio::spawn(async move {
                let _ = input.write_all(&bytes).await;
                let _ = input.shutdown().await;
            });
        }

        // On timeout the guard drops here, killing the whole tree so the pipes a descendant
        // inherited close.
        let Ok(output) = tokio::time::timeout(self.timeout, collect_output(process)).await else {
            return Ok(None);
        };
        let output = output?;
        guard.disarm();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                tracing::debug!(stderr = %stderr, "status line command exited with an error");
            }
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text
            .lines()
            .next()
            .map(strip_ansi)
            .filter(|l| !l.is_empty());
        Ok(line)
    }
}

/// Strips ANSI escape sequences from `text`, keeping only the visible characters. Multi-line
/// and colored output are deferred; the built-in bar is one row of plain styled spans.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            // OSC (`ESC ]`) runs until BEL or ST (`ESC \`), so hyperlinks like
            // `ESC ] 8 ; ; url ST` drop as one unit.
            Some(']') => {
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\u{7}' || (prev == '\u{1b}' && c == '\\') {
                        break;
                    }
                    prev = c;
                }
            }
            // CSI (`ESC [`): parameter/intermediate bytes, then a final byte 0x40–0x7E.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // A lone ESC at end of input or the tail of a two-byte sequence drops.
            Some(_) | None => {}
        }
    }
    out
}

/// Builds the payload from the pieces the TUI assembles. Kept as a free function so the TUI
/// can read live state and pass it in without exposing runtime internals to serialization.
// One argument per independently-sourced piece of live TUI state; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub fn build_payload(
    model: String,
    cwd: &Path,
    project_dir: &Path,
    added_dirs: Vec<String>,
    session_id: String,
    version: String,
    effort: Option<crate::core::effort::Effort>,
    mode: PermissionMode,
    guardian_model: Option<String>,
    guardian_off: bool,
    recent_verdicts: Vec<RecentVerdict>,
    account_usage: Vec<AccountUsage>,
    context_used: Option<u64>,
    context_window: Option<u32>,
    cost_total_usd: Option<f64>,
) -> Payload {
    Payload {
        model: ModelInfo {
            id: model.clone(),
            display_name: model,
        },
        cwd: cwd.display().to_string(),
        workspace: WorkspaceInfo {
            current_dir: cwd.display().to_string(),
            project_dir: project_dir.display().to_string(),
            added_dirs,
        },
        session_id,
        version,
        effort: effort.map(|e| EffortInfo {
            level: e.as_str().to_string(),
        }),
        mode: mode.as_str().to_string(),
        guardian: GuardianInfo {
            model: guardian_model,
            off: guardian_off,
        },
        recent_verdicts,
        afk: AfkInfo::default(),
        account_usage: account_usage.iter().map(AccountUsage::json).collect(),
        activity: None,
        context_window: context_used
            .zip(context_window)
            .and_then(|(used, window)| used_percentage(used, window))
            .map(|used_percentage| ContextWindowInfo { used_percentage }),
        cost: cost_total_usd.map(|total_cost_usd| CostInfo { total_cost_usd }),
    }
}

/// The share of the context window in use, rounded; `None` for a zero window.
pub fn used_percentage(used: u64, window: u32) -> Option<u32> {
    (window > 0).then(|| ((used as f64 / f64::from(window)) * 100.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(command: &str) -> StatuslineCommand {
        StatuslineCommand::new(
            command,
            Vec::new(),
            Vec::new(),
            CommandShell::Sh,
            std::env::temp_dir(),
        )
    }

    fn payload() -> Payload {
        build_payload(
            "test/model".into(),
            Path::new("/work"),
            Path::new("/work"),
            Vec::new(),
            "s1".into(),
            "0.0.0".into(),
            None,
            PermissionMode::Auto,
            Some("guardian/model".into()),
            false,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn echo_round_trips_the_first_line() {
        let line = script("cat")
            .run(&payload())
            .await
            .expect("a line from a successful command");
        assert!(line.contains("\"session_id\":\"s1\""), "{line}");
    }

    #[tokio::test]
    async fn only_the_first_stdout_line_is_kept() {
        let line = script("echo 'first'; echo 'second'")
            .run(&payload())
            .await
            .expect("a line");
        assert_eq!(line, "first");
    }

    #[tokio::test]
    async fn ansi_escapes_are_stripped() {
        let line = script("printf '\\033[31mred\\033[0m'")
            .run(&payload())
            .await
            .expect("a line");
        assert_eq!(line, "red");
    }

    #[tokio::test]
    async fn non_zero_exit_yields_no_line() {
        let result = script("exit 1").run(&payload()).await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn empty_stdout_falls_back() {
        let result = script("printf ''").run(&payload()).await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn timeout_keeps_the_previous_line() {
        let result = script("sleep 5")
            .with_timeout(Duration::from_millis(50))
            .run(&payload())
            .await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn timeout_stops_a_backgrounded_descendant_holding_the_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let command = crate::core::test_pid::sleeper_command(&pid_file);
        let result = script(&command)
            .with_timeout(Duration::from_millis(500))
            .run(&payload())
            .await;
        assert_eq!(result, None);
        crate::core::test_pid::assert_gone(&pid_file).await;
    }

    #[test]
    fn strip_ansi_removes_sequences() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m"), "bold");
        assert_eq!(strip_ansi("no escape"), "no escape");
        assert_eq!(
            strip_ansi("\x1b]8;;https://x\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
    }

    #[test]
    fn payload_omits_absent_values() {
        let p = build_payload(
            "a/b".into(),
            Path::new("/w"),
            Path::new("/w"),
            vec!["/extra".into()],
            "s".into(),
            env!("CARGO_PKG_VERSION").into(),
            None,
            PermissionMode::Manual,
            None,
            false,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
        );
        let value = serde_json::to_value(&p).unwrap();
        assert!(value.get("effort").is_none());
        assert!(value.get("context_window").is_none());
        assert!(value.get("cost").is_none());
        assert_eq!(value["guardian"]["model"], serde_json::Value::Null);
        assert_eq!(
            value["workspace"]["added_dirs"],
            serde_json::json!(["/extra"])
        );
        assert!(value["version"].as_str().unwrap().contains('.'));
    }

    #[test]
    fn context_window_used_percentage_rounds() {
        let p = build_payload(
            "a/b".into(),
            Path::new("/w"),
            Path::new("/w"),
            Vec::new(),
            "s".into(),
            "0".into(),
            None,
            PermissionMode::Auto,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Some(1_600),
            Some(10_000),
            None,
        );
        assert_eq!(p.context_window.unwrap().used_percentage, 16);
    }

    #[test]
    fn verdict_ring_entry_serializes() {
        let record = DecisionRecord {
            ts: "t".into(),
            session_id: "s".into(),
            agent_id: "a".into(),
            agent_name: None,
            tool_use_id: None,
            tool: "shell".into(),
            args_digest: "d".into(),
            original_args_digest: None,
            tier: crate::core::tier::Tier::Guardian,
            decision: crate::core::audit::Decision::Allow,
            rule: Some("rule".into()),
            confidence: None,
            guardian_model: None,
            latency_ms: 0,
            guardian_profile: None,
            guardian_verdict: Some("allow".into()),
            guardian_cache_hit: None,
            verdict_transport: None,
            guardian_latency_ms: None,
            fail_closed_reason: None,
            screen_risk: None,
            screen_allow_risk: None,
            screen_steering: None,
            screen_scores: None,
            screen_model: None,
            screen_outcome: None,
            screen_latency_ms: None,
            guardian_dropped_input_tokens: None,
            guardian_dropped_provider: None,
            guardian_dropped_model: None,
            guardian_dropped_priced: None,
            permission_mode: "auto".into(),
            headless: false,
            human_channel: "present".into(),
            explanation: None,
            afk: false,
            converted: None,
        };
        let v = RecentVerdict::from(&record);
        assert_eq!(v.tool, "shell");
        assert_eq!(v.guardian_verdict, "allow");
    }
}
