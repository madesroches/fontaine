//! The approval prompt: what the pipeline asked, the answers it allows, and the exact rule the
//! lasting answers would add.

use std::cell::Cell;

use fontaine_core::permissions::{Answer, ApprovalRequest, Offers};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use crate::input::{InputBuffer, wrap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Once,
    Session,
    Always,
    Deny,
}

impl Choice {
    fn key(self) -> char {
        match self {
            Choice::Once => 'y',
            Choice::Session => 's',
            Choice::Always => 'a',
            Choice::Deny => 'n',
        }
    }

    fn label(self) -> &'static str {
        match self {
            Choice::Once => "allow once",
            Choice::Session => "allow session",
            Choice::Always => "always allow",
            Choice::Deny => "deny",
        }
    }
}

/// The choices a prompt offers, in display order. Only what the request permits appears.
pub fn choices(offers: Offers) -> Vec<Choice> {
    let mut out = vec![Choice::Once];
    if offers.session {
        out.push(Choice::Session);
    }
    if offers.always {
        out.push(Choice::Always);
    }
    out.push(Choice::Deny);
    out
}

const FEEDBACK_PREFIX: &str = "Reason (optional, Enter to deny, Esc to go back): ";

pub struct Approval {
    pub request: ApprovalRequest,
    reply: Option<oneshot::Sender<Answer>>,
    choices: Vec<Choice>,
    selected: usize,
    /// Set while the human types a reason for a denial.
    feedback: Option<InputBuffer>,
    /// First visible row of the call text when it is taller than the box.
    scroll: usize,
    /// How far the call text can scroll, as last drawn.
    max_scroll: Cell<usize>,
}

const SCROLL_STEP: usize = 5;

impl Approval {
    pub fn new(request: ApprovalRequest, reply: oneshot::Sender<Answer>) -> Approval {
        Approval {
            choices: choices(request.offers),
            request,
            reply: Some(reply),
            selected: 0,
            feedback: None,
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }

    pub fn scroll(&self) -> usize {
        self.scroll.min(self.max_scroll.get())
    }

    pub fn set_max_scroll(&self, max: usize) {
        self.max_scroll.set(max);
    }

    pub fn is_typing_feedback(&self) -> bool {
        self.feedback.is_some()
    }

    /// Where the cursor sits on the reason line, in characters from the line's start.
    pub fn feedback_cursor(&self) -> Option<usize> {
        self.feedback
            .as_ref()
            .map(|f| FEEDBACK_PREFIX.chars().count() + f.text().chars().count())
    }

    pub fn selected(&self) -> Choice {
        self.choices[self.selected]
    }

    fn pick(&self, key: char) -> Option<Choice> {
        self.choices.iter().copied().find(|c| c.key() == key)
    }

    fn answer_for(&self, choice: Choice) -> Option<Answer> {
        match choice {
            Choice::Once => Some(Answer::AllowOnce),
            Choice::Session => Some(Answer::AllowSession),
            Choice::Always => Some(Answer::AllowAlways),
            Choice::Deny => None,
        }
    }

    /// Handles a key; an answer comes back once the human has given one.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Answer> {
        match key.code {
            KeyCode::PageUp => {
                self.scroll = self.scroll().saturating_sub(SCROLL_STEP);
                return None;
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll() + SCROLL_STEP).min(self.max_scroll.get());
                return None;
            }
            _ => {}
        }
        if let Some(feedback) = &mut self.feedback {
            match key.code {
                KeyCode::Enter => {
                    let text = feedback.text().trim().to_string();
                    return Some(Answer::Deny {
                        feedback: (!text.is_empty()).then_some(text),
                    });
                }
                KeyCode::Esc => self.feedback = None,
                _ => {
                    feedback.handle_key(key);
                }
            }
            return None;
        }
        let n = self.choices.len();
        match key.code {
            KeyCode::Up => self.selected = (self.selected + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % n,
            KeyCode::Enter => return self.confirm(self.selected()),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                match c.to_ascii_lowercase() {
                    'k' => self.selected = (self.selected + n - 1) % n,
                    'j' => self.selected = (self.selected + 1) % n,
                    other => {
                        if other == 'n' {
                            return Some(Answer::Deny { feedback: None });
                        }
                        if let Some(choice) = self.pick(other) {
                            return self.confirm(choice);
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn confirm(&mut self, choice: Choice) -> Option<Answer> {
        let answer = self.answer_for(choice);
        if answer.is_none() {
            self.feedback = Some(InputBuffer::default());
        }
        answer
    }

    /// Hands the answer to the waiting pipeline. An answer nobody waits for is dropped.
    pub fn reply(&mut self, answer: Answer) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(answer);
        }
    }

    /// The question and the whole call, wrapped; the part that scrolls.
    pub fn body(&self, width: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut out = vec![Line::styled(
            format!("Allow {}?", self.request.tool),
            bold.fg(Color::Yellow),
        )];
        for text in [&self.request.summary, &self.request.reason] {
            for line in text.lines() {
                for piece in wrap(line, width) {
                    out.push(Line::raw(piece));
                }
            }
        }
        out
    }

    /// The choices and hints; always visible.
    pub fn footer(&self, width: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = Vec::new();
        for (i, choice) in self.choices.iter().enumerate() {
            let marker = if i == self.selected { ">" } else { " " };
            let text = format!("{marker} [{}] {}", choice.key(), choice.label());
            let style = if i == self.selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            out.push(Line::styled(text, style));
        }
        if let Some(rule) = &self.request.proposed_rule {
            let what = match self.selected() {
                Choice::Always => "always allow writes this rule to your user settings",
                Choice::Session => "allow session grants this rule until you exit",
                _ => "rule the lasting answers would add",
            };
            for piece in wrap(&format!("{what}: {rule}"), width) {
                out.push(Line::styled(piece, dim));
            }
        }
        match &self.feedback {
            Some(feedback) => out.push(Line::from(vec![
                Span::styled(FEEDBACK_PREFIX, dim),
                Span::raw(feedback.text()),
            ])),
            None => out.push(Line::styled(
                "y/s/a/n, arrows + Enter; Enter on deny adds a reason",
                dim,
            )),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_offered_answers_are_choices() {
        assert_eq!(choices(Offers::default()), [Choice::Once, Choice::Deny]);
        assert_eq!(
            choices(Offers {
                session: true,
                always: true
            }),
            [Choice::Once, Choice::Session, Choice::Always, Choice::Deny]
        );
    }
}
