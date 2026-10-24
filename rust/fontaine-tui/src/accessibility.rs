//! Whether screen-reader mode is on, and why.

/// The environment variable that turns the mode on (`1`) or keeps it off (`0`).
pub const ENV_VAR: &str = "FONTAINE_AX_SCREEN_READER";

/// What switched screen-reader mode on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    Settings,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Flag => "flag",
            Source::Env => "env",
            Source::Settings => "settings",
        }
    }

    /// The line printed once at startup, before any terminal mode changes.
    pub fn announcement(self) -> String {
        format!("[Screen Reader Mode: on via {}]", self.as_str())
    }
}

/// The flag turns it on. Otherwise the environment decides when it is `1` or `0`, and the
/// setting decides when the environment has no say.
pub fn resolve(flag: bool, env: Option<&str>, setting: Option<bool>) -> Option<Source> {
    if flag {
        return Some(Source::Flag);
    }
    match env {
        Some("1") => return Some(Source::Env),
        Some("0") => return None,
        _ => {}
    }
    setting.unwrap_or(false).then_some(Source::Settings)
}

/// Whether the environment value is one `resolve` acts on. An empty value counts as unset.
pub fn env_is_valid(value: &str) -> bool {
    matches!(value, "" | "0" | "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_wins_over_everything() {
        assert_eq!(resolve(true, None, None), Some(Source::Flag));
        assert_eq!(resolve(true, Some("0"), Some(false)), Some(Source::Flag));
    }

    #[test]
    fn the_environment_turns_it_on_or_keeps_it_off() {
        assert_eq!(resolve(false, Some("1"), None), Some(Source::Env));
        assert_eq!(resolve(false, Some("1"), Some(false)), Some(Source::Env));
        assert_eq!(resolve(false, Some("0"), Some(true)), None);
        assert_eq!(resolve(false, Some("0"), None), None);
    }

    #[test]
    fn the_setting_applies_when_the_environment_is_silent() {
        assert_eq!(resolve(false, None, Some(true)), Some(Source::Settings));
        assert_eq!(resolve(false, None, Some(false)), None);
        assert_eq!(resolve(false, None, None), None);
    }

    #[test]
    fn an_invalid_environment_value_is_ignored() {
        assert_eq!(
            resolve(false, Some("yes"), Some(true)),
            Some(Source::Settings)
        );
        assert_eq!(resolve(false, Some("yes"), None), None);
        assert!(!env_is_valid("yes"));
        assert!(env_is_valid("1") && env_is_valid("0") && env_is_valid(""));
    }

    #[test]
    fn the_announcement_names_the_source() {
        assert_eq!(
            Source::Env.announcement(),
            "[Screen Reader Mode: on via env]"
        );
    }
}
