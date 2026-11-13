use crate::risk::RiskClass;

/// How `destructive::derive` treats a tool's calls. The facts that decide `FileWrite` and
/// `Shell` live in `fontaine-core`; the table only names which analysis applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destructive {
    Never,
    Always,
    FileWrite,
    Shell,
}

/// What a permission rule's `(specifier)` means for a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Specifier {
    /// The tool takes no specifier; a rule can only name it whole.
    None,
    Shell,
    Domain,
    Path,
    /// One name, matched against a per-tool argument.
    Name,
}

/// Every decision input a tool contributes, in one reviewed row. The risk class lives here and
/// never on the tool, so a tool cannot declare itself safe.
#[derive(Clone, Copy, Debug)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub risk: RiskClass,
    /// Names no path, so workspace containment has nothing to check.
    pub path_free: bool,
    pub destructive: Destructive,
    pub specifier: Specifier,
    /// Claude Code names for this tool; the first is canonical.
    pub claude: &'static [&'static str],
    /// Whether the file the call names is saved before the tool changes it. A tool absent from
    /// the table is not saved; the table test requires it of every `FileWrite` row.
    pub checkpointed: bool,
}

impl ToolDescriptor {
    /// Whether the tool runs a command line, so command analysis applies to its calls.
    pub fn shell_class(&self) -> bool {
        self.specifier == Specifier::Shell
    }
}

/// Whether the tool named `name` runs a command line.
pub fn is_shell_class(name: &str) -> bool {
    find(name).is_some_and(ToolDescriptor::shell_class)
}

/// Tools that only reach agents of the same tree: every call those agents make is gated on its
/// own, so the permission gate lets these through without a guardian verdict or a prompt.
const COORDINATION: [&str; 2] = ["send_message", "board_update"];

/// The tool that queues a redacted bug-report draft in the data directory for the user to review.
pub const DRAFT_BUG_REPORT: &str = "draft_bug_report";

/// Whether the tool named `name` is an agent-coordination tool.
pub fn is_coordination(name: &str) -> bool {
    COORDINATION.contains(&name)
}

const fn row(
    name: &'static str,
    risk: RiskClass,
    path_free: bool,
    destructive: Destructive,
    specifier: Specifier,
    claude: &'static [&'static str],
) -> ToolDescriptor {
    ToolDescriptor {
        name,
        risk,
        path_free,
        destructive,
        specifier,
        claude,
        checkpointed: matches!(destructive, Destructive::FileWrite),
    }
}

/// A tool absent from this table is the riskiest class everywhere it is looked up, so a
/// forgotten row can only make a tool stricter.
pub static TOOLS: &[ToolDescriptor] = &[
    row(
        "shell",
        RiskClass::Arbitrary,
        false,
        Destructive::Shell,
        Specifier::Shell,
        &["Bash"],
    ),
    row(
        "job_output",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &[
            "BashOutput",
            "TaskOutput",
            "BashOutputTool",
            "AgentOutput",
            "AgentOutputTool",
        ],
    ),
    row(
        "job_cancel",
        RiskClass::Arbitrary,
        true,
        Destructive::Never,
        Specifier::None,
        &["KillShell", "KillBash", "TaskStop"],
    ),
    row(
        "monitor",
        RiskClass::Arbitrary,
        false,
        Destructive::Shell,
        Specifier::Shell,
        &["Monitor"],
    ),
    row(
        "powershell",
        RiskClass::Arbitrary,
        false,
        Destructive::Always,
        Specifier::None,
        &["PowerShell"],
    ),
    row(
        "read",
        RiskClass::ReadOnly,
        false,
        Destructive::Never,
        Specifier::Path,
        &["Read"],
    ),
    row(
        "edit",
        RiskClass::WorkspaceWrite,
        false,
        Destructive::FileWrite,
        Specifier::Path,
        &["Edit", "MultiEdit"],
    ),
    row(
        "write",
        RiskClass::WorkspaceWrite,
        false,
        Destructive::FileWrite,
        Specifier::Path,
        &["Write"],
    ),
    row(
        "notebook_edit",
        RiskClass::WorkspaceWrite,
        false,
        Destructive::FileWrite,
        Specifier::Path,
        &["NotebookEdit"],
    ),
    row(
        "ls",
        RiskClass::ReadOnly,
        false,
        Destructive::Never,
        Specifier::Path,
        &["LS"],
    ),
    row(
        "glob",
        RiskClass::ReadOnly,
        false,
        Destructive::Never,
        Specifier::Path,
        &["Glob"],
    ),
    row(
        "grep",
        RiskClass::ReadOnly,
        false,
        Destructive::Never,
        Specifier::Path,
        &["Grep"],
    ),
    row(
        "lsp",
        RiskClass::ReadOnly,
        false,
        Destructive::Never,
        Specifier::Path,
        &["LSP"],
    ),
    row(
        "webfetch",
        RiskClass::Arbitrary,
        false,
        Destructive::Always,
        Specifier::Domain,
        &["WebFetch"],
    ),
    row(
        "websearch",
        RiskClass::Arbitrary,
        false,
        Destructive::Always,
        Specifier::None,
        &["WebSearch"],
    ),
    row(
        "agent",
        RiskClass::Arbitrary,
        false,
        Destructive::Always,
        Specifier::Name,
        &["Agent", "Task"],
    ),
    row(
        "skill",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::Name,
        &["Skill"],
    ),
    row(
        "send_message",
        RiskClass::Arbitrary,
        false,
        Destructive::Always,
        Specifier::None,
        &["SendMessage"],
    ),
    row(
        "board_show",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &["TaskList", "TaskGet"],
    ),
    row(
        "board_update",
        RiskClass::Arbitrary,
        false,
        Destructive::Always,
        Specifier::None,
        &["TaskCreate", "TaskUpdate"],
    ),
    row(
        "ask_user_question",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &["AskUserQuestion"],
    ),
    row(
        "todo_write",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &["TodoWrite"],
    ),
    row(
        "cron_create",
        RiskClass::Arbitrary,
        true,
        Destructive::Always,
        Specifier::None,
        &["CronCreate"],
    ),
    row(
        "cron_jobs",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &["CronList"],
    ),
    row(
        "cron_delete",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &["CronDelete"],
    ),
    row(
        "schedule_wakeup",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &["ScheduleWakeup"],
    ),
    row(
        "models",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &[],
    ),
    // Reads only the in-memory registry of tool definitions and takes no path. Loading a
    // definition grants nothing: a call to the loaded tool takes its own gate.
    row(
        "tool_search",
        RiskClass::ReadOnly,
        true,
        Destructive::Never,
        Specifier::None,
        &[],
    ),
    row(
        DRAFT_BUG_REPORT,
        RiskClass::Arbitrary,
        true,
        Destructive::Always,
        Specifier::None,
        &[],
    ),
    // Arbitrary because the conversation and a model-written question go to a second provider.
    row(
        "advisor",
        RiskClass::Arbitrary,
        true,
        Destructive::Never,
        Specifier::None,
        &[],
    ),
    // The listing is server-declared text and its read is a server round trip: both take the
    // same gate as everything else MCP, like the `mcp__*` tools.
    row(
        "list_mcp_resources",
        RiskClass::Arbitrary,
        true,
        Destructive::Never,
        Specifier::None,
        &["ListMcpResources"],
    ),
    row(
        "read_mcp_resource",
        RiskClass::Arbitrary,
        true,
        Destructive::Never,
        Specifier::None,
        &["ReadMcpResource"],
    ),
    // Writes the session's persistent memory store, outside any git checkout; the
    // WorkspaceWrite class keeps it out of every fast path, and auto routes it to the guardian.
    row(
        "memory",
        RiskClass::WorkspaceWrite,
        true,
        Destructive::Always,
        Specifier::None,
        &[],
    ),
];

pub fn find(name: &str) -> Option<&'static ToolDescriptor> {
    TOOLS.iter().find(|d| d.name == name)
}

/// The tool a Claude Code tool name stands for.
pub fn find_claude(claude_name: &str) -> Option<&'static ToolDescriptor> {
    TOOLS.iter().find(|d| d.claude.contains(&claude_name))
}

/// Every tool name in the table, in table order.
pub fn names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().map(|d| d.name)
}

/// Every Claude Code name the table knows, in table order.
pub fn claude_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().flat_map(|d| d.claude.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Tools Claude Code has no counterpart for.
    const FONTAINE_ONLY: &[&str] = &["models", "tool_search", DRAFT_BUG_REPORT, "advisor", "memory"];

    #[test]
    fn names_are_unique_rule_names() {
        let mut seen = BTreeSet::new();
        for d in TOOLS {
            assert!(seen.insert(d.name), "{} appears twice", d.name);
            assert!(
                d.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{} is not a valid rule name",
                d.name
            );
            assert_eq!(
                d.claude.is_empty(),
                FONTAINE_ONLY.contains(&d.name),
                "{} needs a Claude Code name unless it is Fontaine-only",
                d.name
            );
        }
        let aliases: Vec<_> = claude_names().collect();
        let unique: BTreeSet<_> = aliases.iter().collect();
        assert_eq!(aliases.len(), unique.len(), "a Claude Code name repeats");
    }

    #[test]
    fn path_policy_is_consistent() {
        for d in TOOLS {
            if d.path_free {
                assert_ne!(d.specifier, Specifier::Path, "{}", d.name);
            }
            if matches!(d.destructive, Destructive::FileWrite) || d.specifier == Specifier::Path {
                assert!(!d.path_free, "{} names a path", d.name);
            }
            // `job_cancel` stops only a process or subagent this session started, by id, and
            // names no path; `advisor` changes nothing but sends the conversation to a second
            // model, which is why it is not read-only; the two MCP resource tools stay
            // arbitrary like every other MCP surface — MCP is never fast-pathed.
            if d.destructive == Destructive::Never
                && !matches!(
                    d.name,
                    "job_cancel" | "advisor" | "list_mcp_resources" | "read_mcp_resource"
                )
            {
                assert_eq!(d.risk, RiskClass::ReadOnly, "{}", d.name);
            }
        }
    }

    #[test]
    fn every_file_writing_tool_is_checkpointed() {
        for d in TOOLS {
            assert_eq!(
                d.checkpointed,
                d.destructive == Destructive::FileWrite,
                "{}",
                d.name
            );
        }
        let saved: Vec<_> = TOOLS
            .iter()
            .filter(|d| d.checkpointed)
            .map(|d| d.name)
            .collect();
        assert_eq!(saved, ["edit", "write", "notebook_edit"]);
    }

    #[test]
    fn the_shell_class_is_every_row_that_takes_a_command_line() {
        let shell_class: Vec<_> = TOOLS
            .iter()
            .filter(|d| d.shell_class())
            .map(|d| d.name)
            .collect();
        assert_eq!(shell_class, ["shell", "monitor"]);
        assert!(is_shell_class("monitor") && !is_shell_class("job_output"));
        assert!(!is_shell_class("no_such_tool"));
        assert_eq!(find_claude("Monitor").map(|d| d.name), Some("monitor"));
        assert_eq!(find_claude("Bash").map(|d| d.name), Some("shell"));
        // PowerShell text never meets the POSIX analyses.
        assert!(!is_shell_class("powershell"));
        assert_eq!(
            find_claude("PowerShell").map(|d| d.name),
            Some("powershell")
        );
    }

    #[test]
    fn rows_are_pinned() {
        use Destructive as D;
        use RiskClass as R;
        use Specifier as S;
        type Pinned = (&'static str, R, bool, D, S, &'static [&'static str]);
        let expected: [Pinned; 32] = [
            ("shell", R::Arbitrary, false, D::Shell, S::Shell, &["Bash"]),
            (
                "job_output",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &[
                    "BashOutput",
                    "TaskOutput",
                    "BashOutputTool",
                    "AgentOutput",
                    "AgentOutputTool",
                ],
            ),
            (
                "job_cancel",
                R::Arbitrary,
                true,
                D::Never,
                S::None,
                &["KillShell", "KillBash", "TaskStop"],
            ),
            (
                "monitor",
                R::Arbitrary,
                false,
                D::Shell,
                S::Shell,
                &["Monitor"],
            ),
            (
                "powershell",
                R::Arbitrary,
                false,
                D::Always,
                S::None,
                &["PowerShell"],
            ),
            ("read", R::ReadOnly, false, D::Never, S::Path, &["Read"]),
            (
                "edit",
                R::WorkspaceWrite,
                false,
                D::FileWrite,
                S::Path,
                &["Edit", "MultiEdit"],
            ),
            (
                "write",
                R::WorkspaceWrite,
                false,
                D::FileWrite,
                S::Path,
                &["Write"],
            ),
            (
                "notebook_edit",
                R::WorkspaceWrite,
                false,
                D::FileWrite,
                S::Path,
                &["NotebookEdit"],
            ),
            ("ls", R::ReadOnly, false, D::Never, S::Path, &["LS"]),
            ("glob", R::ReadOnly, false, D::Never, S::Path, &["Glob"]),
            ("grep", R::ReadOnly, false, D::Never, S::Path, &["Grep"]),
            ("lsp", R::ReadOnly, false, D::Never, S::Path, &["LSP"]),
            (
                "webfetch",
                R::Arbitrary,
                false,
                D::Always,
                S::Domain,
                &["WebFetch"],
            ),
            (
                "websearch",
                R::Arbitrary,
                false,
                D::Always,
                S::None,
                &["WebSearch"],
            ),
            (
                "agent",
                R::Arbitrary,
                false,
                D::Always,
                S::Name,
                &["Agent", "Task"],
            ),
            ("skill", R::ReadOnly, true, D::Never, S::Name, &["Skill"]),
            (
                "send_message",
                R::Arbitrary,
                false,
                D::Always,
                S::None,
                &["SendMessage"],
            ),
            (
                "board_show",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &["TaskList", "TaskGet"],
            ),
            (
                "board_update",
                R::Arbitrary,
                false,
                D::Always,
                S::None,
                &["TaskCreate", "TaskUpdate"],
            ),
            (
                "ask_user_question",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &["AskUserQuestion"],
            ),
            (
                "todo_write",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &["TodoWrite"],
            ),
            (
                "cron_create",
                R::Arbitrary,
                true,
                D::Always,
                S::None,
                &["CronCreate"],
            ),
            (
                "cron_jobs",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &["CronList"],
            ),
            (
                "cron_delete",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &["CronDelete"],
            ),
            (
                "schedule_wakeup",
                R::ReadOnly,
                true,
                D::Never,
                S::None,
                &["ScheduleWakeup"],
            ),
            ("models", R::ReadOnly, true, D::Never, S::None, &[]),
            ("tool_search", R::ReadOnly, true, D::Never, S::None, &[]),
            (
                "draft_bug_report",
                R::Arbitrary,
                true,
                D::Always,
                S::None,
                &[],
            ),
            ("advisor", R::Arbitrary, true, D::Never, S::None, &[]),
            (
                "list_mcp_resources",
                R::Arbitrary,
                true,
                D::Never,
                S::None,
                &["ListMcpResources"],
            ),
            (
                "read_mcp_resource",
                R::Arbitrary,
                true,
                D::Never,
                S::None,
                &["ReadMcpResource"],
            ),
            ("memory", R::WorkspaceWrite, true, D::Always, S::None, &[]),
        ];
        let actual: Vec<_> = TOOLS
            .iter()
            .map(|d| {
                (
                    d.name,
                    d.risk,
                    d.path_free,
                    d.destructive,
                    d.specifier,
                    d.claude,
                )
            })
            .collect();
        assert_eq!(actual, expected);
    }
}
