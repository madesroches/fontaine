//! What a status-line script sees: a snapshot of the session, and the styled runs it builds.
//!
//! A run names a [`Role`], not a colour, so this module needs no terminal types: the interface
//! maps each role to a style from its theme.

use std::collections::BTreeMap;

/// The meaning a run is drawn in. The interface owns the mapping to a terminal style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Normal,
    Dim,
    Bold,
    Accent,
    Warning,
    WarningBold,
    Success,
    Error,
    User,
    Heading,
    Link,
    Shell,
    ModeManual,
    ModeAccept,
    ModeAuto,
    ModeAfk,
}

/// The names a script gives to `ctx.style`, with the role each one is.
pub const STYLE_NAMES: &[(&str, Role)] = &[
    ("normal", Role::Normal),
    ("dim", Role::Dim),
    ("bold", Role::Bold),
    ("accent", Role::Accent),
    ("warning", Role::Warning),
    ("warning_bold", Role::WarningBold),
    ("success", Role::Success),
    ("error", Role::Error),
    ("user", Role::User),
    ("heading", Role::Heading),
    ("link", Role::Link),
    ("shell", Role::Shell),
    ("mode.manual", Role::ModeManual),
    ("mode.accept", Role::ModeAccept),
    ("mode.auto", Role::ModeAuto),
    ("mode.afk", Role::ModeAfk),
];

impl Role {
    /// The role `ctx.style(name)` names.
    pub fn named(name: &str) -> Option<Role> {
        STYLE_NAMES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, role)| *role)
    }
}

/// The preformatted pieces a template or `ctx.segment` names. A segment with nothing to show is
/// empty.
pub const SEGMENTS: &[&str] = &[
    "mode",
    "vim",
    "location",
    "dir",
    "branch",
    "model",
    "quota",
    "quota.guardian",
    "effort",
    "agents",
    "jobs",
    "scheduled",
    "bug_drafts",
    "goal",
    "remote",
    "hint",
    "context",
    "cost",
    "session_id",
    "version",
    "command",
];

/// The bare numbers a template expands to their plain text, for thresholds and hand-written
/// wording.
pub const VALUES: &[&str] = &[
    "context.used_percentage",
    "context.remaining_percentage",
    "context.total_tokens",
    "context.window_size",
    "cost.total_cost_usd",
];

/// A piece of the line drawn in one role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledText {
    pub text: String,
    pub role: Role,
    /// Cut from the left, with a leading ellipsis, when the line is wider than the row: the
    /// text whose end matters most, such as a command's line.
    pub tail: bool,
}

impl StyledText {
    pub fn new(text: impl Into<String>, role: Role) -> StyledText {
        StyledText {
            text: text.into(),
            role,
            tail: false,
        }
    }

    /// The same text, cut from the left when the line is too wide.
    pub fn tail(mut self) -> StyledText {
        self.tail = true;
        self
    }
}

/// The numbers a script thresholds on. A number that is not known yet is `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    pub context_used_percentage: Option<u32>,
    pub context_remaining_percentage: Option<u32>,
    pub context_total_tokens: Option<u64>,
    pub context_window_size: Option<u32>,
    pub cost_total_cost_usd: Option<f64>,
}

impl Values {
    /// The plain text of the value `name` names: `None` for a name that is not a value, and
    /// `Some(None)` for a value that is not known.
    pub fn text(&self, name: &str) -> Option<Option<String>> {
        Some(match name {
            "context.used_percentage" => self.context_used_percentage.map(|n| n.to_string()),
            "context.remaining_percentage" => {
                self.context_remaining_percentage.map(|n| n.to_string())
            }
            "context.total_tokens" => self.context_total_tokens.map(|n| n.to_string()),
            "context.window_size" => self.context_window_size.map(|n| n.to_string()),
            "cost.total_cost_usd" => self.cost_total_cost_usd.map(|cost| format!("{cost:.4}")),
            _ => return None,
        })
    }
}

/// The session as one render of the status line sees it. Nothing in it is read after the
/// snapshot is built, so a script is pure over it.
#[derive(Clone, Debug, Default)]
pub struct StatuslineSnapshot {
    pub segments: BTreeMap<&'static str, Vec<StyledText>>,
    pub values: Values,
    /// `PermissionMode::as_str()`.
    pub mode: String,
    pub model: String,
    /// The working directory as the bar writes it, home as `~` and cut from the left.
    pub dir: String,
    pub branch: Option<String>,
    pub cwd: String,
    pub project_dir: String,
    pub session_id: String,
    pub version: String,
    /// `Effort::as_str()`.
    pub effort: Option<String>,
    /// The input editor's vim state, such as `vim:insert`.
    pub vim: Option<String>,
    pub afk: bool,
    pub afk_stopped: bool,
    pub guardian_off: bool,
    pub running_agents: usize,
    pub background_shells: usize,
    pub background_monitors: usize,
    pub scheduled: usize,
    pub bug_drafts: Option<usize>,
    /// The active goal's age in seconds, and whether it is paused.
    pub goal: Option<(u64, bool)>,
    pub remote: Option<String>,
    pub cost_is_estimate: bool,
    pub command_line: Option<String>,
}

impl StatuslineSnapshot {
    /// The runs of the segment `name`; `None` for a name that is not a segment.
    pub fn segment(&self, name: &str) -> Option<&[StyledText]> {
        let known = SEGMENTS.iter().find(|candidate| **candidate == name)?;
        Some(self.segments.get(known).map_or(&[][..], Vec::as_slice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_name_names_one_role_and_every_role_has_a_name() {
        for (name, role) in STYLE_NAMES {
            assert_eq!(Role::named(name), Some(*role), "{name}");
        }
        let roles: Vec<Role> = STYLE_NAMES.iter().map(|(_, role)| *role).collect();
        for (i, role) in roles.iter().enumerate() {
            assert!(!roles[..i].contains(role), "{role:?} is named twice");
        }
        assert_eq!(Role::named("nope"), None);
    }

    #[test]
    fn the_name_tables_hold_no_duplicates_and_no_overlap() {
        for table in [SEGMENTS, VALUES] {
            for (i, name) in table.iter().enumerate() {
                assert!(!table[..i].contains(name), "{name} is listed twice");
            }
        }
        for name in VALUES {
            assert!(!SEGMENTS.contains(name), "{name} is a segment and a value");
        }
    }

    #[test]
    fn every_value_name_has_text_and_an_unknown_one_has_none() {
        let known = Values {
            context_used_percentage: Some(42),
            context_remaining_percentage: Some(58),
            context_total_tokens: Some(4_200),
            context_window_size: Some(10_000),
            cost_total_cost_usd: Some(0.1234),
        };
        for name in VALUES {
            assert!(
                matches!(known.text(name), Some(Some(_))),
                "{name} has no text when known"
            );
            assert_eq!(Values::default().text(name), Some(None), "{name}");
        }
        assert_eq!(
            known.text("cost.total_cost_usd"),
            Some(Some("0.1234".into()))
        );
        assert_eq!(known.text("model"), None);
    }

    #[test]
    fn a_segment_without_runs_is_empty_and_an_unknown_name_is_not_a_segment() {
        let snapshot = StatuslineSnapshot::default();
        for name in SEGMENTS {
            assert_eq!(snapshot.segment(name), Some(&[][..]), "{name}");
        }
        assert_eq!(snapshot.segment("nope"), None);
    }
}
