//! The confirmation before `/guardian-disable`: the guardian and the deterministic floor both
//! come off for the rest of the session, so Fontaine asks rather than toggling on one key.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::server_prompt::ANSWER_GRACE;
use crate::theme::Theme;

pub struct GuardianOffPrompt {
    /// Set by the app; tells two consecutive prompts apart.
    pub(crate) id: u64,
    opened: Instant,
}

impl Default for GuardianOffPrompt {
    fn default() -> Self {
        Self::new()
    }
}

impl GuardianOffPrompt {
    pub fn new() -> GuardianOffPrompt {
        GuardianOffPrompt {
            id: 0,
            opened: Instant::now(),
        }
    }

    /// `Some(true)` turns the guardian off, `Some(false)` leaves it on.
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
            "Turn the guardian off for this session?",
            bold.fg(theme.heading),
        )];
        for piece in wrap(
            "Every tool call will run without the guardian, the permission rules and the protected paths until you type /guardian-enable. This is a development escape hatch.",
            width,
        ) {
            out.push(Line::raw(piece));
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![Line::styled("y turns it off · n leaves it on", dim)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn only_y_turns_it_off_and_not_before_the_grace_period() {
        let mut prompt = GuardianOffPrompt::new();
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let opened = prompt.opened;
        assert_eq!(prompt.handle_key_at(y, opened), None);
        let later = opened + Duration::from_millis(450);
        assert_eq!(prompt.handle_key_at(y, later), Some(true));
        let n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(n, later), Some(false));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(enter, later), None);
    }
}
