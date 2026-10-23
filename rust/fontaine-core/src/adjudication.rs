//! Builds the guardian's request for one tool call. Everything in it comes from the session
//! ledger and the call itself, never from the model's context: a compaction summary, a tool
//! result or anything else a model wrote cannot reach the guardian as the user's intent.

use fontaine_guardian::{
    ACTIONS_TAKEN, ActionTaken, AdjudicationRequest, DenialRecord, PreExistence,
    RECENT_USER_MESSAGES, SessionFacts, TASK_INTENT_CHARS, ToolCallSummary, TouchedPath,
    task_intent,
};
use fontaine_tools::scope::Scope;
use rig_core::redact;
use serde_json::Value;

use crate::audit::args_digest;
use crate::destructive::{self, Facts, touched_paths};
use crate::gate::SessionState;
use crate::ledger::{Existence, LedgerView};
use crate::permissions::summarize;
use crate::prompt::InstructionFile;

/// Longest compact summary of a call's arguments kept in the ledger and shown to the guardian.
pub const SUMMARY_CHARS: usize = 200;
/// Denials the request lists, most recent last.
const PRIOR_DENIALS: usize = 20;
/// Instruction text the guardian reads, in characters.
const INSTRUCTION_CHARS: usize = 16_000;

/// A compact, single-line summary of a call's arguments with credential-shaped text redacted.
pub fn action_summary(tool: &str, args: &Value) -> String {
    let full = summarize(tool, args);
    let line: String = redact::text(&full)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    if line.chars().count() <= SUMMARY_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(SUMMARY_CHARS - 1).collect();
    format!("{cut}…")
}

/// The instruction files as the guardian reads them: the same text the agent sees.
pub fn project_instructions(files: &[InstructionFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let text = files
        .iter()
        .map(|file| format!("From {}:\n{}", file.path.display(), file.text.trim_end()))
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(text.chars().take(INSTRUCTION_CHARS).collect())
}

fn status(existence: Existence) -> PreExistence {
    match existence {
        Existence::PreExisting => PreExistence::PreExisting,
        Existence::CreatedThisSession => PreExistence::CreatedThisSession,
        Existence::Unindexed => PreExistence::Unindexed,
    }
}

fn facts(view: &LedgerView, scope: &Scope, tool: &str, args: &Value) -> SessionFacts {
    let roots = if view.workspace_roots.is_empty() {
        scope
            .roots()
            .iter()
            .map(|root| root.display().to_string())
            .collect()
    } else {
        view.workspace_roots.clone()
    };
    let touched_paths = touched_paths(tool, args, scope)
        .into_iter()
        .map(|path| TouchedPath {
            status: status(view.existence(&path)),
            path,
        })
        .collect();
    let from = view.actions.len().saturating_sub(ACTIONS_TAKEN);
    SessionFacts {
        workspace_roots: roots,
        workspace_trust: view.workspace_trust.clone(),
        git_remotes: view.git_remotes.clone(),
        touched_paths,
        actions_taken: view.actions[from..]
            .iter()
            .map(|action| ActionTaken {
                tool: action.tool.clone(),
                summary: action.summary.clone(),
            })
            .collect(),
    }
}

/// The request for `tool` called with `args`, read from the ledger as it stands now.
pub fn build(
    state: &SessionState,
    tool: &str,
    args: &Value,
    hook_context: Vec<String>,
    annotations: Option<&str>,
) -> AdjudicationRequest {
    let scope = state.scope();
    let view = state.ledger.view();
    let from = view
        .user_messages
        .len()
        .saturating_sub(RECENT_USER_MESSAGES);
    let recent = view.user_messages[from..].to_vec();
    let destructive = destructive::derive(
        tool,
        args,
        &Facts {
            scope: &scope,
            view: &view,
        },
    );
    let denials_from = view.denials.len().saturating_sub(PRIOR_DENIALS);
    AdjudicationRequest {
        task_intent: task_intent(&view.user_messages, TASK_INTENT_CHARS),
        recent_user_messages: recent,
        project_instructions: state.auto.project_instructions.clone(),
        environment: state.auto.environment.clone(),
        rules: state.auto.rules.clone(),
        tool_call: ToolCallSummary {
            tool: tool.to_string(),
            args: args.clone(),
            args_digest: args_digest(args),
            destructive,
            annotations: annotations.map(str::to_string),
        },
        session_facts: facts(&view, &scope, tool, args),
        hook_context,
        prior_denials: view.denials[denials_from..]
            .iter()
            .map(|denial| DenialRecord {
                tool: denial.tool.clone(),
                rule: denial.rule.clone(),
                summary: denial.summary.clone(),
            })
            .collect(),
    }
}
