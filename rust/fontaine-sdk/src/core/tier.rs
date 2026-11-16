use serde::{Deserialize, Serialize};

/// Which pipeline stage produced a gate decision; the audit log, telemetry, the headless
/// `gate_decision` event and eval `expected_tier` all carry exactly these values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// `plan_block` is the tier an older session's ledger and audit log wrote for a plan-mode
    /// denial; they are read back on resume.
    #[serde(alias = "plan_block")]
    DenyRule,
    AskRule,
    /// `critical_path` is the tier an older session's ledger and audit log wrote for a
    /// recognized deletion of a critical place; they are read back on resume.
    #[serde(alias = "critical_path")]
    ProtectedPath,
    Hook,
    ReadOnlyFastPath,
    /// `auto` only: an edit to a tracked, new or session-created file, after the floor.
    TrackedEdit,
    /// `auto` only: a file-tool write inside the session scratch directory, after the floor.
    ScratchWrite,
    Tier05,
    /// An `agent` spawn after the floor: the child's own calls are gated, so the spawn is not
    /// judged by the guardian.
    Spawn,
    /// `send_message` and `board_update` after the floor, in every mode: they reach only agents
    /// of the same tree, whose own calls are gated.
    Coordination,
    AllowRule,
    Mode,
    Screen,
    Guardian,
    DontAsk,
    Human,
    /// The session escape hatch `/guardian-disable` is on: the call ran before the floor and
    /// the guardian, so nothing judged it.
    GuardianOff,
    /// AFK mode: the temperature ceiling stopped the session, so every call is denied until the
    /// user has reviewed it; also the tier of the audit events that toggle AFK.
    Afk,
    /// `draft_bug_report` after the floor, in every mode: it only writes a redacted draft to the
    /// data directory, with no network I/O, so the guardian does not judge it.
    BugDraft,
    /// The audit event for a bug report the human submitted with `/bug`.
    BugReport,
}

impl Tier {
    pub const ALL: [Tier; 20] = [
        Tier::DenyRule,
        Tier::AskRule,
        Tier::ProtectedPath,
        Tier::Hook,
        Tier::ReadOnlyFastPath,
        Tier::TrackedEdit,
        Tier::ScratchWrite,
        Tier::Tier05,
        Tier::Spawn,
        Tier::Coordination,
        Tier::AllowRule,
        Tier::Mode,
        Tier::Screen,
        Tier::Guardian,
        Tier::DontAsk,
        Tier::Human,
        Tier::GuardianOff,
        Tier::Afk,
        Tier::BugDraft,
        Tier::BugReport,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::DenyRule => "deny_rule",
            Tier::AskRule => "ask_rule",
            Tier::ProtectedPath => "protected_path",
            Tier::Hook => "hook",
            Tier::ReadOnlyFastPath => "read_only_fast_path",
            Tier::TrackedEdit => "tracked_edit",
            Tier::ScratchWrite => "scratch_write",
            Tier::Tier05 => "tier05",
            Tier::Spawn => "spawn",
            Tier::Coordination => "coordination",
            Tier::AllowRule => "allow_rule",
            Tier::Mode => "mode",
            Tier::Screen => "screen",
            Tier::Guardian => "guardian",
            Tier::DontAsk => "dont_ask",
            Tier::Human => "human",
            Tier::GuardianOff => "guardian_off",
            Tier::Afk => "afk",
            Tier::BugDraft => "bug_draft",
            Tier::BugReport => "bug_report",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names_match_as_str() {
        for tier in Tier::ALL {
            let json = toml::Value::try_from(tier).unwrap();
            assert_eq!(json.as_str(), Some(tier.as_str()));
        }
    }
}
