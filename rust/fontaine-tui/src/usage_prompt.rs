//! The prompt shown while the run is held for a spent usage window: resume now, cancel, or
//! switch the exhausted role to another account. Keys only count here, never in the message box,
//! because the prompt owns the keyboard while it is open.

use std::time::Instant;

use fontaine_core::models::ModelRef;
use fontaine_core::permissions::PauseChoice;
use fontaine_core::quota::UsageRole;
use fontaine_core::usage_pause::PauseInfo;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use tokio::sync::oneshot;

use crate::confirm_prompt::ANSWER_GRACE;
use crate::input::wrap;
use crate::theme::Theme;

/// What the human picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsagePauseAnswer {
    /// Go on now.
    Resume,
    Cancel,
    /// Use this model, `<account>/<model>`, for the exhausted role.
    Pick(String),
}

pub struct UsagePausePrompt {
    pub info: PauseInfo,
    /// The models the exhausted role could move to, other accounts' first. Empty when the
    /// pause is for an account that cannot be switched from here.
    candidates: Vec<ModelRef>,
    picking: bool,
    selected: usize,
    reply: Option<oneshot::Sender<PauseChoice>>,
    opened: Instant,
    /// Set by `App::begin_usage_pause`; tells two openings of this prompt apart.
    pub id: u64,
}

impl UsagePausePrompt {
    pub fn new(
        info: PauseInfo,
        candidates: Vec<ModelRef>,
        reply: oneshot::Sender<PauseChoice>,
    ) -> UsagePausePrompt {
        UsagePausePrompt {
            info,
            candidates,
            picking: false,
            selected: 0,
            reply: Some(reply),
            opened: Instant::now(),
            id: 0,
        }
    }

    /// Whether the pause stopped waiting for an answer, because it ended on its own.
    pub fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(|reply| reply.is_closed())
    }

    pub fn is_picking(&self) -> bool {
        self.picking
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UsagePauseAnswer> {
        self.handle_key_at(key, Instant::now())
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<UsagePauseAnswer> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if self.picking {
            return self.pick_key(key);
        }
        let answers = matches!(key.code, KeyCode::Char(_) | KeyCode::Esc);
        if answers
            && (key.kind == KeyEventKind::Repeat
                || now.saturating_duration_since(self.opened) < ANSWER_GRACE)
        {
            return None;
        }
        match key.code {
            KeyCode::Char('r') => Some(UsagePauseAnswer::Resume),
            KeyCode::Char('s') if !self.candidates.is_empty() => {
                self.picking = true;
                None
            }
            KeyCode::Char('c') | KeyCode::Esc => Some(UsagePauseAnswer::Cancel),
            _ => None,
        }
    }

    fn pick_key(&mut self, key: KeyEvent) -> Option<UsagePauseAnswer> {
        let n = self.candidates.len();
        match key.code {
            KeyCode::Esc => self.picking = false,
            KeyCode::Up | KeyCode::Char('k') => self.selected = (self.selected + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = (self.selected + 1) % n;
            }
            KeyCode::Enter => {
                return Some(UsagePauseAnswer::Pick(
                    self.candidates[self.selected].to_string(),
                ));
            }
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < n {
                    return Some(UsagePauseAnswer::Pick(self.candidates[index].to_string()));
                }
            }
            _ => {}
        }
        None
    }

    /// Hands the choice to the pause.
    pub fn reply(&mut self, choice: PauseChoice) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(choice);
        }
    }

    /// `status` is the live countdown line, which the caller computes from the clock.
    pub fn body(&self, width: usize, theme: &Theme, status: &str) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let role = match self.info.role {
            UsageRole::Main => "main",
            UsageRole::Guardian => "guardian",
        };
        let mut out = vec![Line::styled(
            format!("The {} usage window is spent", self.info.account),
            bold.fg(theme.error),
        )];
        out.push(Line::raw(format!("It serves the {role} model.")));
        for piece in wrap(status, width) {
            out.push(Line::raw(piece));
        }
        out.push(Line::raw(
            "Nothing runs while it is paused; pending calls wait and are checked again afterwards.",
        ));
        if self.picking {
            out.push(Line::styled("Switch to another model", bold));
            for (i, candidate) in self.candidates.iter().enumerate() {
                let marker = if i == self.selected { ">" } else { " " };
                let style = if i == self.selected {
                    bold.add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                out.push(Line::styled(
                    format!("{marker} {}. {candidate}", i + 1),
                    style,
                ));
            }
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        if self.picking {
            return vec![Line::styled(
                "up/down or a number, Enter picks · Esc goes back",
                dim,
            )];
        }
        let mut out = vec![Line::raw("[r] resume now")];
        if !self.candidates.is_empty() {
            out.push(Line::raw("[s] switch to another account"));
        }
        out.push(Line::raw("[c] cancel the run"));
        out
    }
}

#[cfg(test)]
mod tests {
    use fontaine_core::usage_pause::PauseInfo;
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn prompt(candidates: &[&str]) -> UsagePausePrompt {
        let (tx, _rx) = oneshot::channel();
        let candidates = candidates
            .iter()
            .map(|c| ModelRef::parse(c).unwrap())
            .collect();
        UsagePausePrompt::new(
            PauseInfo::new(UsageRole::Main, "work", None),
            candidates,
            tx,
        )
    }

    #[test]
    fn keys_inside_the_grace_period_answer_nothing() {
        let mut p = prompt(&[]);
        let opened = p.opened;
        assert_eq!(p.handle_key_at(key('c'), opened), None);
        assert_eq!(
            p.handle_key_at(key('c'), opened + ANSWER_GRACE),
            Some(UsagePauseAnswer::Cancel)
        );
    }

    #[test]
    fn r_resumes_and_esc_cancels() {
        let mut p = prompt(&[]);
        let later = p.opened + ANSWER_GRACE;
        assert_eq!(
            p.handle_key_at(key('r'), later),
            Some(UsagePauseAnswer::Resume)
        );
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(p.handle_key_at(esc, later), Some(UsagePauseAnswer::Cancel));
    }

    #[test]
    fn switching_needs_candidates_and_returns_the_chosen_model() {
        let mut none = prompt(&[]);
        let later = none.opened + ANSWER_GRACE;
        assert_eq!(none.handle_key_at(key('s'), later), None);
        assert!(!none.is_picking());

        let mut p = prompt(&["a/one", "b/two"]);
        let later = p.opened + ANSWER_GRACE;
        assert_eq!(p.handle_key_at(key('s'), later), None);
        assert!(p.is_picking());
        assert_eq!(
            p.handle_key(key('2')),
            Some(UsagePauseAnswer::Pick("b/two".into()))
        );
        assert_eq!(p.handle_key(key('0')), None);
    }

    #[test]
    fn typing_never_reaches_anything_but_the_prompt() {
        let mut p = prompt(&[]);
        let later = p.opened + ANSWER_GRACE;
        assert_eq!(p.handle_key_at(key('x'), later), None);
        assert_eq!(p.handle_key_at(key('s'), later), None);
    }
}
