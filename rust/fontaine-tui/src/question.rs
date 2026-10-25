//! The question dialog: the model's multiple-choice questions, one at a time, with an "Other" row
//! that takes the human's own words.

use std::time::Instant;

use fontaine_tools::ask_user::{Question, QuestionAnswer, QuestionReply};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use tokio::sync::oneshot;

use crate::input::{InputBuffer, wrap};
use crate::server_prompt::ANSWER_GRACE;
use crate::theme::Theme;

pub const OTHER_PROMPT: &str = "Other: ";

/// The model wrote every string here; control characters would move the cursor or recolor the
/// screen.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

pub struct QuestionPanel {
    pub agent: String,
    /// Set by the app; tells two consecutive dialogs apart.
    pub(crate) id: u64,
    questions: Vec<Question>,
    reply: Option<oneshot::Sender<QuestionReply>>,
    current: usize,
    answers: Vec<QuestionAnswer>,
    /// The highlighted row: the options, then "Other".
    selected: usize,
    checked: Vec<bool>,
    other: InputBuffer,
    typing: bool,
    opened: Instant,
}

impl QuestionPanel {
    pub fn new(
        agent: String,
        questions: Vec<Question>,
        reply: oneshot::Sender<QuestionReply>,
    ) -> QuestionPanel {
        let checked = vec![false; questions.first().map_or(0, |q| q.options.len())];
        QuestionPanel {
            agent,
            id: 0,
            questions,
            reply: Some(reply),
            current: 0,
            answers: Vec::new(),
            selected: 0,
            checked,
            other: InputBuffer::default(),
            typing: false,
            opened: Instant::now(),
        }
    }

    /// Whether nobody waits for the answer any more, such as when the turn was cut short.
    pub fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(oneshot::Sender::is_closed)
    }

    pub fn is_typing(&self) -> bool {
        self.typing
    }

    /// The "Other" field while it is open: its prompt and what is typed.
    pub fn field(&self) -> Option<(&'static str, String)> {
        self.typing.then(|| (OTHER_PROMPT, self.other.text()))
    }

    /// Hands the reply to the waiting tool call. A reply nobody waits for is dropped.
    pub fn reply(&mut self, reply: QuestionReply) {
        if let Some(sender) = self.reply.take() {
            let _ = sender.send(reply);
        }
    }

    /// Lets a test answer at once instead of waiting out the grace period.
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        self.opened = self
            .opened
            .checked_sub(ANSWER_GRACE * 2)
            .unwrap_or(self.opened);
    }

    fn question(&self) -> &Question {
        &self.questions[self.current]
    }

    fn rows(&self) -> usize {
        self.question().options.len() + 1
    }

    fn on_other_row(&self) -> bool {
        self.selected == self.question().options.len()
    }

    /// Handles a key; the reply comes back once the human has answered every question or
    /// cancelled the call.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<QuestionReply> {
        self.handle_key_at(key, Instant::now())
    }

    /// `handle_key` with the clock supplied.
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<QuestionReply> {
        // Navigation works at once; only the keys that answer or type wait.
        let answers = matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(_));
        if answers
            && (key.kind == KeyEventKind::Repeat
                || now.saturating_duration_since(self.opened) < ANSWER_GRACE)
        {
            return None;
        }
        if self.typing {
            match key.code {
                KeyCode::Enter => return self.commit_other(),
                KeyCode::Esc => self.typing = false,
                _ => {
                    self.other.handle_key(key);
                }
            }
            return None;
        }
        let rows = self.rows();
        let options = rows - 1;
        match key.code {
            KeyCode::Esc => return Some(QuestionReply::Declined),
            KeyCode::Up => self.selected = (self.selected + rows - 1) % rows,
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % rows,
            KeyCode::Enter => return self.confirm(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => match c {
                'k' => self.selected = (self.selected + rows - 1) % rows,
                'j' => self.selected = (self.selected + 1) % rows,
                ' ' if self.question().multi_select => {
                    if self.selected < options {
                        self.checked[self.selected] = !self.checked[self.selected];
                    } else {
                        self.typing = true;
                    }
                }
                '1'..='9' => {
                    let row = usize::from(c as u8 - b'1');
                    if row < rows {
                        self.selected = row;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn confirm(&mut self) -> Option<QuestionReply> {
        if self.on_other_row() {
            self.typing = true;
            return None;
        }
        if self.question().multi_select {
            return self.finish(self.chosen(), self.other_text());
        }
        let label = self.question().options[self.selected].label.clone();
        self.finish(vec![label], None)
    }

    fn commit_other(&mut self) -> Option<QuestionReply> {
        let text = self.other_text()?;
        self.typing = false;
        if self.question().multi_select {
            self.finish(self.chosen(), Some(text))
        } else {
            self.finish(Vec::new(), Some(text))
        }
    }

    fn other_text(&self) -> Option<String> {
        let text = self.other.text();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn chosen(&self) -> Vec<String> {
        self.question()
            .options
            .iter()
            .zip(&self.checked)
            .filter(|(_, on)| **on)
            .map(|(option, _)| option.label.clone())
            .collect()
    }

    /// Records the answer to the current question and moves on. An answer needs at least one
    /// selected label or some typed text, so an empty multi-select answer is ignored.
    fn finish(&mut self, selected: Vec<String>, other: Option<String>) -> Option<QuestionReply> {
        if selected.is_empty() && other.is_none() {
            return None;
        }
        self.answers.push(QuestionAnswer {
            question: self.question().question.clone(),
            selected,
            other,
        });
        if self.current + 1 == self.questions.len() {
            return Some(QuestionReply::Answered(std::mem::take(&mut self.answers)));
        }
        self.current += 1;
        self.selected = 0;
        self.checked = vec![false; self.question().options.len()];
        self.other = InputBuffer::default();
        self.typing = false;
        None
    }

    /// The question and its options, windowed to `max_rows` around the highlighted option.
    pub fn body(&self, width: usize, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let question = self.question();
        let mut head = vec![Line::styled(
            format!(
                "[{}] question {} of {}",
                clean(&question.header),
                self.current + 1,
                self.questions.len()
            ),
            bold.fg(theme.heading),
        )];
        for piece in wrap(&clean(&question.question), width) {
            head.push(Line::styled(piece, bold));
        }
        let max_rows = max_rows.max(1);
        head.truncate(max_rows.saturating_sub(3).max(1));

        let mut rows: Vec<Line<'static>> = Vec::new();
        let (mut sel_start, mut sel_end) = (0, 0);
        let options = question.options.len();
        for row in 0..=options {
            let selected = row == self.selected;
            if selected {
                sel_start = rows.len();
            }
            let mark = if row == options {
                String::new()
            } else if question.multi_select {
                format!("[{}] ", if self.checked[row] { 'x' } else { ' ' })
            } else {
                String::new()
            };
            let label = if row == options {
                "Other".to_string()
            } else {
                clean(&question.options[row].label)
            };
            let style = if selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let text = format!("{}. {mark}{label}", row + 1);
            // The continuation prefix is 3 columns, and the overlay clips rather than wraps.
            for (i, piece) in wrap(&text, width.saturating_sub(3).max(1))
                .into_iter()
                .enumerate()
            {
                rows.push(Line::styled(
                    if i == 0 { piece } else { format!("   {piece}") },
                    style,
                ));
            }
            if row < options {
                let description = clean(&question.options[row].description);
                let indent = "   ";
                for piece in wrap(&description, width.saturating_sub(indent.len()).max(1)) {
                    rows.push(Line::styled(format!("{indent}{piece}"), dim));
                }
            }
            if selected {
                sel_end = rows.len();
            }
        }

        let window = max_rows.saturating_sub(head.len()).max(1);
        if rows.len() > window {
            let mut start = sel_end.saturating_sub(window);
            start = start.min(sel_start);
            rows = rows.into_iter().skip(start).take(window).collect();
        }
        head.extend(rows);
        head
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = Vec::new();
        if self.typing {
            out.push(Line::raw(format!("{OTHER_PROMPT}{}▏", self.other.text())));
            out.push(Line::styled(
                "Enter confirms · Esc back to the options",
                dim,
            ));
        } else if self.question().multi_select {
            out.push(Line::styled(
                "↑/↓ or number moves · Space toggles · Enter confirms · Esc declines",
                dim,
            ));
        } else {
            out.push(Line::styled(
                "↑/↓ or number moves · Enter chooses · Esc declines",
                dim,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_tools::ask_user::QuestionOption;

    fn option(label: &str) -> QuestionOption {
        QuestionOption {
            label: label.into(),
            description: format!("what {label} means"),
        }
    }

    fn question(text: &str, multi: bool) -> Question {
        Question {
            question: text.into(),
            header: "Pick".into(),
            options: vec![option("a"), option("b"), option("c")],
            multi_select: multi,
        }
    }

    fn panel(questions: Vec<Question>) -> (QuestionPanel, oneshot::Receiver<QuestionReply>) {
        let (tx, rx) = oneshot::channel();
        (QuestionPanel::new("root".into(), questions, tx), rx)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(panel: &mut QuestionPanel, code: KeyCode) -> Option<QuestionReply> {
        let later = panel.opened + ANSWER_GRACE;
        panel.handle_key_at(key(code), later)
    }

    fn type_text(panel: &mut QuestionPanel, text: &str) {
        for c in text.chars() {
            press(panel, KeyCode::Char(c));
        }
    }

    fn answered(question: &str, selected: &[&str], other: Option<&str>) -> QuestionAnswer {
        QuestionAnswer {
            question: question.into(),
            selected: selected.iter().map(|s| s.to_string()).collect(),
            other: other.map(str::to_string),
        }
    }

    #[test]
    fn navigation_wraps_and_numbers_jump() {
        let (mut panel, _rx) = panel(vec![question("Q?", false)]);
        press(&mut panel, KeyCode::Up);
        assert_eq!(panel.selected, 3, "wraps to the Other row");
        press(&mut panel, KeyCode::Down);
        assert_eq!(panel.selected, 0);
        press(&mut panel, KeyCode::Char('3'));
        assert_eq!(panel.selected, 2);
        press(&mut panel, KeyCode::Char('9'));
        assert_eq!(panel.selected, 2, "a number past the last row is ignored");
    }

    #[test]
    fn enter_chooses_in_single_select() {
        let (mut panel, _rx) = panel(vec![question("Q?", false)]);
        press(&mut panel, KeyCode::Down);
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            Some(QuestionReply::Answered(vec![answered("Q?", &["b"], None)]))
        );
    }

    #[test]
    fn multi_select_toggles_and_needs_a_choice() {
        let (mut panel, _rx) = panel(vec![question("Q?", true)]);
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            None,
            "nothing chosen yet"
        );
        press(&mut panel, KeyCode::Char(' '));
        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Char(' '));
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            Some(QuestionReply::Answered(vec![answered(
                "Q?",
                &["a", "c"],
                None
            )]))
        );
    }

    #[test]
    fn toggling_twice_unchecks() {
        let (mut panel, _rx) = panel(vec![question("Q?", true)]);
        press(&mut panel, KeyCode::Char(' '));
        press(&mut panel, KeyCode::Char(' '));
        assert_eq!(press(&mut panel, KeyCode::Enter), None);
    }

    #[test]
    fn other_takes_typed_text() {
        let (mut panel, _rx) = panel(vec![question("Q?", false)]);
        press(&mut panel, KeyCode::Char('4'));
        assert_eq!(press(&mut panel, KeyCode::Enter), None);
        assert!(panel.is_typing());
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            None,
            "empty text is not an answer"
        );
        type_text(&mut panel, "my own");
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            Some(QuestionReply::Answered(vec![answered(
                "Q?",
                &[],
                Some("my own")
            )]))
        );
    }

    #[test]
    fn multi_select_combines_labels_with_other() {
        let (mut panel, _rx) = panel(vec![question("Q?", true)]);
        press(&mut panel, KeyCode::Char(' '));
        press(&mut panel, KeyCode::Char('4'));
        press(&mut panel, KeyCode::Char(' '));
        assert!(panel.is_typing());
        type_text(&mut panel, "extra");
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            Some(QuestionReply::Answered(vec![answered(
                "Q?",
                &["a"],
                Some("extra")
            )]))
        );
    }

    #[test]
    fn esc_while_typing_leaves_the_field_and_esc_elsewhere_declines() {
        let (mut panel, _rx) = panel(vec![question("Q?", false)]);
        press(&mut panel, KeyCode::Char('4'));
        press(&mut panel, KeyCode::Enter);
        type_text(&mut panel, "x");
        assert_eq!(press(&mut panel, KeyCode::Esc), None);
        assert!(!panel.is_typing());
        assert_eq!(
            press(&mut panel, KeyCode::Esc),
            Some(QuestionReply::Declined)
        );
    }

    #[test]
    fn answers_advance_and_a_decline_discards_them() {
        let (mut panel, _rx) = panel(vec![question("One?", false), question("Two?", false)]);
        assert_eq!(press(&mut panel, KeyCode::Enter), None);
        assert_eq!(panel.current, 1);
        assert_eq!(panel.selected, 0);
        assert_eq!(
            press(&mut panel, KeyCode::Esc),
            Some(QuestionReply::Declined)
        );

        let (mut panel, _rx) = self::panel(vec![question("One?", false), question("Two?", false)]);
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Down);
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            Some(QuestionReply::Answered(vec![
                answered("One?", &["a"], None),
                answered("Two?", &["b"], None),
            ]))
        );
    }

    #[test]
    fn keys_inside_the_grace_period_do_not_answer() {
        let (mut panel, _rx) = panel(vec![question("Q?", false)]);
        let opened = panel.opened;
        assert_eq!(panel.handle_key_at(key(KeyCode::Enter), opened), None);
        assert_eq!(panel.handle_key_at(key(KeyCode::Esc), opened), None);
        assert_eq!(panel.handle_key_at(key(KeyCode::Char('2')), opened), None);
        assert_eq!(panel.selected, 0);
        // Arrows are navigation, not answers.
        panel.handle_key_at(key(KeyCode::Down), opened);
        assert_eq!(panel.selected, 1);
        let mut repeat = key(KeyCode::Enter);
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(panel.handle_key_at(repeat, opened + ANSWER_GRACE), None);
    }

    #[test]
    fn a_dropped_sender_closes_the_panel() {
        let (panel, rx) = panel(vec![question("Q?", false)]);
        assert!(!panel.is_closed());
        drop(rx);
        assert!(panel.is_closed());
    }

    #[test]
    fn a_reply_reaches_the_waiting_call() {
        let (mut panel, mut rx) = panel(vec![question("Q?", false)]);
        panel.reply(QuestionReply::Declined);
        assert_eq!(rx.try_recv().unwrap(), QuestionReply::Declined);
    }

    #[test]
    fn a_short_window_keeps_the_selected_option_visible() {
        let (mut panel, _rx) = panel(vec![question("Q?", false)]);
        let theme = Theme::default();
        for target in 0..4 {
            panel.selected = target;
            let body = panel.body(40, 6, &theme);
            assert!(body.len() <= 6, "{} rows", body.len());
            let shown: Vec<String> = body
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            let label = if target == 3 {
                "Other"
            } else {
                ["a", "b", "c"][target]
            };
            assert!(
                shown.iter().any(|row| row.starts_with(&format!("{}. ", target + 1)) && row.contains(label)),
                "option {target} is visible in {shown:?}"
            );
        }
    }

    #[test]
    fn wrapped_labels_fit_the_width() {
        let mut q = question("Q?", true);
        q.options[0].label = "a very long option label that has to wrap over several rows".into();
        let (panel, _rx) = panel(vec![q]);
        for line in panel.body(30, 40, &Theme::default()) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.chars().count() <= 30, "{text:?} is too wide");
        }
    }

    #[test]
    fn model_written_control_characters_are_not_drawn() {
        let mut q = question("Q?\u{1b}[31m", false);
        q.options[0].label = "a\u{7}".into();
        let (panel, _rx) = panel(vec![q]);
        let body = panel.body(40, 20, &Theme::default());
        for line in body {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(!text.chars().any(char::is_control), "{text:?}");
        }
    }
}
