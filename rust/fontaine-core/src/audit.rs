//! The audit log: one typed record per gate decision, appended to an owner-only JSONL file per
//! session before anything else sees the decision. It is the security record, so it is
//! written whatever `[telemetry]` says; telemetry and the headless `gate_decision` event are
//! projections of the same record.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::owner_only;
use crate::tier::Tier;
use fontaine_tools::lock::lock;

pub const AUDIT_DIR: &str = "audit";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    Prompt,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
            Decision::Prompt => "prompt",
        }
    }
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One gate decision. A prompt that a human then answers adds a second record with
/// `tier = human`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// RFC 3339, UTC.
    pub ts: String,
    pub session_id: String,
    pub agent_id: String,
    /// The model's id for the call; absent when the call did not come through an agent run.
    pub tool_use_id: Option<String>,
    pub tool: String,
    /// SHA-256 over the RFC 8785 serialization of the arguments; the arguments themselves are
    /// never logged.
    pub args_digest: String,
    /// The digest of the model's arguments when a hook rewrote the call.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub original_args_digest: Option<String>,
    pub tier: Tier,
    pub decision: Decision,
    /// The rule text and its source scope, hook id, or floor check that decided; null if none.
    pub rule: Option<String>,
    pub confidence: Option<f64>,
    pub guardian_model: Option<String>,
    pub latency_ms: u64,
    /// `full` or `minimal`, for guardian decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_profile: Option<String>,
    /// What the guardian's adjudication came to: `allow`, `deny` or `escalate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_cache_hit: Option<bool>,
    /// `tool` or `schema`: the transport the verdict was obtained on. Absent when the model could not be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_transport: Option<String>,
    /// How long the guardian took, apart from the rest of the pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_latency_ms: Option<u64>,
    /// `error`, `timeout`, `unparseable`, `low_confidence` or `unavailable`, when the guardian
    /// failed closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_closed_reason: Option<String>,
    /// The screen stage's risk score, when it scored the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_risk: Option<f64>,
    /// The screen's hardened allow read, when the allow reads ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_allow_risk: Option<f64>,
    /// The screen's steering detector score, when the allow reads ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_steering: Option<f64>,
    /// `<account>/<model>` of the screen that scored, or tried to score, the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_latency_ms: Option<u64>,
    /// `stop`, `allow`, `pass` or `unavailable`. In shadow mode `stop` and `allow` are what the
    /// screen would have done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_outcome: Option<String>,
    /// An estimate of the input tokens of the guardian request a screen that settled the call
    /// cut short; the request had usually been sent and billed already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_dropped_input_tokens: Option<u64>,
    /// The provider name of the model that estimate was spent on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_dropped_provider: Option<String>,
    /// The model id of the model that estimate was spent on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_dropped_model: Option<String>,
    /// Whether that model's cost is counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_dropped_priced: Option<bool>,
    pub permission_mode: String,
    pub headless: bool,
    /// What the model was told for a denial.
    pub explanation: Option<String>,
}

/// SHA-256 over the canonical JSON of `args`, lowercase hex.
pub fn args_digest(args: &Value) -> String {
    // Canonicalization of a `Value` cannot fail; fall back to the plain form rather than skip
    // the record.
    let bytes = serde_jcs::to_vec(args).unwrap_or_else(|_| args.to_string().into_bytes());
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The current time as RFC 3339 UTC with millisecond precision.
pub fn now_rfc3339() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(since.as_secs(), since.subsec_millis())
}

fn rfc3339(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

pub struct AuditLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl AuditLog {
    /// Starts the audit log of `session_id` under `<data dir>/audit/`.
    pub fn create(data_dir: &Path, session_id: &str) -> io::Result<AuditLog> {
        let dir = data_dir.join(AUDIT_DIR);
        match owner_only::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let path = dir.join(format!("{session_id}.jsonl"));
        let file = owner_only::open(&path, true)?;
        Ok(AuditLog {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends and flushes one record.
    pub fn append(&self, record: &DecisionRecord) -> io::Result<()> {
        let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut file = lock(&self.file, "audit file");
        file.write_all(&line)?;
        file.flush()
    }

    /// Every record written so far, in order.
    pub fn read(&self) -> io::Result<Vec<DecisionRecord>> {
        std::fs::read_to_string(&self.path)?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(io::Error::other))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_rfc_3339() {
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_709_210_096, 7), "2024-02-29T12:34:56.007Z");
        assert_eq!(rfc3339(951_782_400, 0), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn the_digest_ignores_key_order() {
        let a = serde_json::json!({"a": 1, "b": [1, 2]});
        let b = serde_json::json!({"b": [1, 2], "a": 1});
        assert_eq!(args_digest(&a), args_digest(&b));
        assert_ne!(args_digest(&a), args_digest(&serde_json::json!({"a": 2})));
        assert_eq!(args_digest(&a).len(), 64);
    }
}
