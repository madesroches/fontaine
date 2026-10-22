//! The two lists the guardian puts in front of the human: the calls it denied, which the human
//! can approve for one retry, and the choice to make when the pinned guardian model cannot be
//! used at all.

use std::time::{Duration, Instant};

use fontaine_core::gate::DeniedCall;
use fontaine_core::models::ModelRef;
use fontaine_core::permissions::{GuardianUnavailable, UnavailableChoice};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use tokio::sync::oneshot;

use crate::input::wrap;

/// Keys typed ahead during the turn must not answer a prompt that appears under them.
const ANSWER_GRACE: Duration = Duration::from_millis(400);

/// What the human did with the denied list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeniedKey {
    None,
    Close,
    /// Approve entry `n` once, as `/denied` lists it.
    Retry(usize),
}

/// `/denied`: every call the guardian denied this session, oldest first.
pub struct DeniedList {
    entries: Vec<DeniedCall>,
    selected: usize,
}

impl DeniedList {
    pub fn new(entries: Vec<DeniedCall>) -> DeniedList {
        let selected = entries.len().saturating_sub(1);
        DeniedList { entries, selected }
    }

    pub fn entries(&self) -> &[DeniedCall] {
        &self.entries
    }

    pub fn mark_retried(&mut self, index: usize) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.retried = true;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DeniedKey {
        if key.kind == KeyEventKind::Release {
            return DeniedKey::None;
        }
        let n = self.entries.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return DeniedKey::Close,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                self.selected = (self.selected + n - 1) % n;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab if n > 0 => {
                self.selected = (self.selected + 1) % n;
            }
            KeyCode::Char('r')
                if n > 0
                    && !self.entries[self.selected].retried
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                return DeniedKey::Retry(self.selected);
            }
            _ => {}
        }
        DeniedKey::None
    }

    pub fn body(&self, width: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        if self.entries.is_empty() {
            return vec![Line::styled(
                "The guardian has denied nothing this session.",
                dim,
            )];
        }
        let mut out = vec![Line::styled(
            "Calls the guardian denied",
            bold.fg(Color::Yellow),
        )];
        for (i, entry) in self.entries.iter().enumerate() {
            let marker = if i == self.selected { ">" } else { " " };
            let state = if entry.retried {
                " (approved once)"
            } else {
                ""
            };
            let head = format!("{marker} {}. {} [{}]{state}", i + 1, entry.tool, entry.rule);
            let style = if i == self.selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            for piece in wrap(&head, width) {
                out.push(Line::styled(piece, style));
            }
            for piece in wrap(&format!("    {}", entry.summary), width) {
                out.push(Line::raw(piece));
            }
            for piece in wrap(&format!("    {}", entry.explanation), width) {
                out.push(Line::styled(piece, dim));
            }
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let hint = if self.entries.is_empty() {
            "Esc closes"
        } else {
            "up/down select · r approves it once so the agent may retry · Esc closes"
        };
        vec![Line::styled(hint, dim)]
    }
}

/// What the human picked at the unavailable-guardian prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnavailableAnswer {
    /// Try the pinned model again.
    Retry,
    /// Use this guardian model, `<account>/<model>`, for the session.
    Pick(String),
    /// Switch the session to `manual`.
    Manual,
    Deny,
}

/// The pinned guardian cannot be used: retry, pick another, or leave `auto`.
pub struct UnavailablePrompt {
    pub info: GuardianUnavailable,
    candidates: Vec<ModelRef>,
    picking: bool,
    selected: usize,
    reply: Option<oneshot::Sender<UnavailableChoice>>,
    opened: Instant,
}

impl UnavailablePrompt {
    pub fn new(
        info: GuardianUnavailable,
        candidates: Vec<ModelRef>,
        reply: oneshot::Sender<UnavailableChoice>,
    ) -> UnavailablePrompt {
        UnavailablePrompt {
            info,
            candidates,
            picking: false,
            selected: 0,
            reply: Some(reply),
            opened: Instant::now(),
        }
    }

    pub fn is_picking(&self) -> bool {
        self.picking
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UnavailableAnswer> {
        self.handle_key_at(key, Instant::now())
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<UnavailableAnswer> {
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
            KeyCode::Char('r') => Some(UnavailableAnswer::Retry),
            KeyCode::Char('p') if !self.candidates.is_empty() => {
                self.picking = true;
                None
            }
            KeyCode::Char('m') => Some(UnavailableAnswer::Manual),
            KeyCode::Char('n') | KeyCode::Esc => Some(UnavailableAnswer::Deny),
            _ => None,
        }
    }

    fn pick_key(&mut self, key: KeyEvent) -> Option<UnavailableAnswer> {
        let n = self.candidates.len();
        match key.code {
            KeyCode::Esc => self.picking = false,
            KeyCode::Up | KeyCode::Char('k') => self.selected = (self.selected + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = (self.selected + 1) % n;
            }
            KeyCode::Enter => {
                return Some(UnavailableAnswer::Pick(
                    self.candidates[self.selected].to_string(),
                ));
            }
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < n {
                    return Some(UnavailableAnswer::Pick(self.candidates[index].to_string()));
                }
            }
            _ => {}
        }
        None
    }

    /// Hands the choice to the waiting pipeline.
    pub fn reply(&mut self, choice: UnavailableChoice) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(choice);
        }
    }

    pub fn body(&self, width: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut out = vec![Line::styled(
            format!("The guardian {} cannot be used", self.info.model),
            bold.fg(Color::Red),
        )];
        for piece in wrap(&self.info.reason, width) {
            out.push(Line::raw(piece));
        }
        out.push(Line::raw(format!(
            "It was needed for a `{}` call.",
            self.info.tool
        )));
        if self.picking {
            out.push(Line::styled("Pick another guardian model", bold));
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
        let mut out = vec![Line::raw("[r] retry the pinned guardian")];
        if !self.candidates.is_empty() {
            out.push(Line::raw("[p] pick another validated guardian"));
        }
        out.push(Line::raw("[m] switch this session to manual"));
        out.push(Line::raw("[n] deny the call"));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn prompt(candidates: &[&str]) -> (UnavailablePrompt, oneshot::Receiver<UnavailableChoice>) {
        let (tx, rx) = oneshot::channel();
        let candidates = candidates
            .iter()
            .map(|c| ModelRef::parse(c).unwrap())
            .collect();
        let info = GuardianUnavailable {
            model: "judge/m".into(),
            reason: "no credential".into(),
            tool: "shell".into(),
        };
        (UnavailablePrompt::new(info, candidates, tx), rx)
    }

    #[test]
    fn keys_inside_the_grace_period_answer_nothing() {
        let (mut p, _rx) = prompt(&[]);
        let opened = p.opened;
        assert_eq!(p.handle_key_at(key('m'), opened), None);
        assert_eq!(
            p.handle_key_at(key('m'), opened + ANSWER_GRACE),
            Some(UnavailableAnswer::Manual)
        );
    }

    #[test]
    fn picking_needs_candidates_and_returns_the_chosen_model() {
        let (mut none, _rx) = prompt(&[]);
        let later = none.opened + ANSWER_GRACE;
        assert_eq!(none.handle_key_at(key('p'), later), None);
        assert!(!none.is_picking());

        let (mut p, _rx) = prompt(&["a/one", "b/two"]);
        let later = p.opened + ANSWER_GRACE;
        assert_eq!(p.handle_key_at(key('p'), later), None);
        assert!(p.is_picking());
        assert_eq!(
            p.handle_key(key('2')),
            Some(UnavailableAnswer::Pick("b/two".into()))
        );
        assert_eq!(p.handle_key(key('0')), None);
    }

    #[test]
    fn the_denied_list_selects_and_retries_the_newest_first() {
        let call = |n: usize| DeniedCall {
            tool: "shell".into(),
            agent: "root".into(),
            tool_use_id: None,
            args: serde_json::json!({}),
            args_digest: format!("d{n}"),
            summary: format!("call {n}"),
            rule: "Git Destructive".into(),
            explanation: "no".into(),
            retried: false,
        };
        let mut list = DeniedList::new(vec![call(0), call(1)]);
        assert_eq!(list.handle_key(key('r')), DeniedKey::Retry(1));
        list.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(list.handle_key(key('r')), DeniedKey::Retry(0));
        list.mark_retried(0);
        assert_eq!(list.handle_key(key('r')), DeniedKey::None);
        assert_eq!(list.handle_key(key('q')), DeniedKey::Close);
        let empty = DeniedList::new(vec![]);
        assert!(empty.body(40)[0].to_string().contains("nothing"));
    }
}
