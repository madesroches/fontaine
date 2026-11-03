//! Custom status line: a user command that receives JSON session data on stdin and whose
//! stdout replaces the built-in TUI status bar. The feature is off when no command is configured.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use fontaine_tools::git_bash::{GitBash, sh_command};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::audit::DecisionRecord;
use crate::config::PermissionMode;
use crate::quota::AccountUsage;

/// How long a status-line command may run before it is killed. Not configurable in this
/// iteration; a hung script must not stall the bar.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// `FONTAINE_PROJECT_DIR`, set on the child so a script can locate the project it runs for.
pub const PROJECT_DIR_VAR: &str = "FONTAINE_PROJECT_DIR";

/// The last good custom status line produced by the configured command.
#[derive(Clone, Debug, Default)]
pub struct StatuslineState {
    pub line: String,
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
    pub account_usage: Vec<serde_json::Value>,
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

#[derive(Clone, Debug, Serialize)]
pub struct ContextWindowInfo {
    pub used_percentage: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct CostInfo {
    pub total_cost_usd: f64,
}

/// A configured status-line script and the environment it runs in.
#[derive(Clone)]
pub struct StatuslineScript {
    command: String,
    scrub_env: Vec<OsString>,
    set_env: Vec<(OsString, OsString)>,
    git_bash: Option<GitBash>,
    project_dir: PathBuf,
    timeout: Duration,
}

impl StatuslineScript {
    /// Builds a script runner from the runtime's configuration, matching the environment posture
    /// of hooks: scrubbed env, `[env]`, `FONTAINE_PROJECT_DIR`, and Git Bash on Windows.
    pub fn new(
        command: impl Into<String>,
        scrub_env: Vec<OsString>,
        set_env: Vec<(OsString, OsString)>,
        git_bash: Option<GitBash>,
        project_dir: PathBuf,
    ) -> StatuslineScript {
        StatuslineScript {
            command: command.into(),
            scrub_env,
            set_env,
            git_bash,
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
        let sh = sh_command(&self.command, self.git_bash.is_some());
        let mut child = Command::new("sh");
        child
            .arg("-c")
            .arg(sh.script)
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
        if let Some((name, value)) = sh.env {
            child.env(name, value);
        }
        if let Some(git_bash) = &self.git_bash {
            child.env(
                "PATH",
                git_bash.path_value(fontaine_tools::git_bash::base_path(&self.set_env)),
            );
        }

        let mut process = child.spawn()?;
        if let Some(mut input) = process.stdin.take() {
            let bytes =
                serde_json::to_vec(payload).expect("the payload is a serializable JSON object");
            tokio::spawn(async move {
                let _ = input.write_all(&bytes).await;
                let _ = input.shutdown().await;
            });
        }

        let result = tokio::time::timeout(self.timeout, process.wait_with_output()).await;
        let output = match result {
            Ok(output) => output?,
            Err(_) => {
                return Ok(None);
            }
        };
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
    effort: Option<crate::effort::Effort>,
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
        account_usage: account_usage.iter().map(AccountUsage::json).collect(),
        context_window: match (context_used, context_window) {
            (Some(used), Some(window)) if window > 0 => Some(ContextWindowInfo {
                used_percentage: ((used as f64 / f64::from(window)) * 100.0).round() as u32,
            }),
            _ => None,
        },
        cost: cost_total_usd.map(|total_cost_usd| CostInfo { total_cost_usd }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(command: &str) -> StatuslineScript {
        StatuslineScript::new(command, Vec::new(), Vec::new(), None, std::env::temp_dir())
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
            tier: crate::tier::Tier::Guardian,
            decision: crate::audit::Decision::Allow,
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
            explanation: None,
        };
        let v = RecentVerdict::from(&record);
        assert_eq!(v.tool, "shell");
        assert_eq!(v.guardian_verdict, "allow");
    }
}
