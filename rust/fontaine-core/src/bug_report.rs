//! The bug report `/bug` shows and files: environment, provider, redacted configuration, recent
//! warnings, guardian verdict summaries and, when the user chooses the full level, the tail of
//! the transcript. Pure: the caller gathers the [`Facts`], and redaction runs over the final text.

use std::fmt::Write as _;

use crate::redact::Redactor;

/// The GitHub issue body limit is 65536 characters; stay well inside it.
pub const BODY_LIMIT: usize = 60_000;
/// Transcript turns the full level adds.
pub const DEFAULT_TURNS: usize = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// Environment, configuration, warnings, verdict summaries and the description.
    #[default]
    Minimal,
    /// Minimal plus the last turns of the transcript.
    Full,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Minimal => "minimal",
            Level::Full => "full",
        }
    }

    pub fn toggled(self) -> Level {
        match self {
            Level::Minimal => Level::Full,
            Level::Full => Level::Minimal,
        }
    }
}

/// What the caller knows about the session. Strings only; nothing here is read from the
/// environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub version: String,
    pub git_sha: String,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub term: String,
    pub terminal_size: String,
    pub install_method: String,
    pub provider: String,
    pub model: String,
    pub guardian_profile: String,
    pub guardian_model: String,
    pub permission_mode: String,
    /// `key = value (scope)` rows from the settings renderers.
    pub config_rows: Vec<String>,
    pub config_scopes: Vec<String>,
    pub warnings: Vec<String>,
    /// One line per guardian verdict.
    pub verdicts: Vec<String>,
    pub session_id: String,
    pub description: String,
    /// The transcript tail as Markdown; used by the full level only.
    pub transcript: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BugReport {
    facts: Facts,
    level: Level,
}

/// The report as text, redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub title: String,
    /// What goes in the issue.
    pub body: String,
    /// The body without the transcript, for a prefilled URL that is visible in browser history.
    pub url_body: String,
    /// Everything, written to the local bundle file.
    pub bundle: String,
}

impl BugReport {
    pub fn build(facts: &Facts, level: Level) -> BugReport {
        BugReport {
            facts: facts.clone(),
            level,
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn render(&self, redactor: &Redactor) -> Rendered {
        let f = &self.facts;
        let title = title_of(&f.description, redactor);
        let core = self.core_sections();
        let transcript = (self.level == Level::Full && !f.transcript.trim().is_empty())
            .then(|| transcript_block(&f.transcript));
        let bundle = match &transcript {
            Some(block) => format!("{core}\n{block}"),
            None => core.clone(),
        };
        let url_body = match &transcript {
            Some(_) => format!("{core}\n{TRANSCRIPT_IN_BUNDLE}"),
            None => core.clone(),
        };
        let body = match &transcript {
            Some(block) if core.len() + block.len() < BODY_LIMIT => format!("{core}\n{block}"),
            Some(_) => format!("{core}\n{TRANSCRIPT_IN_BUNDLE}"),
            None => core,
        };
        Rendered {
            title,
            body: redactor.redact(&body),
            url_body: redactor.redact(&url_body),
            bundle: redactor.redact(&bundle),
        }
    }

    fn core_sections(&self) -> String {
        let f = &self.facts;
        let mut out = String::new();
        let _ = writeln!(out, "## Description\n\n{}\n", f.description.trim());
        let _ = writeln!(out, "## Environment\n");
        let rows = [
            ("version", f.version.clone()),
            ("build target", f.target.clone()),
            ("os", format!("{} {}", f.os, f.arch)),
            ("terminal", format!("{} {}", f.term, f.terminal_size)),
            ("install", f.install_method.clone()),
            ("provider/model", format!("{} / {}", f.provider, f.model)),
            (
                "guardian",
                format!("{} / {}", f.guardian_profile, f.guardian_model),
            ),
            ("permission mode", f.permission_mode.clone()),
            ("session", f.session_id.clone()),
        ];
        for (name, value) in rows {
            let _ = writeln!(out, "- {name}: {}", value.trim());
        }
        let _ = writeln!(out, "\n## Configuration\n");
        if !f.config_scopes.is_empty() {
            let _ = writeln!(out, "Scopes: {}\n", f.config_scopes.join(", "));
        }
        out.push_str(&fenced(&f.config_rows.join("\n")));
        let _ = writeln!(out, "\n## Warnings\n");
        out.push_str(&list_or_none(&f.warnings));
        let _ = writeln!(out, "\n## Guardian verdicts\n");
        out.push_str(&list_or_none(&f.verdicts));
        out
    }
}

const TRANSCRIPT_IN_BUNDLE: &str =
    "_The transcript tail is in the local bundle file, not in this issue._\n";

/// Redacts before cutting: a cut through a secret would leave a prefix the redactor misses.
fn title_of(description: &str, redactor: &Redactor) -> String {
    let line = redactor.redact(description.lines().next().unwrap_or_default().trim());
    let mut title: String = line.chars().take(80).collect();
    if title.is_empty() {
        title = "Bug report".to_string();
    }
    title
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        return "none\n".to_string();
    }
    items.iter().fold(String::new(), |mut out, item| {
        let _ = writeln!(out, "- {}", item.replace('\n', " "));
        out
    })
}

/// A code fence longer than any backtick run inside, so the text cannot close it early.
fn fenced(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}\n")
}

fn transcript_block(transcript: &str) -> String {
    format!(
        "<details>\n<summary>Transcript tail</summary>\n\n{}\n</details>\n",
        fenced(transcript.trim())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Secret;

    fn facts() -> Facts {
        Facts {
            version: "0.1.0".into(),
            git_sha: "abc123".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            term: "xterm".into(),
            terminal_size: "80x24".into(),
            install_method: "cargo".into(),
            provider: "work".into(),
            model: "work/opus".into(),
            guardian_profile: "standard".into(),
            guardian_model: "g/m".into(),
            permission_mode: "manual".into(),
            config_rows: vec!["tui.theme = dark (user)".into()],
            config_scopes: vec!["user".into()],
            warnings: vec!["slow disk".into()],
            verdicts: vec!["shell: allow".into()],
            session_id: "s1".into(),
            description: "It crashes\nwhen I press x".into(),
            transcript: "## User\n\nhi".into(),
        }
    }

    fn render(level: Level, facts: &Facts) -> Rendered {
        BugReport::build(facts, level).render(&Redactor::default())
    }

    #[test]
    fn minimal_has_no_transcript_and_a_one_line_title() {
        let r = render(Level::Minimal, &facts());
        assert_eq!(r.title, "It crashes");
        for needle in [
            "It crashes",
            "0.1.0",
            "tui.theme",
            "slow disk",
            "shell: allow",
        ] {
            assert!(r.body.contains(needle), "{needle}");
        }
        assert!(!r.body.contains("Transcript tail"));
        assert!(!r.bundle.contains("Transcript tail"));
    }

    #[test]
    fn full_adds_the_transcript_in_a_details_block() {
        let r = render(Level::Full, &facts());
        assert!(r.body.contains("<details>"));
        assert!(!r.url_body.contains("## User"));
        assert!(r.url_body.contains("local bundle file"));
        assert!(r.bundle.contains("## User"));
    }

    #[test]
    fn an_oversized_transcript_stays_out_of_the_body() {
        let mut f = facts();
        f.transcript = "x".repeat(BODY_LIMIT);
        let r = render(Level::Full, &f);
        assert!(r.body.len() < BODY_LIMIT);
        assert!(r.body.contains("local bundle file"));
        assert!(r.bundle.contains(&f.transcript));
    }

    #[test]
    fn a_backtick_fence_in_the_text_cannot_close_the_block() {
        let mut f = facts();
        f.config_rows = vec!["```\nescape".into()];
        let r = render(Level::Minimal, &f);
        assert!(r.body.contains("````\n```\nescape\n````"));
    }

    #[test]
    fn a_planted_secret_survives_in_no_field() {
        let secret = "planted-secret-value-123";
        let mut f = facts();
        f.description = format!("title {secret}");
        f.config_rows.push(format!("k = {secret}"));
        f.warnings.push(format!("warn {secret}"));
        f.verdicts.push(format!("verdict {secret}"));
        f.transcript = format!("said {secret}");
        f.model = secret.into();
        let redactor = Redactor::new([Secret::new(secret)]);
        let r = BugReport::build(&f, Level::Full).render(&redactor);
        for text in [&r.title, &r.body, &r.bundle] {
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn an_empty_description_gets_a_default_title() {
        let mut f = facts();
        f.description = String::new();
        assert_eq!(render(Level::Minimal, &f).title, "Bug report");
    }

    #[test]
    fn a_secret_crossing_the_title_cut_is_still_redacted() {
        let secret = "crossing-secret-value-xyz";
        let mut f = facts();
        f.description = format!("{} {secret}", "a".repeat(70));
        let r = BugReport::build(&f, Level::Minimal).render(&Redactor::new([Secret::new(secret)]));
        assert!(!r.title.contains("crossing"), "{}", r.title);
        assert!(!r.title.contains("xyz"), "{}", r.title);
    }

    #[test]
    fn level_toggles() {
        assert_eq!(Level::Minimal.toggled(), Level::Full);
        assert_eq!(Level::Full.toggled().as_str(), "minimal");
    }
}
