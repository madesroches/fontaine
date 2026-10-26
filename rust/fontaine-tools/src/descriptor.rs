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
];

pub fn find(name: &str) -> Option<&'static ToolDescriptor> {
    TOOLS.iter().find(|d| d.name == name)
}

/// The tool a Claude Code tool name stands for.
pub fn find_claude(claude_name: &str) -> Option<&'static ToolDescriptor> {
    TOOLS.iter().find(|d| d.claude.contains(&claude_name))
}

/// Every Claude Code name the table knows, in table order.
pub fn claude_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().flat_map(|d| d.claude.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

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
            assert!(!d.claude.is_empty(), "{} has no Claude Code name", d.name);
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
            if d.destructive == Destructive::Never {
                assert_eq!(d.risk, RiskClass::ReadOnly, "{}", d.name);
            }
        }
    }

    #[test]
    fn rows_are_pinned() {
        use Destructive as D;
        use RiskClass as R;
        use Specifier as S;
        type Pinned = (&'static str, R, bool, D, S, &'static [&'static str]);
        let expected: [Pinned; 18] = [
            ("shell", R::Arbitrary, false, D::Shell, S::Shell, &["Bash"]),
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
            ("ls", R::ReadOnly, false, D::Never, S::Path, &["LS"]),
            ("glob", R::ReadOnly, false, D::Never, S::Path, &["Glob"]),
            ("grep", R::ReadOnly, false, D::Never, S::Path, &["Grep"]),
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
