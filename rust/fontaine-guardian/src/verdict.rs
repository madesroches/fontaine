//! The guardian's answer, and what the harness makes of it. The model's own answer is the
//! [`Verdict`]; every outcome that is not a verdict is turned into a denial by code, with a
//! label the model is never offered.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::rule_label::RuleLabel;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
    Escalate,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
            Decision::Escalate => "escalate",
        }
    }
}

/// The verdict as the model writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawVerdict {
    /// `allow`, `deny` or `escalate`.
    decision: Decision,
    /// `allow` exactly when the decision is `allow`; otherwise the best-fitting label.
    rule: RuleLabel,
    /// One to three sentences, shown to the user and the agent.
    explanation: String,
    /// How sure the guardian is, from 0 to 1.
    #[schemars(range(min = 0.0, max = 1.0))]
    confidence: f64,
}

/// A validated verdict. Deserializing one is what checks it: a decision and a label that
/// disagree, or a confidence outside 0..=1, is a malformed answer, so the extractor retries
/// it and, failing that, the call is denied as unparseable.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Verdict {
    pub decision: Decision,
    pub rule: RuleLabel,
    pub explanation: String,
    pub confidence: f64,
}

impl Verdict {
    fn from_raw(raw: RawVerdict) -> Result<Verdict, String> {
        if !(raw.confidence.is_finite() && (0.0..=1.0).contains(&raw.confidence)) {
            return Err("`confidence` must be a number from 0 to 1".to_string());
        }
        let allow_label = raw.rule == RuleLabel::Allow;
        if (raw.decision == Decision::Allow) != allow_label {
            return Err("`rule` is `allow` exactly when `decision` is `allow`".to_string());
        }
        Ok(Verdict {
            decision: raw.decision,
            rule: raw.rule,
            explanation: raw.explanation.trim().to_string(),
            confidence: raw.confidence,
        })
    }
}

impl<'de> Deserialize<'de> for Verdict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawVerdict::deserialize(deserializer)?;
        Verdict::from_raw(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for Verdict {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Verdict")
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        RawVerdict::json_schema(generator)
    }
}

/// Why an adjudication did not end in a verdict the model gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailClosed {
    /// Provider error, stream drop or connection reset.
    Error,
    Timeout,
    Unparseable,
    LowConfidence,
    /// No credential, or a pinned guardian that cannot be reached at all.
    Unavailable,
}

impl FailClosed {
    /// The `fail_closed_reason` name in the audit log and telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            FailClosed::Error => "error",
            FailClosed::Timeout => "timeout",
            FailClosed::Unparseable => "unparseable",
            FailClosed::LowConfidence => "low_confidence",
            FailClosed::Unavailable => "unavailable",
        }
    }

    pub fn label(self) -> AuditLabel {
        match self {
            FailClosed::Error | FailClosed::Unavailable => AuditLabel::GuardianUnavailable,
            FailClosed::Timeout => AuditLabel::Timeout,
            FailClosed::Unparseable => AuditLabel::Unparseable,
            FailClosed::LowConfidence => AuditLabel::LowConfidence,
        }
    }
}

/// The `rule` an adjudication carries: a label the model chose, or one the harness assigned to
/// a fail-closed denial.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AuditLabel {
    Model(RuleLabel),
    GuardianUnavailable,
    Unparseable,
    LowConfidence,
    Timeout,
}

impl AuditLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditLabel::Model(label) => label.display_name(),
            AuditLabel::GuardianUnavailable => "GuardianUnavailable",
            AuditLabel::Unparseable => "Unparseable",
            AuditLabel::LowConfidence => "LowConfidence",
            AuditLabel::Timeout => "Timeout",
        }
    }
}

impl std::fmt::Display for AuditLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> Result<Verdict, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn a_well_formed_verdict_parses() {
        let v = parse(json!({
            "decision": "deny",
            "rule": "Download+Execute",
            "explanation": " pipes a download into a shell ",
            "confidence": 0.95
        }))
        .unwrap();
        assert_eq!(v.decision, Decision::Deny);
        assert_eq!(v.rule, RuleLabel::DownloadExecute);
        assert_eq!(v.explanation, "pipes a download into a shell");
    }

    #[test]
    fn a_label_outside_the_enum_is_malformed() {
        assert!(
            parse(json!({"decision": "deny", "rule": "Rm Everything", "explanation": "x", "confidence": 0.9}))
                .is_err()
        );
    }

    #[test]
    fn the_label_and_the_decision_must_agree() {
        assert!(
            parse(json!({"decision": "allow", "rule": "Git Destructive", "explanation": "x", "confidence": 0.9}))
                .is_err()
        );
        assert!(
            parse(
                json!({"decision": "deny", "rule": "allow", "explanation": "x", "confidence": 0.9})
            )
            .is_err()
        );
        assert!(
            parse(json!({"decision": "escalate", "rule": "allow", "explanation": "x", "confidence": 0.9}))
                .is_err()
        );
    }

    #[test]
    fn a_confidence_outside_the_unit_interval_is_malformed() {
        for confidence in [json!(1.5), json!(-0.1), json!("high")] {
            assert!(
                parse(json!({"decision": "allow", "rule": "allow", "explanation": "x", "confidence": confidence}))
                    .is_err()
            );
        }
    }

    #[test]
    fn a_missing_or_extra_field_is_malformed() {
        assert!(parse(json!({"decision": "allow", "rule": "allow", "explanation": "x"})).is_err());
        assert!(
            parse(json!({"decision": "allow", "rule": "allow", "explanation": "x", "confidence": 1, "extra": 1}))
                .is_err()
        );
    }

    #[test]
    fn the_schema_offers_every_label_and_requires_every_field() {
        let schema = serde_json::to_value(schemars::schema_for!(Verdict)).unwrap();
        let text = schema.to_string();
        for label in RuleLabel::ALL {
            assert!(text.contains(label.display_name()), "{label:?}");
        }
        for field in ["decision", "rule", "explanation", "confidence"] {
            assert!(schema["required"].to_string().contains(field), "{field}");
        }
    }

    #[test]
    fn fail_closed_reasons_map_to_the_audit_labels() {
        use FailClosed::*;
        let table = [
            (Error, "error", "GuardianUnavailable"),
            (Unavailable, "unavailable", "GuardianUnavailable"),
            (Timeout, "timeout", "Timeout"),
            (Unparseable, "unparseable", "Unparseable"),
            (LowConfidence, "low_confidence", "LowConfidence"),
        ];
        for (reason, name, label) in table {
            assert_eq!(reason.as_str(), name);
            assert_eq!(reason.label().as_str(), label);
        }
    }
}
