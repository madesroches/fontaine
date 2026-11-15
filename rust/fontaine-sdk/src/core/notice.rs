//! A startup message for the user, with enough structure for a surface to style it: how serious
//! it is, a short headline, and what to do about it.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub severity: Severity,
    pub headline: String,
    pub hint: Option<String>,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Self {
        Notice {
            severity: Severity::Info,
            headline: text.into(),
            hint: None,
        }
    }

    pub fn warning(headline: impl Into<String>, hint: Option<String>) -> Self {
        Notice {
            severity: Severity::Warning,
            headline: headline.into(),
            hint,
        }
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.headline.contains(needle) || self.hint.as_deref().is_some_and(|h| h.contains(needle))
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.hint {
            Some(hint) => write!(f, "{} — {hint}", self.headline),
            None => f.write_str(&self.headline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_joins_headline_and_hint() {
        let n = Notice::warning("head", Some("do this".into()));
        assert_eq!(n.to_string(), "head — do this");
        assert_eq!(n.severity, Severity::Warning);
        assert!(n.contains("do this") && n.contains("head") && !n.contains("zzz"));
    }

    #[test]
    fn info_has_no_hint() {
        let n = Notice::info("just so");
        assert_eq!(n.severity, Severity::Info);
        assert_eq!(n.to_string(), "just so");
        assert_eq!(n.hint, None);
    }
}
