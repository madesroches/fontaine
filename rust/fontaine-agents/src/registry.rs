//! The agent types a session can spawn: the built-in three, `[agents.types]` entries and agent
//! files in one registry. A type names a model, a toolset and a mode; the toolset a child
//! really gets is that set cut down to what its parent has.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::mode::ModeSetting;

/// The model an agent runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSpec {
    /// The session's model.
    Main,
    /// The top-ranked validated guardian model the accounts offer, or the session's model when
    /// none is.
    GuardianDefault,
    /// `<account>/<model>`.
    Model(String),
}

impl ModelSpec {
    pub fn parse(text: &str) -> Result<ModelSpec, String> {
        match text {
            "$main" => Ok(ModelSpec::Main),
            "$guardian_default" => Ok(ModelSpec::GuardianDefault),
            other => match other.split_once('/') {
                Some((account, model)) if !account.is_empty() && !model.is_empty() => {
                    Ok(ModelSpec::Model(other.to_string()))
                }
                _ => Err(format!(
                    "`{other}` is not a model: use `$main`, `$guardian_default` or `<account>/<model>`"
                )),
            },
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            ModelSpec::Main => "$main".to_string(),
            ModelSpec::GuardianDefault => "$guardian_default".to_string(),
            ModelSpec::Model(text) => text.clone(),
        }
    }
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
    pub fn toolset(&self, parent: &[String]) -> Vec<String> {
        let wanted: Option<Vec<&str>> = self.tools.as_ref().map(|tools| {
            let mut wanted: Vec<&str> = tools.iter().map(|t| entry_tool(t)).collect();
            // A child that can start subagents needs the tool that waits for them.
            if wanted.contains(&"agent") {
                wanted.push("job_output");
            }
            wanted
        });
        let removed: Vec<&str> = self
            .disallowed
            .iter()
            .filter(|entry| !entry_has_specifier(entry))
            .map(|entry| entry.as_str())
            .collect();
        let mut out: Vec<String> = Vec::new();
        for tool in parent {
            let named = wanted
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|w| entry_covers(w, tool)));
            if named && !removed.iter().any(|r| entry_covers(r, tool)) && !out.contains(tool) {
                out.push(tool.clone());
            }
        }
        out
    }

    /// Whether a per-call `model` may replace this type's model. A repository's or a plugin's
    /// type stays on the session model: it must not end up on a model it steered.
    pub fn accepts_model_override(&self) -> bool {
        match self.origin {
            TypeOrigin::Builtin | TypeOrigin::Config | TypeOrigin::UserFile(_) => true,
            TypeOrigin::ProjectFile(_) | TypeOrigin::PluginFile(_) => false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AgentTypes {
    types: BTreeMap<String, AgentType>,
}

impl AgentTypes {
    /// The types every session has: a read-only `explore`, a `worker` that can edit and run
    /// commands, and a `fork` that starts from the main conversation.
    pub fn builtin() -> AgentTypes {
        let mut types = AgentTypes::default();
        types.insert(AgentType {
            name: "explore".to_string(),
            description: "Reads and searches the workspace; cannot change anything.".to_string(),
            prompt: None,
            tools: Some(
                ["ls", "read", "glob", "grep", "lsp", "send_message"]
                    .map(String::from)
                    .to_vec(),
            ),
            disallowed: Vec::new(),
            model: Some(ModelSpec::Main),
            mode: ModeSetting::Inherit,
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
                    "shell",
                    "send_message",
                ]
                .map(String::from)
                .to_vec(),
            ),
            disallowed: Vec::new(),
            model: Some(ModelSpec::Main),
            mode: ModeSetting::Inherit,
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
            origin: TypeOrigin::Builtin,
            inherits_context: true,
        });
        types
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
        assert!(ModelSpec::parse("sonnet").is_err());
        assert!(ModelSpec::parse("/x").is_err());
        let mut ty = AgentTypes::builtin().get("explore").unwrap().clone();
        assert!(ty.accepts_model_override());
        ty.origin = TypeOrigin::ProjectFile("x.md".into());
        assert!(!ty.accepts_model_override());
        ty.origin = TypeOrigin::PluginFile("x.md".into());
        assert!(!ty.accepts_model_override());
    }

    #[test]
    fn entries_split_into_tool_and_specifier() {
        assert_eq!(entry_tool("shell(git *)"), "shell");
        assert_eq!(entry_tool("read"), "read");
        assert!(entry_has_specifier("shell(git *)"));
        assert!(!entry_has_specifier("shell"));
    }
}
