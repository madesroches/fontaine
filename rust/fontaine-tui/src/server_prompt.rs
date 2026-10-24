//! The question about a repository-defined MCP server: it starts only when the user says so,
//! and the repository cannot say so for them.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::theme::Theme;

/// Keys typed ahead must not answer a prompt that appears under them.
pub(crate) const ANSWER_GRACE: Duration = Duration::from_millis(400);

pub struct ServerPrompt {
    pub name: String,
    /// The command line or URL the repository gave it.
    pub description: String,
    opened: Instant,
}

impl ServerPrompt {
    pub fn new(name: String, description: String) -> ServerPrompt {
        ServerPrompt {
            name,
            description,
            opened: Instant::now(),
        }
    }

    /// `Some(true)` starts the server, `Some(false)` leaves it off.
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
        let mut out = vec![Line::styled(
            format!("Start the MCP server `{}`?", self.name),
            bold.fg(theme.heading),
        )];
        for piece in wrap(
            "This repository defines it. It runs as a program with your permissions, and its tools are always checked like any other tool that can do anything.",
            width,
        ) {
            out.push(Line::raw(piece));
        }
        for piece in wrap(&self.description, width) {
            out.push(Line::styled(piece, bold));
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![Line::styled(
            "y starts it and remembers the answer for this definition · n leaves it off",
            dim,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_y_starts_it_and_not_before_the_grace_period() {
        let mut prompt = ServerPrompt::new("gh".into(), "gh-mcp stdio".into());
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let opened = prompt.opened;
        assert_eq!(prompt.handle_key_at(y, opened), None);
        let later = opened + ANSWER_GRACE;
        assert_eq!(prompt.handle_key_at(y, later), Some(true));
        let n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(n, later), Some(false));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(enter, later), None);
    }
}
