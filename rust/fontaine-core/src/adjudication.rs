//! Builds the guardian's request for one tool call. Everything in it comes from the session
//! ledger and the call itself, never from the model's context: a compaction summary, a tool
//! result or anything else a model wrote cannot reach the guardian as the user's intent.

use fontaine_agents::registry::AgentTypes;
use fontaine_guardian::{
    ACTIONS_TAKEN, ActionTaken, AdjudicationRequest, DenialRecord, PreExistence,
    RECENT_USER_MESSAGES, SessionFacts, TASK_INTENT_CHARS, ToolCallSummary, TouchedPath,
    task_intent,
};
use fontaine_tools::descriptor::{self, Destructive};
use fontaine_tools::gate::ROOT_AGENT;
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
use crate::trust;

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

/// The toolset of the agent an `agent` call starts, computed by the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnFacts {
    pub agent_type: String,
    pub tools: Vec<String>,
    pub changes_state: bool,
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

/// Nothing is at `path` as the request is built. A dangling symlink is something. A path that
/// went through a lossy conversion (U+FFFD) names a different file than the one on disk.
fn is_absent(path: &str) -> bool {
    !path.contains('\u{FFFD}')
        && matches!(
            std::fs::symlink_metadata(path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound
        )
}

/// Programs that neither change the working directory nor take an option that names another one.
/// An allowlist: anything that can run or wrap another command, or move the shell, is absent.
const CWD_SAFE_PROGRAMS: [&str; 19] = [
    "mv", "cp", "rm", "rmdir", "mkdir", "touch", "ln", "cat", "echo", "printf", "ls", "head",
    "tail", "wc", "rg", "grep", "true", "test", "git",
];

/// Whether the paths `touched_paths` joined to the call's working directory are the ones the
/// command will hit. A shell command that moves the shell, runs another command that might,
/// assigns `GIT_WORK_TREE` and the like, or names another tree with `-C`/`--git-dir`/
/// `--work-tree`, resolves its relative paths elsewhere, and a mark computed for the wrong file
/// would tell the guardian an overwrite is safe. So only a short list of programs is trusted.
fn cwd_is_certain(tool: &str, args: &Value) -> bool {
    if !fontaine_tools::descriptor::is_shell_class(tool) {
        return true;
    }
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return true;
    };
    let analysis = fontaine_tools::preclassify::analyze(command);
    if !analysis.parsed || analysis.unanalyzed || analysis.nested_chain || analysis.assignments {
        return false;
    }
    const DIR_FLAGS: [&str; 5] = ["-C", "--directory", "--chdir", "--git-dir", "--work-tree"];
    const DIR_PREFIXES: [&str; 4] = ["--directory=", "--chdir=", "--git-dir=", "--work-tree="];
    analysis.commands.iter().all(|simple| {
        let Some(first) = simple.words.first() else {
            return true;
        };
        let Some(program) = first.literal.as_deref() else {
            return false;
        };
        let program = fontaine_tools::preclassify::program_name(program);
        if !CWD_SAFE_PROGRAMS.contains(&program.as_ref()) {
            return false;
        }
        // Git options before the subcommand (`-C`, `-c`, `--namespace`, ...) can all redirect it.
        if program == "git"
            && !simple
                .words
                .get(1)
                .is_none_or(|w| w.literal.as_deref().is_some_and(|l| !l.starts_with('-')))
        {
            return false;
        }
        simple.words.iter().all(|word| {
            word.literal.as_deref().is_none_or(|literal| {
                !DIR_FLAGS.contains(&literal)
                    && !literal.starts_with("-C")
                    && !DIR_PREFIXES.iter().any(|p| literal.starts_with(p))
            })
        })
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
    let paths = touched_paths(tool, args, scope);
    let resolved: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
    let certain = cwd_is_certain(tool, args);
    // `absent` describes the path before the call runs; a path named twice may be created by one
    // command and destroyed by the next.
    let repeated = crate::destructive::repeated_paths(tool, args, scope);
    let tracked = if certain {
        trust::tracked_files(scope.primary(), &resolved)
    } else {
        vec![false; resolved.len()]
    };
    let touched_paths = paths
        .into_iter()
        .zip(tracked)
        .map(|(path, git_tracked)| TouchedPath {
            status: status(view.existence(&path)),
            in_scratch: in_scratch(&path, scratch.as_deref()),
            absent: certain && !repeated.contains(&path) && is_absent(&path),
            git_tracked,
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
        project_environment: view
            .project_environment
            .as_ref()
            .map(crate::project_env::to_facts),
    }
}

/// `caller` and every agent above it below the root: the agents whose own view's messages are
/// intent for `caller`. Empty for the root.
fn ancestry(state: &SessionState, caller: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut at = caller.to_string();
    // A tree is a few layers deep; the bound keeps a corrupt parent link finite.
    for _ in 0..64 {
        if at == ROOT_AGENT {
            break;
        }
        chain.push(at.clone());
        match state.agents().parent_of(&at) {
            Some(parent) => at = parent,
            None => break,
        }
    }
    chain
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
    let intent = view.intent_for(&ancestry(state, caller));
    let from = intent.len().saturating_sub(RECENT_USER_MESSAGES);
    let recent = intent[from..].to_vec();
    let destructive = destructive::derive(
        tool,
        args,
        &Facts {
            scope: &scope,
            view: &view,
        },
    );
    let session_facts = facts(
        &view,
        &scope,
        state.memory_root.as_deref(),
        state.scratch.as_deref(),
        tool,
        args,
    );
    let denials_from = view.denials.len().saturating_sub(PRIOR_DENIALS);
    AdjudicationRequest {
        task_intent: task_intent(&intent, TASK_INTENT_CHARS),
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

    #[test]
    fn absent_and_git_tracked_are_read_from_the_disk_and_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let work = std::fs::canonicalize(dir.path()).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(work.join("kept.txt"), "a").unwrap();
        std::fs::write(work.join("loose.txt"), "a").unwrap();
        git(&["add", "kept.txt"]);
        #[cfg(unix)]
        std::os::unix::fs::symlink(work.join("nowhere"), work.join("dangling")).unwrap();
        let scope = Scope::new(work.clone(), []);
        let view = LedgerView::default();
        let command = "echo x > kept.txt > loose.txt > missing.txt > dangling";
        let got = facts(
            &view,
            &scope,
            None,
            None,
            "shell",
            &serde_json::json!({ "command": command }),
        );
        let by_name = |name: &str| {
            got.touched_paths
                .iter()
                .find(|t| t.path.ends_with(name))
                .unwrap_or_else(|| panic!("{name} in {:?}", got.touched_paths))
        };
        assert!(by_name("kept.txt").git_tracked && !by_name("kept.txt").absent);
        assert!(!by_name("loose.txt").git_tracked && !by_name("loose.txt").absent);
        assert!(by_name("missing.txt").absent && !by_name("missing.txt").git_tracked);
        #[cfg(unix)]
        assert!(!by_name("dangling").absent);
    }

    #[test]
    fn marks_need_a_certain_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let work = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(work.join("README.md"), "a").unwrap();
        let scope = Scope::new(work.clone(), []);
        let view = LedgerView::default();
        let marks = |command: &str| {
            let got = facts(
                &view,
                &scope,
                None,
                None,
                "shell",
                &serde_json::json!({ "command": command }),
            );
            got.touched_paths.iter().any(|t| t.absent || t.git_tracked)
        };
        assert!(marks("mv a.txt ./b.txt"));
        assert!(!marks("cd /x && mv a.txt ./b.txt"));
        assert!(!marks("pushd /x && cp a README.md"));
        assert!(!marks("git -C /x mv a README.md"));
        assert!(!marks("git --work-tree=/x mv a ./b"));
        for probe in [
            "builtin cd /x && mv a ./b",
            "command cd /x && mv a ./b",
            "eval cd /x; mv a ./b",
            "GIT_WORK_TREE=/x git mv a ./b",
            "export GIT_WORK_TREE=/x; git mv a ./b",
            "env -C/x mv a ./b",
            "sudo -D /x mv a ./b",
            "find . -execdir mv {} ./b \\;",
            "source ./x.sh; mv a ./b",
        ] {
            assert!(!marks(probe), "{probe}");
        }
        std::fs::create_dir(work.join("tasks")).unwrap();
        std::fs::create_dir(work.join("tasks/completed")).unwrap();
        assert!(marks(
            "git mv tasks/x_plan.md tasks/completed/x_plan.md && git add -u"
        ));
        let absent = |command: &str| {
            facts(
                &view,
                &scope,
                None,
                None,
                "shell",
                &serde_json::json!({ "command": command }),
            )
            .touched_paths
            .iter()
            .any(|t| t.path.ends_with("/t") && t.absent)
        };
        assert!(absent("mv notes.txt ./t"));
        assert!(!absent("mv notes.txt ./t && rm ./t"));
        assert!(!absent("mv a ./t && cp junk ./t"));
        assert!(
            facts(
                &view,
                &scope,
                None,
                None,
                "shell",
                &serde_json::json!({ "command": "git mv tasks/x_plan.md tasks/completed/x_plan.md && git add -u" }),
            )
            .touched_paths
            .iter()
            .any(|t| t.path.ends_with("completed/x_plan.md") && t.absent)
        );
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
            names(&["glob", "grep", "ls", "lsp", "read", "send_message"])
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
