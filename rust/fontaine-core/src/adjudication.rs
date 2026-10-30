//! Builds the guardian's request for one tool call. Everything in it comes from the session
//! ledger and the call itself, never from the model's context: a compaction summary, a tool
//! result or anything else a model wrote cannot reach the guardian as the user's intent.

use fontaine_agents::registry::AgentTypes;
use fontaine_guardian::{
    ACTIONS_TAKEN, ActionTaken, AdjudicationRequest, DenialRecord, PreExistence,
    RECENT_USER_MESSAGES, SessionFacts, SpawnFacts, TASK_INTENT_CHARS, ToolCallSummary,
    TouchedPath, task_intent,
};
use fontaine_tools::descriptor::{self, Destructive};
use fontaine_tools::risk::RiskClass;
use fontaine_tools::scope::Scope;
use rig_core::redact;
use serde_json::Value;

use crate::audit::args_digest;
use crate::destructive::{self, Facts, touched_paths};
use crate::gate::SessionState;
use crate::instructions::InstructionFile;
use crate::ledger::{Existence, LedgerView};
use crate::permissions::summarize;

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

/// What the child of an `agent` call can do, from the same toolset `AgentTool` gives it. `None`
/// when the type is unknown: the call then fails on its own.
///
/// `changes_state` is false only when every tool is read-only and non-destructive, or is
/// `send_message`, which reaches only agents of the same tree whose calls are all gated. A tool
/// missing from the descriptor table (every MCP tool) counts as state-changing, and so does an
/// unknown parent toolset (`parent_tools` is `None`: never set, which a type that inherits it
/// would otherwise resolve to nothing). A toolset that is known and empty can change nothing.
/// The depth trim that drops
/// `agent` at the nesting limit is left out, which only makes the answer more conservative.
pub fn spawn_facts(
    types: &AgentTypes,
    parent_tools: Option<&[String]>,
    args: &Value,
) -> Option<SpawnFacts> {
    let name = args.get("agent_type")?.as_str()?;
    let tools = types.get(name)?.toolset(parent_tools.unwrap_or_default());
    let read_only = |tool: &String| {
        tool == "send_message"
            || descriptor::find(tool).is_some_and(|d| {
                d.risk == RiskClass::ReadOnly && d.destructive == Destructive::Never
            })
    };
    let changes_state = parent_tools.is_none() || !tools.iter().all(read_only);
    Some(SpawnFacts {
        agent_type: name.to_string(),
        tools,
        changes_state,
    })
}

/// Whether `path`, already resolved, lies inside the scratch root. Component-wise, so a sibling
/// that merely shares the prefix does not count, and a `..` that survived resolution never does.
fn in_scratch(path: &str, scratch: Option<&std::path::Path>) -> bool {
    let path = std::path::Path::new(path);
    scratch.is_some_and(|root| {
        path.starts_with(root)
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    })
}

fn facts(
    view: &LedgerView,
    scope: &Scope,
    memory_dir: Option<&std::path::Path>,
    scratch: Option<&std::path::Path>,
    tool: &str,
    args: &Value,
) -> SessionFacts {
    let memory = memory_dir.map(|dir| dir.display().to_string());
    // The store is told to the guardian as its own fact; listing it as a workspace root would
    // read as a trusted location.
    let roots: Vec<String> = if view.workspace_roots.is_empty() {
        scope
            .roots()
            .iter()
            .map(|root| root.display().to_string())
            .collect()
    } else {
        view.workspace_roots.clone()
    }
    .into_iter()
    .filter(|root| Some(root) != memory.as_ref())
    .collect();
    let scratch =
        scratch.map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
    let touched_paths = touched_paths(tool, args, scope)
        .into_iter()
        .map(|path| TouchedPath {
            status: status(view.existence(&path)),
            in_scratch: in_scratch(&path, scratch.as_deref()),
            path,
        })
        .collect();
    let from = view.actions.len().saturating_sub(ACTIONS_TAKEN);
    SessionFacts {
        workspace_roots: roots,
        workspace_trust: view.workspace_trust.clone(),
        git_remotes: view.git_remotes.clone(),
        env_keys: view.env_keys.clone(),
        touched_paths,
        actions_taken: view.actions[from..]
            .iter()
            .map(|action| ActionTaken {
                tool: action.tool.clone(),
                summary: action.summary.clone(),
            })
            .collect(),
        memory_dir: memory_dir.map(|dir| dir.display().to_string()),
        scratch_dir: scratch.map(|dir| dir.display().to_string()),
        spawn: None,
    }
}

/// The request for `tool` called with `args`, read from the ledger as it stands now.
pub fn build(
    state: &SessionState,
    caller: &str,
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
    let mut session_facts = facts(
        &view,
        &scope,
        state.memory_root.as_deref(),
        state.scratch.as_deref(),
        tool,
        args,
    );
    if tool == "agent" {
        session_facts.spawn = spawn_facts(
            state.agent_types(),
            state.agents().known_tools_of(caller).as_deref(),
            args,
        );
    }
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
        session_facts,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_memory_store_is_not_a_workspace_root_for_the_guardian() {
        let work = PathBuf::from("/work/proj");
        let memory = PathBuf::from("/data/memory/proj-1");
        let scope = Scope::new(work.clone(), [memory.clone()]);
        let view = LedgerView::default();
        let got = facts(&view, &scope, Some(&memory), None, "Read", &Value::Null);
        assert_eq!(got.workspace_roots, vec![work.display().to_string()]);
        assert_eq!(got.memory_dir, Some(memory.display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn scratch_membership_is_component_wise_and_follows_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let work = base.join("work");
        let scratch = base.join("scratch");
        let outside = base.join("scratch-x");
        for d in [&work, &scratch, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::os::unix::fs::symlink(&outside, scratch.join("link")).unwrap();
        let scope = Scope::new(work.clone(), [scratch.clone()]);
        let old = scratch.join("old.txt").display().to_string();
        let ledger = crate::ledger::Ledger::create(dir.path()).unwrap();
        ledger
            .append(crate::ledger::Record::Action {
                tool: "write".into(),
                agent: "main".into(),
                paths: vec![old.clone()],
                created: vec![old.clone()],
                summary: "s".into(),
            })
            .unwrap();
        let view = ledger.view();
        let command = |c: String| serde_json::json!({ "command": c });
        let marks = |c: String| -> Vec<(String, bool, PreExistence)> {
            facts(&view, &scope, None, Some(&scratch), "shell", &command(c))
                .touched_paths
                .into_iter()
                .map(|t| (t.path, t.in_scratch, t.status))
                .collect()
        };
        let inside = scratch.join("new.txt").display().to_string();
        let got = marks(format!("rm {inside}"));
        assert_eq!(got, vec![(inside, true, PreExistence::Unindexed)]);
        assert_eq!(
            marks(format!("rm {old}")),
            vec![(old, true, PreExistence::CreatedThisSession)]
        );
        let sibling = outside.join("a").display().to_string();
        assert!(marks(format!("rm {sibling}")).iter().all(|m| !m.1));
        let escaped = format!("{}/../work/f", scratch.display());
        assert!(marks(format!("rm {escaped}")).iter().all(|m| !m.1));
        let linked = scratch.join("link").join("f").display().to_string();
        assert!(marks(format!("rm {linked}")).iter().all(|m| !m.1));
    }

    #[test]
    fn the_scratch_fact_is_set_only_with_a_scratch_root() {
        let scope = Scope::new(PathBuf::from("/work/proj"), []);
        let view = LedgerView::default();
        let none = facts(&view, &scope, None, None, "read", &Value::Null);
        assert_eq!(none.scratch_dir, None);
        let dir = tempfile::tempdir().unwrap();
        let some = facts(&view, &scope, None, Some(dir.path()), "read", &Value::Null);
        assert_eq!(
            some.scratch_dir,
            Some(
                std::fs::canonicalize(dir.path())
                    .unwrap()
                    .display()
                    .to_string()
            )
        );
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn spawn(types: &AgentTypes, parent: &[&str], agent_type: &str) -> Option<SpawnFacts> {
        spawn_facts(
            types,
            Some(&names(parent)),
            &serde_json::json!({ "agent_type": agent_type, "prompt": "p" }),
        )
    }

    #[test]
    fn explore_cannot_change_state_and_worker_can() {
        let types = AgentTypes::builtin();
        let all: Vec<&str> = fontaine_tools::descriptor::names().collect();
        let explore = spawn(&types, &all, "explore").unwrap();
        let mut tools = explore.tools.clone();
        tools.sort();
        assert_eq!(
            tools,
            names(&["glob", "grep", "ls", "read", "send_message"])
        );
        assert!(!explore.changes_state);
        assert!(spawn(&types, &all, "worker").unwrap().changes_state);
        assert!(spawn(&types, &all, "fork").unwrap().changes_state);
    }

    #[test]
    fn the_toolset_is_cut_to_the_parents_and_an_unknown_one_fails_closed() {
        let types = AgentTypes::builtin();
        let cut = spawn(&types, &["read", "shell"], "explore").unwrap();
        assert_eq!(cut.tools, names(&["read"]));
        assert!(!cut.changes_state);
        let empty = spawn(&types, &[], "explore").unwrap();
        assert!(empty.tools.is_empty() && !empty.changes_state);
        let unknown = spawn_facts(
            &types,
            None,
            &serde_json::json!({ "agent_type": "explore", "prompt": "p" }),
        )
        .unwrap();
        assert!(unknown.changes_state);
        assert!(spawn(&types, &["read"], "nope").is_none());
        assert!(spawn_facts(&types, Some(&names(&["read"])), &serde_json::json!({})).is_none());
    }

    #[test]
    fn agent_and_mcp_tools_make_a_spawn_state_changing() {
        use fontaine_agents::mode::ModeSetting;
        use fontaine_agents::registry::{AgentType, ModelSpec, TypeOrigin};
        let mut types = AgentTypes::builtin();
        for (name, tools) in [
            ("nests", vec!["read", "agent"]),
            ("mcp", vec!["read", "mcp__srv__lookup"]),
            ("fetches", vec!["read", "webfetch"]),
        ] {
            types.insert(AgentType {
                name: name.into(),
                description: String::new(),
                prompt: None,
                tools: Some(tools.into_iter().map(String::from).collect()),
                disallowed: Vec::new(),
                model: Some(ModelSpec::Main),
                mode: ModeSetting::Inherit,
                origin: TypeOrigin::Config,
                inherits_context: false,
            });
        }
        let parent = ["read", "agent", "mcp__srv__lookup", "webfetch"];
        for name in ["nests", "mcp", "fetches"] {
            assert!(
                spawn(&types, &parent, name).unwrap().changes_state,
                "{name}"
            );
        }
    }
}
