//! The reasoning effort a user asks of the main agent and its subagents, one provider-neutral
//! setting that rig maps onto each provider's own parameter. It lives here because an agent type
//! carries one and this crate sits below the core.

use std::fmt;
use std::str::FromStr;

use rig_core::completion::{Effort as RigEffort, Reasoning};
use schemars::JsonSchema;
use serde::Deserialize;

/// `xhigh` and `minimal` exist in rig but are not offered: the levels match Claude Code's surface
/// plus `off`, and a level a model refuses is dropped at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Off,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }

    /// The valid words, for an error that names them.
    pub fn valid_set() -> String {
        Self::ALL.map(Effort::as_str).join(", ")
    }

    pub fn to_reasoning(self) -> Reasoning {
        match self {
            Effort::Off => Reasoning::Off,
            Effort::Low => Reasoning::Effort(RigEffort::Low),
            Effort::Medium => Reasoning::Effort(RigEffort::Medium),
            Effort::High => Reasoning::Effort(RigEffort::High),
            Effort::Max => Reasoning::Effort(RigEffort::Max),
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An effort word that is none of the five.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseEffortError(String);

impl fmt::Display for ParseEffortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown effort `{}`: expected one of {}",
            self.0,
            Effort::valid_set()
        )
    }
}

impl std::error::Error for ParseEffortError {}

impl FromStr for Effort {
    type Err = ParseEffortError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Effort::ALL
            .into_iter()
            .find(|e| e.as_str() == text)
            .ok_or_else(|| ParseEffortError(text.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_round_trips_through_display_and_parse() {
        for level in Effort::ALL {
            assert_eq!(level.to_string().parse::<Effort>(), Ok(level));
        }
    }

    #[test]
    fn an_unknown_word_is_refused_with_the_valid_set() {
        let e = "xhigh".parse::<Effort>().unwrap_err().to_string();
        assert!(e.contains("`xhigh`"), "{e}");
        assert!(e.contains("off, low, medium, high, max"), "{e}");
        assert!("High".parse::<Effort>().is_err());
    }

    #[test]
    fn off_is_the_reasoning_off_and_the_rest_are_efforts() {
        assert_eq!(Effort::Off.to_reasoning(), Reasoning::Off);
        assert_eq!(
            Effort::Max.to_reasoning(),
            Reasoning::Effort(RigEffort::Max)
        );
        assert_eq!(
            Effort::Medium.to_reasoning(),
            Reasoning::Effort(RigEffort::Medium)
        );
    }

    #[test]
    fn serde_reads_the_lowercase_word() {
        #[derive(Deserialize)]
        struct T {
            e: Effort,
        }
        let t: T = serde_json::from_str(r#"{"e": "low"}"#).unwrap();
        assert_eq!(t.e, Effort::Low);
        assert!(serde_json::from_str::<T>(r#"{"e": "xhigh"}"#).is_err());
    }
}
