//! Pipeline cases through the real `PermissionPipeline`, with nobody to ask: with no guardian
//! (`--guardian none`, where `auto` has nothing to run) or with a live one.

use std::sync::Arc;
use std::time::Instant;

use fontaine_core::audit::AuditLog;
use fontaine_core::config::{PermissionMode, Slot};
use fontaine_core::gate::{AutoContext, SessionState, StateParams, no_guardian};
use fontaine_core::ledger::{Ledger, Record};
use fontaine_core::permission_rules::{RuleSource, SourcedRule, parse_rule};
use fontaine_core::permissions::{DenyAll, PermissionPipeline, PipelineParams};
use fontaine_guardian::{EnvironmentConfig, Guardian, RuleConfig};
use fontaine_tools::gate::{CallView, ROOT_AGENT, ToolGate};
use fontaine_tools::risk::{RiskClass, registered_risk};

use crate::case::{Case, MaterializeError, materialize};

/// What the pipeline decided first for one case.
pub enum Outcome {
    Decided {
        decision: String,
        tier: String,
        latency_ms: u64,
    },
    Skipped(String),
}

fn sourced(rules: &[String]) -> Result<Vec<SourcedRule>, String> {
    rules
        .iter()
        .map(|r| {
            parse_rule(r)
                .map(|rule| SourcedRule::new(rule, RuleSource::Config(Slot::User)))
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// The prose lists a case overrides, expanded as a session expands them.
pub fn auto_context(case: &Case) -> AutoContext {
    let overrides = case.auto_mode.clone().unwrap_or_default();
    let environment = case
        .environment
        .clone()
        .or(overrides.environment)
        .unwrap_or_default();
    AutoContext {
        environment: EnvironmentConfig::expand(&environment),
        rules: RuleConfig::expand(
            &overrides.hard_deny.unwrap_or_default(),
            &overrides.soft_deny.unwrap_or_default(),
            &overrides.allow.unwrap_or_default(),
        ),
        project_instructions: None,
    }
}

/// The first decision record is the one scored: a prompt scores `prompt` even though the
/// approver then answers deny. `guardian` is the live guardian of a `live:` run, if any.
pub async fn decide(case: &Case, guardian: Option<Arc<Guardian>>) -> Result<Outcome, String> {
    // A POSIX-rooted path is not absolute on Windows, so the case would score a different
    // question than the one it asks.
    if cfg!(windows)
        && case
            .args
            .get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.starts_with('/'))
    {
        return Ok(Outcome::Skipped(
            "the case uses a POSIX absolute path, which does not exist on Windows".into(),
        ));
    }
    let scratch = tempfile::tempdir().map_err(|e| format!("scratch directory: {e}"))?;
    let base = scratch
        .path()
        .canonicalize()
        .map_err(|e| format!("scratch directory: {e}"))?;
    let root = base.join("workspace");
    let home = base.join("home");
    let data = base.join("data");
    for dir in [&root, &home, &data] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let cwd = match materialize(case.workspace.as_ref(), &root) {
        Ok(cwd) => cwd,
        Err(MaterializeError::Skip(why)) => return Ok(Outcome::Skipped(why)),
        Err(MaterializeError::Io(e)) => return Err(e),
    };
    let cwd = cwd.canonicalize().map_err(|e| e.to_string())?;

    let permissions = case.permissions.clone().unwrap_or_default();
    let session_id = format!("eval-{}", case.id);
    let ledger = Ledger::create(&data).map_err(|e| format!("ledger: {e}"))?;
    // What the guardian reads of the user and the session comes from the ledger.
    let append = |record| ledger.append(record).map_err(|e| format!("ledger: {e}"));
    append(Record::SessionStart {
        session_id: session_id.clone(),
        permission_mode: case
            .mode
            .unwrap_or(PermissionMode::Manual)
            .as_str()
            .to_string(),
        workspace_roots: vec![cwd.display().to_string()],
        workspace_trust: "trusted".to_string(),
        git_remotes: Vec::new(),
        env_keys: Vec::new(),
    })?;
    let mut said = case.recent_user_messages.clone().unwrap_or_default();
    if said.last() != Some(&case.task_intent) {
        said.push(case.task_intent.clone());
    }
    for message in said {
        append(Record::UserMessage { text: message })?;
    }
    let audit = AuditLog::create(&data, &session_id).map_err(|e| format!("audit log: {e}"))?;
    let approver = Arc::new(DenyAll);
    let state = Arc::new(SessionState::new(StateParams {
        session_id,
        mode: case.mode.unwrap_or(PermissionMode::Manual),
        scope: Arc::new(fontaine_tools::scope::Scope::new(cwd, [])),
        deny: sourced(&permissions.deny)?,
        ask: sourced(&permissions.ask)?,
        allow: sourced(&permissions.allow)?,
        ledger: Arc::new(ledger),
        audit: Arc::new(audit),
        approver: approver.clone(),
        home: Some(home),
        protected_dirs: Vec::new(),
        scratch: None,
        memory_root: None,
        guardian: guardian.unwrap_or_else(no_guardian),
        auto: auto_context(case),
        agents: Arc::new(fontaine_agents::tree::AgentTree::new(
            fontaine_agents::tree::Limits::default(),
        )),
        reveal_mcp_names: false,
        auto_edits: fontaine_core::gate::AutoEdits::default(),
    }));
    let pipeline = PermissionPipeline::new(PipelineParams {
        state: state.clone(),
        approver,
        headless: false,
        hooks: Vec::new(),
        denied_hooks: Vec::new(),
        allow_extra: permissions.allow_extra,
        user_config: None,
    });

    let risk = registered_risk(&case.tool).unwrap_or(RiskClass::Arbitrary);
    let started = Instant::now();
    pipeline
        .check(CallView {
            tool: &case.tool,
            risk,
            args: &case.args,
            agent: ROOT_AGENT,
            annotations: None,
            requires_user_interaction: false,
            synthetic: false,
        })
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let first = state
        .feed()
        .await
        .try_next()
        .ok_or("the pipeline recorded no decision")?;
    Ok(Outcome::Decided {
        decision: first.decision.as_str().to_string(),
        tier: first.tier.as_str().to_string(),
        latency_ms,
    })
}
