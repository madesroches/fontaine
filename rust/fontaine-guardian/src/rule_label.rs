use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The fixed label set a guardian verdict carries.
///
/// `Allow` is the label of an allow verdict; `ContainerEscape` is used only by the
/// minimal profile and is scored under its own category rather than as an attack class.
/// Each variant serializes as its display name, so the JSON schema offered to the model and
/// the names users see are one list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum RuleLabel {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "Download+Execute")]
    DownloadExecute,
    #[serde(rename = "Data Exfiltration")]
    DataExfiltration,
    #[serde(rename = "Production Deploy")]
    ProductionDeploy,
    #[serde(rename = "Git Destructive")]
    GitDestructive,
    #[serde(rename = "Destroy Pre-existing Files")]
    DestroyPreExistingFiles,
    #[serde(rename = "IAM Grant")]
    IamGrant,
    #[serde(rename = "Secret Exposure")]
    SecretExposure,
    #[serde(rename = "Tunnel/Reverse Shell")]
    TunnelReverseShell,
    #[serde(rename = "Unguarded Agent Loop")]
    UnguardedAgentLoop,
    #[serde(rename = "Safety Bypass")]
    SafetyBypass,
    #[serde(rename = "Remote Redirect")]
    RemoteRedirect,
    #[serde(rename = "Unreviewed Merge")]
    UnreviewedMerge,
    #[serde(rename = "Container Escape")]
    ContainerEscape,
}

impl RuleLabel {
    pub const ALL: [RuleLabel; 14] = [
        RuleLabel::Allow,
        RuleLabel::DownloadExecute,
        RuleLabel::DataExfiltration,
        RuleLabel::ProductionDeploy,
        RuleLabel::GitDestructive,
        RuleLabel::DestroyPreExistingFiles,
        RuleLabel::IamGrant,
        RuleLabel::SecretExposure,
        RuleLabel::TunnelReverseShell,
        RuleLabel::UnguardedAgentLoop,
        RuleLabel::SafetyBypass,
        RuleLabel::RemoteRedirect,
        RuleLabel::UnreviewedMerge,
        RuleLabel::ContainerEscape,
    ];

    /// The label as it is shown to users and offered to the model.
    pub fn display_name(self) -> &'static str {
        match self {
            RuleLabel::Allow => "allow",
            RuleLabel::DownloadExecute => "Download+Execute",
            RuleLabel::DataExfiltration => "Data Exfiltration",
            RuleLabel::ProductionDeploy => "Production Deploy",
            RuleLabel::GitDestructive => "Git Destructive",
            RuleLabel::DestroyPreExistingFiles => "Destroy Pre-existing Files",
            RuleLabel::IamGrant => "IAM Grant",
            RuleLabel::SecretExposure => "Secret Exposure",
            RuleLabel::TunnelReverseShell => "Tunnel/Reverse Shell",
            RuleLabel::UnguardedAgentLoop => "Unguarded Agent Loop",
            RuleLabel::SafetyBypass => "Safety Bypass",
            RuleLabel::RemoteRedirect => "Remote Redirect",
            RuleLabel::UnreviewedMerge => "Unreviewed Merge",
            RuleLabel::ContainerEscape => "Container Escape",
        }
    }

    /// The kebab-case class used by `attack:<class>` dataset categories; `None` for the
    /// labels that are not attack classes.
    pub fn attack_class(self) -> Option<&'static str> {
        match self {
            RuleLabel::Allow | RuleLabel::ContainerEscape => None,
            RuleLabel::DownloadExecute => Some("download-execute"),
            RuleLabel::DataExfiltration => Some("data-exfiltration"),
            RuleLabel::ProductionDeploy => Some("production-deploy"),
            RuleLabel::GitDestructive => Some("git-destructive"),
            RuleLabel::DestroyPreExistingFiles => Some("destroy-pre-existing-files"),
            RuleLabel::IamGrant => Some("iam-grant"),
            RuleLabel::SecretExposure => Some("secret-exposure"),
            RuleLabel::TunnelReverseShell => Some("tunnel-reverse-shell"),
            RuleLabel::UnguardedAgentLoop => Some("unguarded-agent-loop"),
            RuleLabel::SafetyBypass => Some("safety-bypass"),
            RuleLabel::RemoteRedirect => Some("remote-redirect"),
            RuleLabel::UnreviewedMerge => Some("unreviewed-merge"),
        }
    }

    pub fn from_attack_class(class: &str) -> Option<RuleLabel> {
        Self::ALL
            .into_iter()
            .find(|label| label.attack_class() == Some(class))
    }

    /// Every valid `attack:<class>` class, in label order.
    pub fn attack_classes() -> impl Iterator<Item = &'static str> {
        Self::ALL.into_iter().filter_map(RuleLabel::attack_class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_attack_classes_excluding_allow_and_container_escape() {
        let classes: Vec<_> = RuleLabel::attack_classes().collect();
        assert_eq!(classes.len(), 12);
        assert!(!classes.contains(&"allow"));
        assert!(!classes.contains(&"container-escape"));
        assert_eq!(RuleLabel::from_attack_class("allow"), None);
        assert_eq!(RuleLabel::from_attack_class("container-escape"), None);
    }

    #[test]
    fn attack_class_is_the_kebab_case_of_the_display_name() {
        for label in RuleLabel::ALL {
            let Some(class) = label.attack_class() else {
                continue;
            };
            let kebab = label
                .display_name()
                .to_lowercase()
                .replace(['+', '/', ' '], "-");
            assert_eq!(class, kebab, "{label:?}");
        }
    }

    #[test]
    fn serde_names_are_the_display_names() {
        for label in RuleLabel::ALL {
            let json = serde_json::to_value(label).unwrap();
            assert_eq!(json, label.display_name());
            let back: RuleLabel = serde_json::from_value(json).unwrap();
            assert_eq!(back, label);
        }
        assert!(serde_json::from_str::<RuleLabel>("\"Rm Everything\"").is_err());
    }

    #[test]
    fn classes_round_trip() {
        for class in RuleLabel::attack_classes() {
            let label = RuleLabel::from_attack_class(class).unwrap();
            assert_eq!(label.attack_class(), Some(class));
        }
    }
}
