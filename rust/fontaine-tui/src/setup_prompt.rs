//! The prompts of `/auto-mode-setup`. The review comes before it saves what the repository scan
//! proposed. The proposals come from a model reading the repository, and some of them relax the
//! guardian, so nothing is saved until the user has read them and answered. The composition offer
//! prompt asks whether to write a detected local screen model into the guardian composition.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::server_prompt::ANSWER_GRACE;
use crate::theme::Theme;
use fontaine_core::composition_offer::{Candidate, Offer, Posture};

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Posture,
    Account,
    Confirm,
}

/// The offer to write the guardian composition's allow screen: safe or fast, then the account
/// when several hold the benchmarked model, then a confirmation of exactly what will be written.
/// Anything but a yes at the end writes nothing.
pub struct CompositionPrompt {
    pub(crate) id: u64,
    opened: Instant,
    candidates: Vec<Candidate>,
    stage: Stage,
    posture: Option<Posture>,
    account: usize,
}

impl CompositionPrompt {
    pub fn new(candidates: Vec<Candidate>) -> CompositionPrompt {
        CompositionPrompt {
            id: 0,
            opened: Instant::now(),
            candidates,
            stage: Stage::Posture,
            posture: None,
            account: 0,
        }
    }

    fn offer(&self) -> Offer {
        let candidate = &self.candidates[self.account];
        Offer {
            account: candidate.account.clone(),
            benchmark: candidate.benchmark,
            posture: self
                .posture
                .expect("the posture is chosen before the confirmation"),
        }
    }

    /// `None` while the prompt is open; `Some(None)` declines; `Some(Some(offer))` is the
    /// confirmed offer.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Option<Offer>> {
        self.handle_key_at(key, Instant::now())
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<Option<Offer>> {
        if key.kind != KeyEventKind::Press
            || now.saturating_duration_since(self.opened) < ANSWER_GRACE
            || key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return None;
        }
        if key.code == KeyCode::Esc {
            return Some(None);
        }
        match (self.stage, key.code) {
            (Stage::Posture, KeyCode::Char(c @ ('1' | '2'))) => {
                self.posture = Some(if c == '1' {
                    Posture::Safe
                } else {
                    Posture::Fast
                });
                self.stage = if self.candidates.len() > 1 {
                    Stage::Account
                } else {
                    Stage::Confirm
                };
                None
            }
            (Stage::Account, KeyCode::Char(c)) => {
                let n = c.to_digit(10)? as usize;
                if (1..=self.candidates.len()).contains(&n) {
                    self.account = n - 1;
                    self.stage = Stage::Confirm;
                }
                None
            }
            (Stage::Confirm, KeyCode::Char('y')) => Some(Some(self.offer())),
            (Stage::Confirm, KeyCode::Char('n')) => Some(None),
            _ => None,
        }
    }

    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let heading = |text: &str| Line::styled(text.to_string(), bold.fg(theme.heading));
        let mut text = Vec::new();
        let title = match self.stage {
            Stage::Posture => {
                text.push(format!("1. {}", Posture::Safe.describe()));
                text.push(format!("2. {}", Posture::Fast.describe()));
                "Set up the guardian's allow screen?"
            }
            Stage::Account => {
                for (i, c) in self.candidates.iter().enumerate() {
                    text.push(format!("{}. {}/{}", i + 1, c.account, c.benchmark.tag));
                }
                "Which account runs the screen?"
            }
            Stage::Confirm => {
                text.push("Write to your user settings:".to_string());
                text.extend(
                    self.offer()
                        .proposal()
                        .into_iter()
                        .map(|l| format!("  {l}")),
                );
                "Write this composition?"
            }
        };
        let mut out = vec![heading(title)];
        for line in &text {
            out.extend(wrap(line, width).into_iter().map(Line::raw));
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let text = match self.stage {
            Stage::Posture => "1 or 2 chooses · Esc skips",
            Stage::Account => "the number chooses · Esc skips",
            Stage::Confirm => "y writes it · n skips",
        };
        vec![Line::styled(text, dim)]
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

    fn candidate(account: &str) -> Candidate {
        Candidate {
            account: account.to_string(),
            benchmark: &fontaine_core::composition_offer::BENCHMARKS[0],
        }
    }

    fn press(prompt: &mut CompositionPrompt, c: char) -> Option<Option<Offer>> {
        let later = prompt.opened + Duration::from_millis(450);
        prompt.handle_key_at(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), later)
    }

    #[test]
    fn the_composition_offer_is_written_only_after_a_yes() {
        let mut prompt = CompositionPrompt::new(vec![candidate("local")]);
        assert!(press(&mut prompt, '2').is_none());
        assert!(press(&mut prompt, 'x').is_none());
        let offer = press(&mut prompt, 'y').unwrap().unwrap();
        assert_eq!(offer.account, "local");
        assert_eq!(offer.posture, Posture::Fast);
    }

    #[test]
    fn several_accounts_are_asked_which_and_a_decline_writes_nothing() {
        let mut prompt = CompositionPrompt::new(vec![candidate("a"), candidate("b")]);
        assert!(press(&mut prompt, '1').is_none());
        assert!(press(&mut prompt, '3').is_none());
        assert!(press(&mut prompt, '2').is_none());
        let offer = press(&mut prompt, 'y').unwrap().unwrap();
        assert_eq!(
            (offer.account.as_str(), offer.posture),
            ("b", Posture::Safe)
        );

        for decline in ['n', '\u{1b}'] {
            let mut prompt = CompositionPrompt::new(vec![candidate("a")]);
            press(&mut prompt, '1');
            let key = if decline == 'n' {
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)
            } else {
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
            };
            let later = prompt.opened + Duration::from_millis(450);
            assert_eq!(
                prompt.handle_key_at(key, later).map(|o| o.is_none()),
                Some(true)
            );
        }
        let mut early = CompositionPrompt::new(vec![candidate("a")]);
        let y = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);
        assert!(early.handle_key_at(y, early.opened).is_none());
        assert_eq!(early.stage, Stage::Posture);
    }
}
