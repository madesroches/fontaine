use serde::{Deserialize, Serialize};

/// Which pipeline stage produced a gate decision; the audit log, telemetry, the headless
/// `gate_decision` event and eval `expected_tier` all carry exactly these values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    DenyRule,
    AskRule,
    CriticalPath,
    ProtectedPath,
    Hook,
    ReadOnlyFastPath,
    Tier05,
    AllowRule,
    Mode,
    Guardian,
    DontAsk,
    PlanBlock,
    Human,
}

impl Tier {
    pub const ALL: [Tier; 13] = [
        Tier::DenyRule,
        Tier::AskRule,
        Tier::CriticalPath,
        Tier::ProtectedPath,
        Tier::Hook,
        Tier::ReadOnlyFastPath,
        Tier::Tier05,
        Tier::AllowRule,
        Tier::Mode,
        Tier::Guardian,
        Tier::DontAsk,
        Tier::PlanBlock,
        Tier::Human,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::DenyRule => "deny_rule",
            Tier::AskRule => "ask_rule",
            Tier::CriticalPath => "critical_path",
            Tier::ProtectedPath => "protected_path",
            Tier::Hook => "hook",
            Tier::ReadOnlyFastPath => "read_only_fast_path",
            Tier::Tier05 => "tier05",
            Tier::AllowRule => "allow_rule",
            Tier::Mode => "mode",
            Tier::Guardian => "guardian",
            Tier::DontAsk => "dont_ask",
            Tier::PlanBlock => "plan_block",
            Tier::Human => "human",
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
