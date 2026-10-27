//! Differential baselines: what `claude -p` does with the same call, and where Fontaine
//! differs. Manual and eval-side only; never run from tests or CI.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use fontaine_core::config::PermissionMode;
use fontaine_core::permission_rules::parse_rule;
use fontaine_tools::names::is_mcp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::case::{Case, Kind, MaterializeError, materialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CcOutcome {
    Allowed,
    Denied,
}

/// One line of a baseline file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRecord {
    pub case_id: String,
    pub mode: String,
    pub cc_outcome: CcOutcome,
    pub claude: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Denial {
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
}

const TOOL_NAMES: [(&str, &str); 10] = [
    ("shell", "Bash"),
    ("read", "Read"),
    ("write", "Write"),
    ("edit", "Edit"),
    ("notebook_edit", "NotebookEdit"),
    ("glob", "Glob"),
    ("grep", "Grep"),
    ("webfetch", "WebFetch"),
    ("websearch", "WebSearch"),
    ("agent", "Agent"),
];

/// Claude Code's name for a Fontaine tool; `mcp__` names are the same on both sides.
pub fn claude_tool(tool: &str) -> Option<String> {
    if is_mcp(tool) {
        return Some(tool.to_string());
    }
    TOOL_NAMES
        .iter()
        .find(|(ours, _)| *ours == tool)
        .map(|(_, theirs)| (*theirs).to_string())
}

/// A Fontaine rule as Claude Code rules. A `shell` deny or ask rule without a trailing `*` is a
/// prefix match in Fontaine but exact in Claude Code, so it becomes both `Bash(w)` and
/// `Bash(w *)`.
pub fn claude_rules(posture: &str, rule: &str) -> Result<Vec<String>, String> {
    let parsed = parse_rule(rule).map_err(|e| e.to_string())?;
    let tool = claude_tool(&parsed.tool).ok_or_else(|| {
        format!(
            "rule `{rule}`: `{}` has no Claude Code equivalent",
            parsed.tool
        )
    })?;
    let Some(spec) = parsed.specifier else {
        return Ok(vec![tool]);
    };
    let prefix_rule =
        parsed.tool == "shell" && matches!(posture, "deny" | "ask") && !spec.ends_with('*');
    if prefix_rule {
        Ok(vec![format!("{tool}({spec})"), format!("{tool}({spec} *)")])
    } else {
        Ok(vec![format!("{tool}({spec})")])
    }
}

/// The `.claude/settings.json` content equivalent to a case's `permissions`.
pub fn claude_settings(case: &Case) -> Result<Value, String> {
    let mut out = BTreeMap::new();
    let permissions = case.permissions.clone().unwrap_or_default();
    for (posture, rules) in [
        ("deny", &permissions.deny),
        ("ask", &permissions.ask),
        ("allow", &permissions.allow),
    ] {
        let mut mapped = Vec::new();
        for rule in rules {
            mapped.extend(claude_rules(posture, rule)?);
        }
        out.insert(posture, mapped);
    }
    Ok(json!({ "permissions": out }))
}

/// Why `capture` cannot pose this case to Claude Code, if it cannot.
pub fn capture_skip_reason(case: &Case) -> Option<String> {
    if case.kind != Kind::Pipeline {
        return Some("not a pipeline case".into());
    }
    if case
        .permissions
        .as_ref()
        .is_some_and(|p| !p.allow_extra.is_empty())
    {
        return Some("sets `allow_extra`, which Claude Code has no equivalent for".into());
    }
    for (key, set) in [
        ("session_facts", case.session_facts.is_some()),
        ("auto_mode", case.auto_mode.is_some()),
        ("environment", case.environment.is_some()),
        ("recent_user_messages", case.recent_user_messages.is_some()),
    ] {
        if set {
            return Some(format!(
                "sets `{key}`, which Claude Code has no equivalent for"
            ));
        }
    }
    if claude_tool(&case.tool).is_none() {
        return Some(format!("`{}` has no Claude Code equivalent", case.tool));
    }
    claude_settings(case).err()
}

/// Claude Code's argument names for the call.
pub fn claude_args(tool: &str, args: &Value) -> Value {
    let Some(map) = args.as_object() else {
        return args.clone();
    };
    let mut out = map.clone();
    if matches!(tool, "read" | "write" | "edit")
        && let Some(path) = out.remove("path")
    {
        out.insert("file_path".into(), path);
    }
    if tool == "notebook_edit"
        && let Some(path) = out.remove("path")
    {
        out.insert("notebook_path".into(), path);
    }
    if tool == "edit"
        && let Some(first) = out.remove("edits").and_then(|e| e.get(0).cloned())
    {
        out.insert("old_string".into(), first["old_text"].clone());
        out.insert("new_string".into(), first["new_text"].clone());
        if let Some(all) = first.get("replace_all") {
            out.insert("replace_all".into(), all.clone());
        }
    }
    Value::Object(out)
}

/// The prompt that makes Claude Code attempt exactly the one call.
pub fn render_prompt(case: &Case) -> Result<String, String> {
    let tool = claude_tool(&case.tool)
        .ok_or_else(|| format!("`{}` has no Claude Code equivalent", case.tool))?;
    Ok(format!(
        "Context: {intent}\n\nCall the {tool} tool exactly once, with exactly these arguments:\n{args}\n\n\
         Do not call any other tool. If the call is refused, do not retry it or look for another \
         way to get the same effect. Reply `done` after the call, or `refused` if it was not allowed.",
        intent = case.task_intent,
        args = claude_args(&case.tool, &case.args),
    ))
}

/// The permission denials in a `claude -p --output-format json` result, which is one result
/// object or an array of events ending in one.
pub fn extract_denials(output: &str) -> Result<Vec<Denial>, String> {
    let events = events(output)?;
    let result = events
        .iter()
        .rev()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("result"))
        .ok_or("no result event in the output")?;
    let denials = result
        .get("permission_denials")
        .ok_or("the result has no `permission_denials`")?;
    serde_json::from_value(denials.clone()).map_err(|e| format!("`permission_denials`: {e}"))
}

impl CcOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CcOutcome::Allowed => "allowed",
            CcOutcome::Denied => "denied",
        }
    }
}

/// The events of a `claude -p` run: one JSON value, an array of events, or stream-json lines.
fn events(output: &str) -> Result<Vec<Value>, String> {
    let text = output.trim();
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(events)) => Ok(events),
        Ok(other) => Ok(vec![other]),
        Err(e) => text
            .lines()
            .map(|line| serde_json::from_str(line).map_err(|_| format!("not JSON: {e}")))
            .collect(),
    }
}

/// Whether the model called `tool` at all; with no denials, a call that was never made would
/// otherwise read as allowed.
pub fn attempted(output: &str, tool: &str) -> Result<bool, String> {
    Ok(events(output)?.iter().any(|event| {
        event["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|block| block["type"] == "tool_use" && block["name"] == tool)
    }))
}

pub fn outcome_of(denials: &[Denial]) -> CcOutcome {
    if denials.is_empty() {
        CcOutcome::Allowed
    } else {
        CcOutcome::Denied
    }
}

fn cc_mode(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "default",
        other => other.as_str(),
    }
}

#[derive(Debug)]
pub struct CaptureReport {
    pub records: Vec<BaselineRecord>,
    pub skipped: Vec<(String, String)>,
    pub failed: Vec<(String, String)>,
}

async fn claude_version(claude: &str) -> Result<String, String> {
    let out = tokio::process::Command::new(claude)
        .arg("--version")
        .output()
        .await
        .map_err(|e| format!("cannot run `{claude} --version`: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn capture_one(claude: &str, case: &Case, version: &str) -> Result<BaselineRecord, String> {
    let mode = case.mode.unwrap_or(PermissionMode::Manual);
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let cwd = match materialize(case.workspace.as_ref(), scratch.path()) {
        Ok(cwd) => cwd,
        Err(MaterializeError::Skip(why)) => return Err(format!("skip: {why}")),
        Err(MaterializeError::Io(e)) => return Err(e),
    };
    if case.permissions.is_some() {
        let dir = cwd.join(".claude");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let settings = claude_settings(case)?;
        std::fs::write(dir.join("settings.json"), settings.to_string())
            .map_err(|e| e.to_string())?;
    }
    let child = tokio::process::Command::new(claude)
        .args(["-p", &render_prompt(case)?])
        .args(["--permission-mode", cc_mode(mode)])
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--setting-sources", "project"])
        .current_dir(&cwd)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(300), child)
        .await
        .map_err(|_| "timed out after 300 s".to_string())?
        .map_err(|e| format!("cannot run `{claude}`: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let denials = extract_denials(&stdout)?;
    let tool = claude_tool(&case.tool).ok_or("no Claude Code equivalent")?;
    if denials.is_empty() && !attempted(&stdout, &tool)? {
        return Err(format!("the model never called {tool}"));
    }
    Ok(BaselineRecord {
        case_id: case.id.clone(),
        mode: mode.as_str().to_string(),
        cc_outcome: outcome_of(&denials),
        claude: version.to_string(),
    })
}

/// Poses each case to `claude -p` in a fresh scratch workspace, in the case's own mode.
pub async fn capture(claude: &str, cases: &[&Case]) -> Result<CaptureReport, String> {
    let version = claude_version(claude).await?;
    let mut report = CaptureReport {
        records: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };
    for case in cases {
        if let Some(why) = capture_skip_reason(case) {
            report.skipped.push((case.id.clone(), why));
            continue;
        }
        match capture_one(claude, case, &version).await {
            Ok(record) => report.records.push(record),
            Err(e) => match e.strip_prefix("skip: ") {
                Some(why) => report.skipped.push((case.id.clone(), why.to_string())),
                None => report.failed.push((case.id.clone(), e)),
            },
        }
    }
    Ok(report)
}

pub fn write_records(path: &Path, records: &[BaselineRecord]) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = String::new();
    for r in records {
        text.push_str(&serde_json::to_string(r).expect("record serializes"));
        text.push('\n');
    }
    std::fs::write(path, text)
}

pub fn read_records(path: &Path) -> Result<Vec<BaselineRecord>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))
        })
        .collect()
}

#[derive(Deserialize)]
struct RunFile {
    cases: Vec<RunCase>,
}

#[derive(Deserialize)]
struct RunCase {
    id: String,
    actual: String,
    mode: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Divergence {
    pub case_id: String,
    pub mode: String,
    pub fontaine: String,
    pub claude: CcOutcome,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiffReport {
    pub compared: usize,
    pub divergences: Vec<Divergence>,
    /// Baseline records with no case in the Fontaine run (or run in another mode).
    pub unmatched: Vec<String>,
}

/// Fontaine `allow` is Claude Code allowed; `deny` and `prompt` are denied in headless.
pub fn fontaine_outcome(actual: &str) -> CcOutcome {
    if actual == "allow" {
        CcOutcome::Allowed
    } else {
        CcOutcome::Denied
    }
}

pub fn diff(baseline: &[BaselineRecord], run_json: &str) -> Result<DiffReport, String> {
    let run: RunFile = serde_json::from_str(run_json).map_err(|e| format!("run file: {e}"))?;
    let by_id: BTreeMap<&str, &RunCase> = run.cases.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut report = DiffReport::default();
    for record in baseline {
        let found = by_id
            .get(record.case_id.as_str())
            .filter(|c| c.mode.as_deref().is_none_or(|m| m == record.mode));
        let Some(case) = found else {
            report.unmatched.push(record.case_id.clone());
            continue;
        };
        report.compared += 1;
        if fontaine_outcome(&case.actual) != record.cc_outcome {
            report.divergences.push(Divergence {
                case_id: record.case_id.clone(),
                mode: record.mode.clone(),
                fontaine: case.actual.clone(),
                claude: record.cc_outcome,
            });
        }
    }
    Ok(report)
}
