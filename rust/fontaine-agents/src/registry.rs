//! The agent types a session can spawn: the built-in four, `[agents.types]` entries and agent
//! files in one registry. A type names a model, a toolset and a mode; the toolset a child
//! really gets is that set cut down to what its parent has.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fontaine_tools::mcp::TOOL_SEARCH;

use crate::effort::Effort;
use crate::mode::ModeSetting;

/// The model an agent runs on. `fontaine-agents` knows no roles, so a role name or a list stays
/// text here and the session resolves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSpec {
    /// The session's model.
    Main,
    /// The top-ranked validated guardian model the accounts offer, or the session's model when
    /// none is.
    GuardianDefault,
    /// A lone `<account>/<model>`.
    Model(String),
    /// A role name from `[models.roles]`.
    Named(String),
    /// Several references, each `<account>/<model>`, a role name or `$main`, in preference order.
    List(Vec<String>),
}

/// Whether `name` can be a role: letters, digits, `_` and `-`, and not the agent-file sentinel
/// `inherit`.
pub fn valid_role_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "`{name}` is not a role name: use letters, digits, `_` and `-`"
        ));
    }
    if name == "inherit" {
        return Err("`inherit` is reserved and cannot be a role name".to_string());
    }
    Ok(())
}

/// Whether `entry` is something a model list may hold: `<account>/<model>`, a role name or
/// `$main`.
fn list_entry_ok(entry: &str) -> bool {
    entry == "$main"
        || match entry.split_once('/') {
            Some((account, model)) => !account.is_empty() && !model.is_empty(),
            None => valid_role_name(entry).is_ok(),
        }
}

impl ModelSpec {
    /// A spec from one text: `$main`, `$guardian_default`, `<account>/<model>`, a role name, or
    /// several of the last three separated by commas.
    pub fn parse(text: &str) -> Result<ModelSpec, String> {
        if text.contains(',') {
            let entries: Vec<String> = text.split(',').map(|e| e.trim().to_string()).collect();
            return ModelSpec::from_entries(&entries);
        }
        match text.trim() {
            "$main" => Ok(ModelSpec::Main),
            "$guardian_default" => Ok(ModelSpec::GuardianDefault),
            other => match other.split_once('/') {
                Some((account, model)) if !account.is_empty() && !model.is_empty() => {
                    Ok(ModelSpec::Model(other.to_string()))
                }
                Some(_) => Err(not_a_model(other)),
                None if valid_role_name(other).is_ok() => Ok(ModelSpec::Named(other.to_string())),
                None => Err(not_a_model(other)),
            },
        }
    }

    /// A spec from the entries of a configured `model`: one entry is [`ModelSpec::parse`]d, a
    /// list keeps its order. `$guardian_default` is only a spec of its own.
    pub fn from_entries(entries: &[String]) -> Result<ModelSpec, String> {
        match entries {
            [] => Err("a model list names at least one model".to_string()),
            [one] => ModelSpec::parse(one),
            many => {
                if let Some(bad) = many.iter().find(|e| !list_entry_ok(e)) {
                    return Err(not_a_model(bad));
                }
                Ok(ModelSpec::List(many.to_vec()))
            }
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            ModelSpec::Main => "$main".to_string(),
            ModelSpec::GuardianDefault => "$guardian_default".to_string(),
            ModelSpec::Model(text) | ModelSpec::Named(text) => text.clone(),
            ModelSpec::List(entries) => entries.join(","),
        }
    }
}

fn not_a_model(text: &str) -> String {
    format!(
        "`{text}` is not a model: use a role name, `$main`, `$guardian_default` or `<account>/<model>`"
    )
}

/// Where a type was defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeOrigin {
    Builtin,
    /// An `[agents.types]` entry.
    Config,
    UserFile(PathBuf),
    /// A file in the repository: it can only narrow, so it never picks a model.
    ProjectFile(PathBuf),
    /// A file in an installed plugin: third-party, so it follows the project rules.
    PluginFile(PathBuf),
    /// Defined by a host program for this session only: it can only narrow, like a repository's.
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentType {
    pub name: String,
    pub description: String,
    /// The instructions a file-defined type runs under, added to the base prompt.
    pub prompt: Option<String>,
    /// The tools the type may use; `None` means every tool its parent has. Entries may carry a
    /// `(specifier)`, which limits the calls of that tool to the ones it matches.
    pub tools: Option<Vec<String>>,
    /// Tools taken away from the set above; an entry with a specifier forbids only the calls
    /// it matches.
    pub disallowed: Vec<String>,
    /// `None` runs on the session model.
    pub model: Option<ModelSpec>,
    pub mode: ModeSetting,
    /// `None` inherits the level the session runs at.
    pub effort: Option<Effort>,
    pub origin: TypeOrigin,
    /// Whether the child starts from a copy of its parent's conversation. Only the built-in
    /// `fork` has it: no agent file or config entry can set it.
    pub inherits_context: bool,
}

/// The built-in type whose child starts from a copy of the main conversation.
pub const FORK_TYPE: &str = "fork";

/// The tool a toolset entry names: `shell(git *)` is `shell`.
pub fn entry_tool(entry: &str) -> &str {
    entry.split_once('(').map_or(entry, |(tool, _)| tool)
}

/// Whether an entry limits calls of its tool instead of naming the whole tool.
pub fn entry_has_specifier(entry: &str) -> bool {
    entry.contains('(')
}

/// Whether a toolset entry's tool covers `tool`: the same name, or for an MCP entry naming a
/// whole server (`mcp__srv` or `mcp__srv__*`) any tool of that server.
fn entry_covers(entry_tool: &str, tool: &str) -> bool {
    if entry_tool == tool {
        return true;
    }
    let Some(rest) = entry_tool.strip_prefix(fontaine_tools::names::MCP_TOOL_PREFIX) else {
        return false;
    };
    let server = rest.strip_suffix("__*").unwrap_or(rest);
    if server.is_empty() || server.contains("__") {
        return false;
    }
    tool.strip_prefix(fontaine_tools::names::MCP_TOOL_PREFIX)
        .and_then(|t| t.strip_prefix(server))
        .is_some_and(|t| t.starts_with("__"))
}

impl AgentType {
    /// The tools a child of this type gets from a parent holding `parent`: the type's own set
    /// (every parent tool when it names none), cut down to the parent's, minus the ones it
    /// disallows outright. Never a tool the parent lacks.
    ///
    /// When the parent has no `shell` tool (the case on Windows without Git Bash), an entry naming
    /// `shell` on its own, in the set or among the disallowed, covers `powershell` too, so a type written for
    /// `shell` keeps a command tool and one that forbids `shell` gets none. An entry with a
    /// specifier does not: it limits calls by tool name.
    pub fn toolset(&self, parent: &[String]) -> Vec<String> {
        let posix_shell = parent.iter().any(|t| t == "shell");
        let wanted: Option<Vec<&str>> = self.tools.as_ref().map(|tools| {
            let mut wanted: Vec<&str> = tools.iter().map(|t| entry_tool(t)).collect();
            if !posix_shell && tools.iter().any(|t| t == "shell") {
                wanted.push("powershell");
            }
            // A child that can start subagents needs the tools that wait for and stop them.
            if wanted.contains(&"agent") {
                wanted.push("job_output");
                wanted.push("job_cancel");
            }
            wanted
        });
        let removed: Vec<&str> = self
            .disallowed
            .iter()
            .filter(|entry| !entry_has_specifier(entry))
            .map(|entry| entry.as_str())
            .flat_map(|entry| {
                let also = (!posix_shell && entry == "shell").then_some("powershell");
                std::iter::once(entry).chain(also)
            })
            .collect();
        let mut out: Vec<String> = Vec::new();
        for tool in parent {
            // `tools: None` types inherit everything else, so the strip lives here rather
            // than in each type.
            if ROOT_ONLY.contains(&tool.as_str()) || tool == TOOL_SEARCH {
                continue;
            }
            let named = wanted
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|w| entry_covers(w, tool)));
            if named && !removed.iter().any(|r| entry_covers(r, tool)) && !out.contains(tool) {
                out.push(tool.clone());
            }
        }
        // `tool_search` loads the deferred definitions of MCP tools, so it follows them: a child
        // with none has nothing to search, and one that has any needs it whatever its type
        // names.
        if parent.iter().any(|t| t == TOOL_SEARCH)
            && !removed.contains(&TOOL_SEARCH)
            && out.iter().any(|t| fontaine_tools::names::is_mcp(t))
        {
            out.push(TOOL_SEARCH.to_string());
        }
        out
    }

    /// Whether a per-call `model` may replace this type's model. A repository's, a plugin's or a
    /// host program's type stays on the session model: it must not end up on a model it steered.
    pub fn accepts_model_override(&self) -> bool {
        match self.origin {
            TypeOrigin::Builtin | TypeOrigin::Config | TypeOrigin::UserFile(_) => true,
            TypeOrigin::ProjectFile(_) | TypeOrigin::PluginFile(_) | TypeOrigin::Session => false,
        }
    }
}

/// The tools of the built-in `explore`; `reviewer` starts from the same set.
const EXPLORE_TOOLS: [&str; 6] = ["ls", "read", "glob", "grep", "lsp", "send_message"];

/// The commands the built-in `reviewer` may run. The trailing ` *` also matches the command
/// with no arguments; `cargo fmt --check` keeps plain `cargo fmt`, which rewrites files, out.
const REVIEWER_SHELL: [&str; 11] = [
    "shell(git diff *)",
    "shell(git log *)",
    "shell(git show *)",
    "shell(git status *)",
    "shell(gh pr view *)",
    "shell(gh pr diff *)",
    "shell(gh pr checks *)",
    "shell(gh issue view *)",
    "shell(cargo clippy *)",
    "shell(cargo test *)",
    "shell(cargo fmt --check *)",
];

const REVIEWER_PROMPT: &str = "You are a reviewer. Review the change you were pointed at: read the code, run the allowed commands to check it, and report your findings with file and line. Do not fix anything, and do not try to work around a command that is refused.";

/// Tools only the user's session has: it alone drafts bug reports, and a child consulting the
/// advisor would multiply its spend and what leaves for the second provider.
const ROOT_ONLY: &[&str] = &[fontaine_tools::descriptor::DRAFT_BUG_REPORT, "advisor"];

#[derive(Clone, Debug, Default)]
pub struct AgentTypes {
    types: BTreeMap<String, AgentType>,
}

impl AgentTypes {
    /// The types every session has: a read-only `explore`, a `reviewer` that also runs a fixed set
    /// of review commands, a `worker` that can edit and run commands, and a `fork` that starts
    /// from the main conversation.
    pub fn builtin() -> AgentTypes {
        let mut types = AgentTypes::default();
        types.insert(AgentType {
            name: "explore".to_string(),
            description: "Reads and searches the workspace; cannot change anything.".to_string(),
            prompt: None,
            tools: Some(EXPLORE_TOOLS.map(String::from).to_vec()),
            disallowed: Vec::new(),
            model: Some(ModelSpec::Main),
            mode: ModeSetting::Inherit,
            effort: None,
            origin: TypeOrigin::Builtin,
            inherits_context: false,
        });
        types.insert(AgentType {
            name: "reviewer".to_string(),
            description: "Reads and searches the workspace and runs a fixed set of review commands; edits nothing through the file tools.".to_string(),
            prompt: Some(REVIEWER_PROMPT.to_string()),
            tools: Some(
                EXPLORE_TOOLS
                    .into_iter()
                    .chain(REVIEWER_SHELL)
                    .map(String::from)
                    .collect(),
            ),
            disallowed: Vec::new(),
            model: Some(ModelSpec::Main),
            mode: ModeSetting::Inherit,
            effort: None,
            origin: TypeOrigin::Builtin,
            inherits_context: false,
        });
        types.insert(AgentType {
            name: "worker".to_string(),
            description: "Reads, edits and writes files and runs shell commands.".to_string(),
            prompt: None,
            tools: Some(
                [
                    "ls",
                    "read",
                    "glob",
                    "grep",
                    "lsp",
                    "edit",
                    "write",
                    "notebook_edit",
                    "memory",
                    "shell",
                    "send_message",
                ]
                .map(String::from)
                .to_vec(),
            ),
            disallowed: Vec::new(),
            model: Some(ModelSpec::Main),
            mode: ModeSetting::Inherit,
            effort: None,
            origin: TypeOrigin::Builtin,
            inherits_context: false,
        });
        types.insert(AgentType {
            name: FORK_TYPE.to_string(),
            description: "Starts from a copy of your conversation, so it needs only a short task in `prompt`."
                .to_string(),
            prompt: None,
            tools: None,
            // Only the main conversation has a human to ask.
            disallowed: vec!["ask_user_question".to_string()],
            model: None,
            mode: ModeSetting::Inherit,
            effort: None,
            origin: TypeOrigin::Builtin,
            inherits_context: true,
        });
        types
    }

    /// Gives the built-in `worker` both board tools and the built-in `explore` and `reviewer` the reading one,
    /// since a type's toolset is only what it names. `fork` names none and inherits whatever its
    /// parent has.
    pub fn enable_teams(&mut self) {
        for (name, tools) in [
            ("worker", &["board_show", "board_update"][..]),
            ("explore", &["board_show"][..]),
            ("reviewer", &["board_show"][..]),
        ] {
            if let Some(agent) = self
                .types
                .get_mut(name)
                .filter(|t| t.origin == TypeOrigin::Builtin)
                && let Some(list) = &mut agent.tools
            {
                list.extend(tools.iter().map(|tool| tool.to_string()));
            }
        }
    }

    /// Adds a type, replacing one of the same name.
    pub fn insert(&mut self, agent_type: AgentType) {
        self.types.insert(agent_type.name.clone(), agent_type);
    }

    pub fn get(&self, name: &str) -> Option<&AgentType> {
        self.types.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.types.keys().map(String::as_str).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &AgentType> {
        self.types.values()
    }

    /// One line per type, for the `agent` tool's description.
    pub fn describe(&self) -> String {
        self.types
            .values()
            .map(|t| format!("- `{}`: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_toolset_is_never_wider_than_the_parents() {
        let worker = AgentTypes::builtin().get("worker").unwrap().clone();
        let parent = names(&["read", "ls", "shell"]);
        assert_eq!(worker.toolset(&parent), names(&["read", "ls", "shell"]));
        assert_eq!(
            worker.toolset(&names(&["read", "send_message", "agent"])),
            names(&["read", "send_message"])
        );
        let explore = AgentTypes::builtin().get("explore").unwrap().clone();
        assert_eq!(explore.toolset(&parent), names(&["read", "ls"]));
    }

    #[test]
    fn without_shell_in_the_parent_shell_entries_cover_powershell() {
        let worker = AgentTypes::builtin().get("worker").unwrap().clone();
        assert_eq!(
            worker.toolset(&names(&["read", "powershell"])),
            names(&["read", "powershell"])
        );
        let mut ty = worker.clone();
        ty.disallowed = names(&["shell"]);
        assert_eq!(
            ty.toolset(&names(&["read", "powershell"])),
            names(&["read"])
        );
        // With a parent `shell` nothing changes: the worker does not get `powershell`.
        assert_eq!(
            worker.toolset(&names(&["read", "shell", "powershell"])),
            names(&["read", "shell"])
        );
        ty.tools = Some(names(&["read", "shell(git *)"]));
        ty.disallowed = Vec::new();
        assert_eq!(
            ty.toolset(&names(&["read", "powershell"])),
            names(&["read"])
        );
    }

    #[test]
    fn the_built_in_types_do_not_get_the_question_tool() {
        let parent = names(&["read", "shell", "ask_user_question"]);
        for ty in AgentTypes::builtin().iter() {
            assert!(
                !ty.toolset(&parent)
                    .contains(&"ask_user_question".to_string()),
                "{}",
                ty.name
            );
        }
    }

    #[test]
    fn no_subagent_gets_draft_bug_report_even_with_no_tool_list() {
        let parent = names(&["read", "draft_bug_report"]);
        let mut types = AgentTypes::builtin();
        let mut open = types.get("worker").unwrap().clone();
        open.name = "open".into();
        open.tools = None;
        types.insert(open);
        for ty in types.iter() {
            assert!(
                !ty.toolset(&parent)
                    .contains(&"draft_bug_report".to_string()),
                "{}",
                ty.name
            );
        }
        assert!(types.get("fork").is_some());
    }

    #[test]
    fn no_subagent_gets_the_advisor_even_when_its_type_lists_it() {
        let parent = names(&["read", "advisor"]);
        let mut types = AgentTypes::builtin();
        let mut open = types.get("worker").unwrap().clone();
        open.name = "open".into();
        open.tools = None;
        types.insert(open);
        let mut named = types.get("worker").unwrap().clone();
        named.name = "named".into();
        named.tools = Some(names(&["read", "advisor"]));
        types.insert(named);
        for ty in types.iter() {
            assert!(
                !ty.toolset(&parent).contains(&"advisor".to_string()),
                "{}",
                ty.name
            );
        }
        assert!(types.get("fork").is_some());
        assert!(types.get("named").is_some());
    }

    #[test]
    fn the_built_in_explore_and_worker_tiers_exclude_models() {
        let parent = names(&["read", "shell", "models"]);
        let types = AgentTypes::builtin();
        for name in ["explore", "worker"] {
            let toolset = types.get(name).unwrap().toolset(&parent);
            assert!(!toolset.contains(&"models".to_string()), "{name}");
        }
    }

    #[test]
    fn a_type_narrowed_to_read_does_not_get_the_task_list() {
        let mut ty = AgentTypes::builtin().get("worker").unwrap().clone();
        ty.tools = Some(names(&["read"]));
        assert_eq!(
            ty.toolset(&names(&["read", "todo_write"])),
            names(&["read"])
        );
        ty.tools = None;
        assert_eq!(
            ty.toolset(&names(&["read", "todo_write"])),
            names(&["read", "todo_write"])
        );
    }

    #[test]
    fn naming_agent_also_grants_job_output_unless_disallowed() {
        let mut ty = AgentTypes::builtin().get("worker").unwrap().clone();
        ty.tools = Some(names(&["read", "agent"]));
        assert_eq!(
            ty.toolset(&names(&["read", "agent", "job_output", "shell"])),
            names(&["read", "agent", "job_output"])
        );
        assert_eq!(
            ty.toolset(&names(&[
                "read",
                "agent",
                "job_output",
                "job_cancel",
                "shell"
            ])),
            names(&["read", "agent", "job_output", "job_cancel"])
        );
        ty.disallowed = names(&["job_cancel"]);
        assert_eq!(
            ty.toolset(&names(&["read", "agent", "job_output", "job_cancel"])),
            names(&["read", "agent", "job_output"]),
            "a disallowed job_cancel is not granted"
        );
        ty.disallowed = Vec::new();
        assert_eq!(
            ty.toolset(&names(&["read", "agent", "shell"])),
            names(&["read", "agent"]),
            "a parent without it never grants it"
        );
        ty.disallowed = names(&["job_output"]);
        assert_eq!(
            ty.toolset(&names(&["read", "agent", "job_output"])),
            names(&["read", "agent"])
        );
        ty.disallowed = names(&["agent"]);
        assert_eq!(
            ty.toolset(&names(&["read", "agent", "job_output"])),
            names(&["read", "job_output"])
        );
        ty.disallowed = Vec::new();
        ty.tools = Some(names(&["read"]));
        assert_eq!(
            ty.toolset(&names(&["read", "agent", "job_output"])),
            names(&["read"])
        );
    }

    #[test]
    fn the_worker_type_can_edit_notebooks() {
        let worker = AgentTypes::builtin().get("worker").unwrap().clone();
        let set = worker.toolset(&names(&["read", "notebook_edit"]));
        assert_eq!(set, names(&["read", "notebook_edit"]));
    }

    #[test]
    fn the_worker_type_can_save_memory_when_its_parent_can() {
        let worker = AgentTypes::builtin().get("worker").unwrap().clone();
        let set = worker.toolset(&names(&["read", "memory"]));
        assert_eq!(set, names(&["read", "memory"]));
        // Never a tool the parent lacks.
        assert_eq!(worker.toolset(&names(&["read"])), names(&["read"]));
    }

    #[test]
    fn disallowed_tools_are_subtracted_and_a_specifier_keeps_the_tool() {
        let mut ty = AgentTypes::builtin().get("worker").unwrap().clone();
        ty.disallowed = names(&["shell", "edit(src/**)"]);
        let set = ty.toolset(&names(&["read", "edit", "shell"]));
        assert_eq!(set, names(&["read", "edit"]));
    }

    #[test]
    fn a_whole_mcp_server_entry_covers_its_tools_in_both_lists() {
        let parent = names(&["read", "mcp__gh__issue", "mcp__gh__pr", "mcp__ghost__x"]);
        for entry in ["mcp__gh", "mcp__gh__*"] {
            let mut ty = AgentTypes::builtin().get("worker").unwrap().clone();
            ty.tools = None;
            ty.disallowed = names(&[entry]);
            assert_eq!(
                ty.toolset(&parent),
                names(&["read", "mcp__ghost__x"]),
                "{entry}"
            );
            ty.disallowed = Vec::new();
            ty.tools = Some(names(&[entry]));
            assert_eq!(
                ty.toolset(&parent),
                names(&["mcp__gh__issue", "mcp__gh__pr"]),
                "{entry}"
            );
        }
    }

    #[test]
    fn tool_search_follows_the_mcp_tools_a_child_gets() {
        let parent = names(&["read", "mcp__gh__issue", "tool_search"]);
        let mut ty = AgentTypes::builtin().get("worker").unwrap().clone();
        ty.tools = Some(names(&["read", "mcp__gh"]));
        assert_eq!(
            ty.toolset(&parent),
            names(&["read", "mcp__gh__issue", "tool_search"])
        );
        ty.tools = Some(names(&["read"]));
        assert_eq!(ty.toolset(&parent), names(&["read"]));
        ty.tools = Some(names(&["read", "mcp__gh", "tool_search"]));
        ty.disallowed = names(&["tool_search"]);
        assert_eq!(ty.toolset(&parent), names(&["read", "mcp__gh__issue"]));
        ty.tools = None;
        ty.disallowed = names(&["mcp__gh"]);
        assert_eq!(ty.toolset(&parent), names(&["read"]));
        assert_eq!(
            ty.toolset(&names(&["read", "mcp__gh__issue"])),
            names(&["read"]),
            "a parent without it has none to give"
        );
    }

    #[test]
    fn a_type_without_tools_takes_all_the_parents() {
        let mut ty = AgentTypes::builtin().get("worker").unwrap().clone();
        ty.tools = None;
        let parent = names(&["read", "mcp__gh__issue"]);
        assert_eq!(ty.toolset(&parent), parent);
    }

    #[test]
    fn the_built_in_explore_runs_on_the_session_model() {
        let types = AgentTypes::builtin();
        let explore = types.get("explore").unwrap();
        assert_eq!(explore.model, Some(ModelSpec::Main));
    }

    #[test]
    fn models_parse_and_a_project_type_refuses_an_override() {
        assert_eq!(ModelSpec::parse("$main"), Ok(ModelSpec::Main));
        assert_eq!(
            ModelSpec::parse("work/claude"),
            Ok(ModelSpec::Model("work/claude".into()))
        );
        assert_eq!(
            ModelSpec::parse("sonnet"),
            Ok(ModelSpec::Named("sonnet".into()))
        );
        assert!(ModelSpec::parse("inherit").is_err());
        assert!(ModelSpec::parse("/x").is_err());
        assert!(ModelSpec::parse("$other").is_err());
        let mut ty = AgentTypes::builtin().get("explore").unwrap().clone();
        assert!(ty.accepts_model_override());
        ty.origin = TypeOrigin::ProjectFile("x.md".into());
        assert!(!ty.accepts_model_override());
        ty.origin = TypeOrigin::PluginFile("x.md".into());
        assert!(!ty.accepts_model_override());
    }

    #[test]
    fn specs_round_trip_through_their_text() {
        for text in [
            "coding",
            "work/claude",
            "a/b,coding,$main",
            "$main",
            "$guardian_default",
        ] {
            let spec = ModelSpec::parse(text).unwrap();
            assert_eq!(spec.as_text(), text);
            assert_eq!(ModelSpec::parse(&spec.as_text()), Ok(spec));
        }
        assert_eq!(
            ModelSpec::parse("a/b, coding"),
            Ok(ModelSpec::List(vec!["a/b".into(), "coding".into()]))
        );
        assert_eq!(
            ModelSpec::from_entries(&["coding".to_string()]),
            Ok(ModelSpec::Named("coding".into()))
        );
        assert!(ModelSpec::from_entries(&["a/b".into(), "$guardian_default".into()]).is_err());
        assert!(ModelSpec::from_entries(&[]).is_err());
    }

    #[test]
    fn teams_give_the_built_in_types_the_board_tools() {
        let mut types = AgentTypes::builtin();
        types.enable_teams();
        let parent = names(&["read", "board_show", "board_update", "send_message"]);
        assert_eq!(
            types.get("worker").unwrap().toolset(&parent),
            names(&["read", "board_show", "board_update", "send_message"])
        );
        assert_eq!(
            types.get("explore").unwrap().toolset(&parent),
            names(&["read", "board_show", "send_message"])
        );
        assert_eq!(types.get("fork").unwrap().toolset(&parent), parent);
        let plain = AgentTypes::builtin();
        assert_eq!(
            plain.get("worker").unwrap().toolset(&parent),
            names(&["read", "send_message"])
        );
    }

    #[test]
    fn the_reviewer_is_the_explore_tools_plus_shell() {
        let types = AgentTypes::builtin();
        let reviewer = types.get("reviewer").unwrap();
        assert_eq!(reviewer.origin, TypeOrigin::Builtin);
        assert!(reviewer.prompt.is_some());
        let listed = reviewer.tools.as_ref().unwrap();
        for tool in ["edit", "write", "notebook_edit", "memory"] {
            assert!(!listed.iter().any(|e| entry_tool(e) == tool), "{tool}");
        }
        let parent = names(&[
            "ls",
            "read",
            "glob",
            "grep",
            "lsp",
            "send_message",
            "edit",
            "write",
            "notebook_edit",
            "memory",
            "shell",
            "powershell",
        ]);
        let mut got = reviewer.toolset(&parent);
        got.sort();
        let mut want: Vec<String> = EXPLORE_TOOLS.iter().map(|t| t.to_string()).collect();
        want.push("shell".to_string());
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn the_reviewer_has_no_command_tool_without_a_posix_shell() {
        let types = AgentTypes::builtin();
        let set = types
            .get("reviewer")
            .unwrap()
            .toolset(&names(&["read", "powershell"]));
        assert_eq!(set, names(&["read"]));
    }

    #[test]
    fn teams_give_the_reviewer_the_reading_board_tool_only() {
        let mut types = AgentTypes::builtin();
        types.enable_teams();
        let set =
            types
                .get("reviewer")
                .unwrap()
                .toolset(&names(&["read", "board_show", "board_update"]));
        assert_eq!(set, names(&["read", "board_show"]));
    }

    #[test]
    fn entries_split_into_tool_and_specifier() {
        assert_eq!(entry_tool("shell(git *)"), "shell");
        assert_eq!(entry_tool("read"), "read");
        assert!(entry_has_specifier("shell(git *)"));
        assert!(!entry_has_specifier("shell"));
    }
}
