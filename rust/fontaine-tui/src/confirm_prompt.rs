//! The y/n confirmations that share one shape: a heading, some wrapped text and a one-line
//! footer. Each kind lives in its own module as a constructor that supplies only its words.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::theme::Theme;

/// Keys typed ahead must not answer a prompt that appears under them.
pub(crate) const ANSWER_GRACE: Duration = Duration::from_millis(400);

pub enum Block {
    Text(String),
    Bold(String),
}

pub struct ConfirmPrompt {
    /// Set by the app; tells two consecutive prompts apart.
    pub(crate) id: u64,
    opened: Instant,
    heading: String,
    blocks: Vec<Block>,
    footer: &'static str,
    /// Rows shown before the rest is replaced by a pointer to the transcript.
    max_rows: Option<usize>,
}

impl ConfirmPrompt {
    pub fn new(heading: String, blocks: Vec<Block>, footer: &'static str) -> ConfirmPrompt {
        ConfirmPrompt {
            id: 0,
            opened: Instant::now(),
            heading,
            blocks,
            footer,
            max_rows: None,
        }
    }

    pub fn with_max_rows(mut self, rows: usize) -> ConfirmPrompt {
        self.max_rows = Some(rows);
        self
    }

    /// `Some(true)` is a yes, `Some(false)` a no.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<bool> {
        self.handle_key_at(key, Instant::now())
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<bool> {
        if key.kind != KeyEventKind::Press
            || now.saturating_duration_since(self.opened) < ANSWER_GRACE
            || key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return None;
        }
        match key.code {
            KeyCode::Char('y') => Some(true),
            KeyCode::Char('n') | KeyCode::Esc => Some(false),
            _ => None,
        }
    }

    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut out = vec![Line::styled(self.heading.clone(), bold.fg(theme.heading))];
        let mut shown = 0;
        let mut hidden = 0;
        for block in &self.blocks {
            let (text, style) = match block {
                Block::Text(text) => (text, Style::default()),
                Block::Bold(text) => (text, bold),
            };
            for piece in wrap(text, width) {
                if self.max_rows.is_none_or(|max| shown < max) {
                    out.push(Line::styled(piece, style));
                    shown += 1;
                } else {
                    hidden += 1;
                }
            }
        }
        if hidden > 0 {
            out.push(Line::styled(
                format!("… {hidden} more lines; the whole list is in the transcript above"),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![Line::styled(self.footer, dim)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: Vec<Line<'static>>) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    fn theme() -> Theme {
        Theme::preset(Default::default())
    }

    #[test]
    fn only_y_says_yes_and_not_before_the_grace_period() {
        let mut prompt = ConfirmPrompt::new("h".into(), Vec::new(), "f");
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let opened = prompt.opened;
        assert_eq!(prompt.handle_key_at(y, opened), None);
        let later = opened + ANSWER_GRACE;
        assert_eq!(prompt.handle_key_at(y, later), Some(true));
        for no in [KeyCode::Char('n'), KeyCode::Esc] {
            let key = KeyEvent::new(no, KeyModifiers::NONE);
            assert_eq!(prompt.handle_key_at(key, later), Some(false));
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(enter, later), None);
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(prompt.handle_key_at(ctrl_y, later), None);
    }

    #[test]
    fn text_is_plain_and_bold_is_bold_after_the_heading() {
        let prompt = ConfirmPrompt::new(
            "Heading".into(),
            vec![
                Block::Text("plain".into()),
                Block::Text(String::new()),
                Block::Bold("strong".into()),
            ],
            "footer",
        );
        let body = prompt.body(80, &theme());
        assert_eq!(rows(body.clone()), ["Heading", "plain", "", "strong"]);
        assert!(body[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!body[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(body[3].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(rows(prompt.footer()), ["footer"]);
    }

    #[test]
    fn a_long_review_is_cut_with_a_pointer_to_the_transcript() {
        let blocks = (0..40)
            .map(|i| Block::Text(format!("  - entry {i}")))
            .collect();
        let prompt = ConfirmPrompt::new("h".into(), blocks, "f").with_max_rows(14);
        let body = prompt.body(80, &theme());
        assert_eq!(body.len(), 1 + 14 + 1);
        assert!(body.last().unwrap().to_string().contains("26 more lines"));
    }

    #[test]
    fn the_constructors_say_their_own_words() {
        let t = theme();
        let server = crate::server_prompt::new("gh", "gh-mcp stdio");
        let body = rows(server.body(300, &t));
        assert_eq!(body[0], "Start the MCP server `gh`?");
        assert!(body[1].starts_with("This repository defines it."));
        assert_eq!(body[2], "gh-mcp stdio");
        assert_eq!(
            rows(server.footer()),
            ["y starts it and remembers the answer for this definition · n leaves it off"]
        );

        let guardian = crate::guardian_prompt::new();
        let body = rows(guardian.body(300, &t));
        assert_eq!(body[0], "Turn the guardian off for this session?");
        assert!(body[1].ends_with("This is a development escape hatch."));
        assert_eq!(rows(guardian.footer()), ["y turns it off · n leaves it on"]);

        let setup = crate::setup_prompt::new("a\nb".into(), 2);
        assert_eq!(
            rows(setup.body(80, &t)),
            ["Save these 2 proposals from the repository scan?", "a", "b"]
        );
        assert_eq!(rows(setup.footer()), ["y saves them · n discards them"]);

        let skill = fontaine_core::extensions::Skill {
            name: "p:pr".into(),
            description: "d".into(),
            body: String::new(),
            argument_hint: None,
            user_invocable: true,
            disable_model_invocation: false,
            allowed_tools: Vec::new(),
            declared_tools: Vec::new(),
            digest: "sha256:new".into(),
            scope: fontaine_core::extensions::Scope::User,
            path: "/c/pr/SKILL.md".into(),
        };
        let trust = crate::skill_trust_prompt::new(&skill, None);
        let body = rows(trust.body(300, &t));
        assert_eq!(body[0], "Honour the allowed-tools of p:pr?");
        assert_eq!(body[4], "");
        assert_eq!(rows(trust.footer()), ["y trusts it · n leaves it as it is"]);
    }
}
