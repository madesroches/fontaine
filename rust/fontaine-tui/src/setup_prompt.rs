//! The review before `/auto-mode-setup` saves what the repository scan proposed. The proposals
//! come from a model reading the repository, and some of them relax the guardian, so nothing is
//! saved until the user has read them and answered.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::server_prompt::ANSWER_GRACE;
use crate::theme::Theme;

/// Rows of proposals the overlay shows; the transcript above it holds the whole review.
const BODY_ROWS: usize = 14;

pub struct SetupReviewPrompt {
    /// Set by the app; tells two consecutive prompts apart.
    pub(crate) id: u64,
    opened: Instant,
    review: String,
    count: usize,
}

impl SetupReviewPrompt {
    pub fn new(review: String, count: usize) -> SetupReviewPrompt {
        SetupReviewPrompt {
            id: 0,
            opened: Instant::now(),
            review,
            count,
        }
    }

    /// `Some(true)` saves the proposals, `Some(false)` discards them.
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
            format!(
                "Save these {} proposals from the repository scan?",
                self.count
            ),
            bold.fg(theme.heading),
        )];
        let mut shown = 0;
        let mut hidden = 0;
        for line in self.review.lines() {
            for piece in wrap(line, width) {
                if shown < BODY_ROWS {
                    out.push(Line::raw(piece));
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
        vec![Line::styled("y saves them · n discards them", dim)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn only_y_saves_and_not_before_the_grace_period() {
        let mut prompt = SetupReviewPrompt::new("x".into(), 1);
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let opened = prompt.opened;
        assert_eq!(prompt.handle_key_at(y, opened), None);
        let later = opened + Duration::from_millis(450);
        assert_eq!(prompt.handle_key_at(y, later), Some(true));
        for discard in [KeyCode::Char('n'), KeyCode::Esc] {
            let key = KeyEvent::new(discard, KeyModifiers::NONE);
            assert_eq!(prompt.handle_key_at(key, later), Some(false));
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(prompt.handle_key_at(enter, later), None);
    }

    #[test]
    fn a_long_review_is_cut_with_a_pointer_to_the_transcript() {
        let review = (0..40)
            .map(|i| format!("  - entry {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = SetupReviewPrompt::new(review, 40);
        let body = prompt.body(80, &Theme::preset(Default::default()));
        assert_eq!(body.len(), 1 + BODY_ROWS + 1);
        assert!(body.last().unwrap().to_string().contains("26 more lines"));
    }
}
