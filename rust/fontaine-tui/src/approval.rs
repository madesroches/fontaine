//! The approval prompt: what the pipeline asked, the answers it allows, and the exact rule the
//! lasting answers would add.

use std::cell::Cell;
use std::time::{Duration, Instant};

use fontaine_core::permissions::{Answer, ApprovalRequest, Offers};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use crate::diff::diff_rows;
use crate::input::{InputBuffer, wrap};
use crate::theme::Theme;

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

pub(crate) const FEEDBACK_PREFIX: &str = "Reason (optional, Enter to deny, Esc to go back): ";

pub struct Approval {
    pub request: ApprovalRequest,
    /// Tells this prompt from any other, even one for an identical call. The app assigns it.
    pub id: u64,
    reply: Option<oneshot::Sender<Answer>>,
    choices: Vec<Choice>,
    selected: usize,
    /// Set while the human types a reason for a denial.
    feedback: Option<InputBuffer>,
    /// First visible row of the call text when it is taller than the box.
    scroll: usize,
    /// How far the call text can scroll, as last drawn.
    max_scroll: Cell<usize>,
    opened: Instant,
}

const SCROLL_STEP: usize = 5;

/// Keys typed ahead during the turn must not answer a prompt that appears under them.
const ANSWER_GRACE: Duration = Duration::from_millis(400);

impl Approval {
    pub fn new(request: ApprovalRequest, reply: oneshot::Sender<Answer>) -> Approval {
        Approval {
            choices: choices(request.offers),
            request,
            id: 0,
            reply: Some(reply),
            selected: 0,
            feedback: None,
            scroll: 0,
            max_scroll: Cell::new(0),
            opened: Instant::now(),
        }
    }

    /// Whether nobody waits for the answer any more, such as when the agent was killed.
    pub fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(oneshot::Sender::is_closed)
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

    /// The deny-reason field while it is open: its prompt and what is typed.
    pub fn field(&self) -> Option<(&'static str, String)> {
        self.feedback.as_ref().map(|f| (FEEDBACK_PREFIX, f.text()))
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
        self.handle_key_at(key, Instant::now())
    }

    /// `handle_key` with the clock supplied.
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<Answer> {
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
        // Navigation and scrolling work at once; only the keys that answer wait.
        let answers = matches!(key.code, KeyCode::Enter | KeyCode::Char(_));
        if answers
            && (key.kind == KeyEventKind::Repeat
                || now.saturating_duration_since(self.opened) < ANSWER_GRACE)
        {
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
    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let who = match self.request.agent.as_str() {
            "" | fontaine_tools::gate::ROOT_AGENT => String::new(),
            agent => format!(" ({agent})"),
        };
        let mut out = vec![Line::styled(
            format!("Allow {}{who}?", self.request.tool),
            bold.fg(theme.heading),
        )];
        let wrapped = |text: &str, out: &mut Vec<Line<'static>>| {
            for line in text.lines() {
                for piece in wrap(line, width) {
                    out.push(Line::raw(piece));
                }
            }
        };
        wrapped(&self.request.summary, &mut out);
        if let Some(preview) = &self.request.preview {
            diff_rows(preview, width, theme, None, " (not shown)", &mut out);
        }
        wrapped(&self.request.reason, &mut out);
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
            for piece in wrap(&format!("{what}: {}", display_rule(rule)), width) {
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

/// `wrap` chunks by width and ratatui drops control characters, so a rule's newlines and tabs
/// would run its text together; the saved rule keeps them, so they are shown escaped instead.
fn display_rule(rule: &str) -> String {
    let mut out = String::with_capacity(rule.len());
    let mut chars = rule.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::preview::{DiffKind, DiffPreview};

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool: "shell".into(),
            agent: String::new(),
            tool_use_id: None,
            args: serde_json::Value::Null,
            summary: "ls".into(),
            tier: fontaine_core::tier::Tier::Mode,
            reason: "r".into(),
            offers: Offers::default(),
            proposed_rule: None,
            preview: None,
        }
    }

    #[test]
    fn keys_inside_the_grace_period_do_not_answer() {
        let (tx, _rx) = oneshot::channel();
        let mut approval = Approval::new(request(), tx);
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let opened = approval.opened;
        assert_eq!(approval.handle_key_at(y, opened), None);
        assert_eq!(approval.handle_key_at(enter, opened), None);
        let later = opened + ANSWER_GRACE;
        let mut repeat = y;
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(approval.handle_key_at(repeat, later), None);
        assert!(matches!(
            approval.handle_key_at(y, later),
            Some(Answer::AllowOnce)
        ));
    }

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

    fn previewed(text: &str, hidden: usize) -> Approval {
        use fontaine_core::preview::DiffLine;
        let mut req = request();
        req.tool = "edit".into();
        req.preview = Some(DiffPreview {
            path: "a.rs".into(),
            added: 1,
            removed: 1,
            lines: vec![
                DiffLine {
                    kind: DiffKind::Removed,
                    old_no: Some(9),
                    new_no: None,
                    text: "gone".into(),
                },
                DiffLine {
                    kind: DiffKind::Added,
                    old_no: None,
                    new_no: Some(10),
                    text: text.into(),
                },
            ],
            hidden,
        });
        Approval::new(req, oneshot::channel().0)
    }

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn body_puts_the_diff_between_the_summary_and_the_reason() {
        let rows = plain(&previewed("fresh", 0).body(40, &Theme::default()));
        assert_eq!(
            rows,
            [
                "Allow edit?",
                "ls",
                "a.rs  +1 -1",
                " 9 -gone",
                "10 +fresh",
                "r"
            ]
        );
    }

    #[test]
    fn wrapped_diff_text_continues_under_a_blank_gutter() {
        // A two-digit gutter, a space and the sign leave 7 of 11 columns for text.
        let rows = plain(&previewed("abcdefghijklmnop", 0).body(11, &Theme::default()));
        assert_eq!(&rows[4..7], ["10 +abcdefg", "    hijklmn", "    op"]);
    }

    #[test]
    fn hidden_lines_are_announced() {
        let rows = plain(&previewed("x", 7).body(60, &Theme::default()));
        assert!(rows.contains(&"… 7 more changed lines (not shown)".to_string()));
    }

    #[test]
    fn a_request_without_a_preview_has_no_diff_rows() {
        let approval = Approval::new(request(), oneshot::channel().0);
        assert_eq!(
            plain(&approval.body(40, &Theme::default())),
            ["Allow shell?", "ls", "r"]
        );
    }

    #[test]
    fn display_rule_shows_newlines_as_markers() {
        assert_eq!(display_rule("a\nb"), "a\\nb");
        assert_eq!(display_rule("a\r\nb"), "a\\nb");
        assert_eq!(display_rule("a\rb"), "a\\rb");
        assert_eq!(display_rule("shell(ls -la)"), "shell(ls -la)");
    }

    #[test]
    fn display_rule_escapes_tabs_and_other_control_characters() {
        assert_eq!(display_rule("cut -d'\t' -f2"), "cut -d'\\t' -f2");
        assert_eq!(display_rule("a\x1bb\x7f"), "a\\u{1b}b\\u{7f}");
    }

    #[test]
    fn the_footer_rule_keeps_both_lines_of_a_multiline_command() {
        let mut req = request();
        req.proposed_rule = Some("shell(import median\nprint(median([1]))".into());
        let approval = Approval::new(req, oneshot::channel().0);
        let rows = plain(&approval.footer(200)).join("\n");
        let at = |needle: &str| rows.find(needle).expect(needle);
        assert!(at("import median") < at("\\n"));
        assert!(at("\\n") < at("print(median"));
    }
}
