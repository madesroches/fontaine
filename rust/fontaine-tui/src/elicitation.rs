//! The dialog for an MCP server's request for input: the server's message and a form built from
//! its schema. What is typed goes to that server only; it never reaches the model.

use std::time::Instant;

use fontaine_tools::mcp::{Choice, ElicitForm, ElicitReply, Field, FieldKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::confirm_prompt::ANSWER_GRACE;
use crate::input::{InputBuffer, wrap};
use crate::question::clean_multiline;
use crate::theme::Theme;
use crate::url_elicitation::{ElicitKey, UrlElicitationPanel};

/// An MCP server's request for input and the dialog that answers it.
pub enum ElicitDialog {
    Form(Box<ElicitationPanel>),
    Url(UrlElicitationPanel),
}

impl ElicitDialog {
    pub(crate) fn id(&self) -> u64 {
        match self {
            ElicitDialog::Form(panel) => panel.id,
            ElicitDialog::Url(panel) => panel.id,
        }
    }

    /// The overlay title; both renderers match on it to say the dialog needs an answer.
    pub(crate) fn title(&self) -> &'static str {
        match self {
            ElicitDialog::Form(_) => "MCP input",
            ElicitDialog::Url(_) => "MCP link",
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        match self {
            ElicitDialog::Form(panel) => panel.is_closed(),
            ElicitDialog::Url(panel) => panel.is_closed(),
        }
    }

    pub(crate) fn reply(&mut self, reply: ElicitReply) {
        match self {
            ElicitDialog::Form(panel) => panel.reply(reply),
            ElicitDialog::Url(panel) => panel.reply(reply),
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ElicitKey {
        match self {
            ElicitDialog::Form(panel) => match panel.handle_key(key) {
                Some(reply) => ElicitKey::Done(reply),
                None => ElicitKey::Open,
            },
            ElicitDialog::Url(panel) => panel.handle_key(key),
        }
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        match self {
            ElicitDialog::Form(panel) => panel.handle_paste(text),
            ElicitDialog::Url(panel) => panel.handle_paste(text),
        }
    }

    pub(crate) fn body(&self, width: usize, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        match self {
            ElicitDialog::Form(panel) => panel.body(width, max_rows, theme),
            ElicitDialog::Url(panel) => panel.body(width, max_rows, theme),
        }
    }

    pub(crate) fn footer(&self) -> Vec<Line<'static>> {
        match self {
            ElicitDialog::Form(panel) => panel.footer(),
            ElicitDialog::Url(panel) => panel.footer(),
        }
    }

    pub(crate) fn field(&self) -> Option<(&'static str, String)> {
        match self {
            ElicitDialog::Form(panel) => panel.field(),
            ElicitDialog::Url(panel) => panel.field(),
        }
    }

    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        match self {
            ElicitDialog::Form(panel) => panel.settle(),
            ElicitDialog::Url(panel) => panel.settle(),
        }
    }
}

const CARET: &str = "▏";
const PRIVACY: &str =
    "This goes to the server, not to the model. Don't enter secrets you wouldn't give this server.";

/// What a field holds while the human edits it.
enum Entry {
    Text(InputBuffer),
    Bool(bool),
    /// The highlighted option and the chosen one, which stays unset until picked.
    Select {
        cursor: usize,
        chosen: Option<usize>,
    },
    Multi {
        cursor: usize,
        checked: Vec<bool>,
    },
}

fn entry_for(kind: &FieldKind) -> Entry {
    let position = |options: &[Choice], value: &str| options.iter().position(|o| o.value == value);
    match kind {
        FieldKind::Text { default, .. } => {
            Entry::Text(prefilled(default.clone().unwrap_or_default()))
        }
        FieldKind::Number { default, .. } => Entry::Text(prefilled(
            default.map(|n| n.to_string()).unwrap_or_default(),
        )),
        FieldKind::Integer { default, .. } => Entry::Text(prefilled(
            default.map(|n| n.to_string()).unwrap_or_default(),
        )),
        FieldKind::Boolean { default } => Entry::Bool(*default),
        FieldKind::Select { options, default } => {
            let chosen = default.as_deref().and_then(|d| position(options, d));
            Entry::Select {
                cursor: chosen.unwrap_or(0),
                chosen,
            }
        }
        FieldKind::MultiSelect {
            options, default, ..
        } => Entry::Multi {
            cursor: 0,
            checked: options
                .iter()
                .map(|option| default.contains(&option.value))
                .collect(),
        },
    }
}

fn prefilled(text: String) -> InputBuffer {
    let mut buffer = InputBuffer::default();
    buffer.set_text(&clean_single_line(&text));
    buffer
}

fn clean_single_line(text: &str) -> String {
    clean_multiline(text).replace('\n', " ")
}

pub struct ElicitationPanel {
    /// Set by the app; tells two consecutive dialogs apart.
    pub(crate) id: u64,
    server: String,
    message: String,
    form: ElicitForm,
    fields: Vec<Field>,
    entries: Vec<Entry>,
    reply: Option<oneshot::Sender<ElicitReply>>,
    focus: usize,
    /// Why the last attempt to submit was refused.
    error: Option<String>,
    opened: Instant,
}

impl ElicitationPanel {
    pub fn new(form: ElicitForm, reply: oneshot::Sender<ElicitReply>) -> ElicitationPanel {
        let fields = form.fields();
        let entries = fields.iter().map(|field| entry_for(&field.kind)).collect();
        ElicitationPanel {
            id: 0,
            server: clean_single_line(&form.server),
            message: clean_multiline(&form.message),
            form,
            fields,
            entries,
            reply: Some(reply),
            focus: 0,
            error: None,
            opened: Instant::now(),
        }
    }

    /// Whether nobody waits for the answer any more, such as when the call ended.
    pub fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(oneshot::Sender::is_closed)
    }

    /// Every field is drawn inline, so there is no separate input row below the overlay.
    pub fn field(&self) -> Option<(&'static str, String)> {
        None
    }

    /// Hands the answer to the waiting server connection. One nobody waits for is dropped.
    pub fn reply(&mut self, reply: ElicitReply) {
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

    /// A paste lands in the focused text field, never in the message box.
    pub fn handle_paste(&mut self, text: &str) {
        if let Some(Entry::Text(buffer)) = self.entries.get_mut(self.focus) {
            buffer.insert_str(&clean_single_line(text));
            self.error = None;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ElicitReply> {
        self.handle_key_at(key, Instant::now())
    }

    /// Handles a key; the answer comes back once the human submits, declines or cancels.
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<ElicitReply> {
        // Navigation works at once; only the keys that answer or type wait.
        let answers = matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(_));
        if answers
            && (key.kind == KeyEventKind::Repeat
                || now.saturating_duration_since(self.opened) < ANSWER_GRACE)
        {
            return None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Some(ElicitReply::Cancel),
            KeyCode::Char('d') if control => return Some(ElicitReply::Decline),
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab | KeyCode::Down => self.move_focus(1),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(-1),
            _ => self.edit(key),
        }
        None
    }

    fn move_focus(&mut self, by: isize) {
        let count = self.fields.len();
        if count > 0 {
            self.focus = (self.focus as isize + by).rem_euclid(count as isize) as usize;
        }
    }

    fn edit(&mut self, key: KeyEvent) {
        let Some(entry) = self.entries.get_mut(self.focus) else {
            return;
        };
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let options = match &self.fields[self.focus].kind {
            FieldKind::Select { options, .. } | FieldKind::MultiSelect { options, .. } => {
                options.len()
            }
            _ => 0,
        };
        let required = self.fields[self.focus].required;
        match entry {
            Entry::Text(buffer) => {
                buffer.handle_key(key);
            }
            Entry::Bool(value) => match key.code {
                KeyCode::Char(' ') if plain => *value = !*value,
                KeyCode::Left => *value = false,
                KeyCode::Right => *value = true,
                _ => {}
            },
            Entry::Select { cursor, chosen } => match key.code {
                KeyCode::Left if options > 0 => *cursor = (*cursor + options - 1) % options,
                KeyCode::Right if options > 0 => *cursor = (*cursor + 1) % options,
                KeyCode::Char(' ') if plain => {
                    // Picking the chosen option again clears an optional field.
                    *chosen = if *chosen == Some(*cursor) && !required {
                        None
                    } else {
                        Some(*cursor)
                    };
                }
                _ => {}
            },
            Entry::Multi { cursor, checked } => match key.code {
                KeyCode::Left if options > 0 => *cursor = (*cursor + options - 1) % options,
                KeyCode::Right if options > 0 => *cursor = (*cursor + 1) % options,
                KeyCode::Char(' ') if plain => {
                    if let Some(slot) = checked.get_mut(*cursor) {
                        *slot = !*slot;
                    }
                }
                _ => {}
            },
        }
        self.error = None;
    }

    /// What the form holds as the server's answer, or why it is not yet valid.
    fn answer(&self) -> Result<Map<String, Value>, String> {
        let mut answer = Map::new();
        for (field, entry) in self.fields.iter().zip(&self.entries) {
            let value = match (entry, &field.kind) {
                (Entry::Text(buffer), kind) => kind
                    .value_of_text(&buffer.text())
                    .map_err(|e| format!("{}: {e}", field.title))?,
                (Entry::Bool(value), _) => Some(Value::Bool(*value)),
                (Entry::Select { chosen, .. }, FieldKind::Select { options, .. }) => chosen
                    .and_then(|i| options.get(i))
                    .map(|o| Value::String(o.value.clone())),
                (Entry::Multi { checked, .. }, FieldKind::MultiSelect { options, .. }) => {
                    let picked: Vec<Value> = options
                        .iter()
                        .zip(checked)
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| Value::String(option.value.clone()))
                        .collect();
                    // An untouched optional list is left out rather than sent empty.
                    (!picked.is_empty() || field.required).then_some(Value::Array(picked))
                }
                _ => None,
            };
            if let Some(value) = value {
                answer.insert(field.key.clone(), value);
            }
        }
        self.form.validate(&answer)?;
        Ok(answer)
    }

    fn submit(&mut self) -> Option<ElicitReply> {
        match self.answer() {
            Ok(answer) => Some(ElicitReply::Accept(answer)),
            Err(reason) => {
                self.error = Some(reason);
                None
            }
        }
    }

    /// The server's message and the form, windowed to `max_rows` around the focused field.
    pub fn body(&self, width: usize, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let width = width.max(8);
        let mut head = vec![Line::styled(
            format!("MCP server `{}` asks for input", self.server),
            bold.fg(theme.heading),
        )];
        for line in self.message.lines() {
            for piece in wrap(line, width) {
                head.push(Line::styled(piece, bold));
            }
        }
        let max_rows = max_rows.max(1);
        head.truncate(max_rows.saturating_sub(3).max(1));

        let mut tail: Vec<Line<'static>> = Vec::new();
        if let Some(error) = &self.error {
            for piece in wrap(&clean_single_line(error), width) {
                tail.push(Line::styled(piece, Style::default().fg(theme.error)));
            }
        }

        let mut rows: Vec<Line<'static>> = Vec::new();
        let (mut sel_start, mut sel_end) = (0, 0);
        for (index, (field, entry)) in self.fields.iter().zip(&self.entries).enumerate() {
            let focused = index == self.focus;
            if focused {
                sel_start = rows.len();
            }
            let marker = if focused { "▸ " } else { "  " };
            let required = if field.required { " *" } else { "" };
            let label = format!("{marker}{}{required}", clean_single_line(&field.title));
            let style = if focused {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                bold
            };
            for piece in wrap(&label, width) {
                rows.push(Line::styled(piece, style));
            }
            if let Some(description) = &field.description {
                for piece in wrap(
                    &clean_single_line(description),
                    width.saturating_sub(4).max(1),
                ) {
                    rows.push(Line::styled(format!("    {piece}"), dim));
                }
            }
            for piece in wrap(
                &control_line(field, entry, focused),
                width.saturating_sub(4).max(1),
            ) {
                rows.push(Line::raw(format!("    {piece}")));
            }
            if focused {
                sel_end = rows.len();
            }
        }

        let window = max_rows.saturating_sub(head.len() + tail.len()).max(1);
        if rows.len() > window {
            let mut start = sel_end.saturating_sub(window);
            start = start.min(sel_start);
            rows = rows.into_iter().skip(start).take(window).collect();
        }
        head.extend(rows);
        head.extend(tail);
        head
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let toggle = match self.entries.get(self.focus) {
            Some(Entry::Bool(_)) => " · Space toggles",
            Some(Entry::Select { .. }) => " · ←/→ move · Space picks",
            Some(Entry::Multi { .. }) => " · ←/→ move · Space toggles",
            _ => "",
        };
        vec![
            Line::styled(
                format!(
                    "Tab or ↑/↓ move between fields{toggle} · Enter submits · Esc cancels · Ctrl-D declines"
                ),
                dim,
            ),
            Line::styled(PRIVACY, dim),
        ]
    }
}

/// How a field's current value is drawn.
fn control_line(field: &Field, entry: &Entry, focused: bool) -> String {
    match (entry, &field.kind) {
        (Entry::Text(buffer), _) => {
            let chars: Vec<char> = buffer.text().chars().collect();
            let at = buffer.cursor().min(chars.len());
            let before: String = chars[..at].iter().collect();
            let after: String = chars[at..].iter().collect();
            let caret = if focused { CARET } else { "" };
            format!("[{before}{caret}{after}]")
        }
        (Entry::Bool(value), _) => format!("[{}]", if *value { 'x' } else { ' ' }),
        (Entry::Select { cursor, chosen }, FieldKind::Select { options, .. }) => options
            .iter()
            .enumerate()
            .map(|(i, option)| {
                let dot = if *chosen == Some(i) { '•' } else { ' ' };
                let at = if focused && i == *cursor { ">" } else { " " };
                format!("{at}({dot}) {}", clean_single_line(&option.title))
            })
            .collect::<Vec<_>>()
            .join("  "),
        (Entry::Multi { cursor, checked }, FieldKind::MultiSelect { options, .. }) => options
            .iter()
            .enumerate()
            .map(|(i, option)| {
                let mark = if checked.get(i).copied().unwrap_or(false) {
                    'x'
                } else {
                    ' '
                };
                let at = if focused && i == *cursor { ">" } else { " " };
                format!("{at}[{mark}] {}", clean_single_line(&option.title))
            })
            .collect::<Vec<_>>()
            .join("  "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use fontaine_tools::mcp::elicitation::ElicitationSchema;

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(panel: &mut ElicitationPanel, text: &str) {
        for c in text.chars() {
            assert_eq!(panel.handle_key(press(KeyCode::Char(c))), None);
        }
    }

    fn open(schema: ElicitationSchema) -> (ElicitationPanel, oneshot::Receiver<ElicitReply>) {
        let (tx, rx) = oneshot::channel();
        let mut panel = ElicitationPanel::new(
            ElicitForm {
                server: "srv".into(),
                id: String::new(),
                message: "Who\u{1b}[31m are you?\nSecond line".into(),
                schema,
            },
            tx,
        );
        panel.settle();
        (panel, rx)
    }

    fn name_and_age() -> ElicitationSchema {
        ElicitationSchema::builder()
            .required_string("name")
            .optional_integer("age", 0, 150)
            .build()
            .unwrap()
    }

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn theme() -> Theme {
        Theme::preset(fontaine_core::config::ThemeName::default())
    }

    #[test]
    fn the_body_shows_the_server_the_cleaned_message_and_one_row_per_field() {
        let (panel, _rx) = open(name_and_age());
        let shown = text_of(&panel.body(60, 20, &theme()));
        assert!(shown.contains("MCP server `srv` asks for input"), "{shown}");
        assert!(shown.contains("Who[31m are you?\nSecond line"), "{shown}");
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
        assert!(
            shown.contains("▸ age") && shown.contains("  name *"),
            "{shown}"
        );
        let footer = text_of(&panel.footer());
        assert!(footer.contains("Enter submits") && footer.contains("Ctrl-D declines"));
        assert!(footer.contains("not to the model"), "{footer}");
    }

    #[test]
    fn tab_and_the_arrows_move_between_fields_and_wrap() {
        let (mut panel, _rx) = open(name_and_age());
        assert_eq!(panel.focus, 0);
        panel.handle_key(press(KeyCode::Tab));
        assert_eq!(panel.focus, 1);
        panel.handle_key(press(KeyCode::Down));
        assert_eq!(panel.focus, 0);
        panel.handle_key(press(KeyCode::BackTab));
        assert_eq!(panel.focus, 1);
        panel.handle_key(press(KeyCode::Up));
        assert_eq!(panel.focus, 0);
    }

    #[test]
    fn enter_is_blocked_with_an_inline_error_while_the_form_is_invalid() {
        let (mut panel, _rx) = open(name_and_age());
        // `age` comes first and is optional; `name` is required and empty.
        assert_eq!(panel.handle_key(press(KeyCode::Enter)), None);
        let shown = text_of(&panel.body(60, 20, &theme()));
        assert!(shown.contains("name is required"), "{shown}");

        // An out-of-range number is refused too, and typing clears the message.
        type_text(&mut panel, "500");
        panel.handle_key(press(KeyCode::Tab));
        type_text(&mut panel, "Ada");
        assert_eq!(panel.handle_key(press(KeyCode::Enter)), None);
        assert!(text_of(&panel.body(60, 20, &theme())).contains("at most 150"));
        panel.handle_key(press(KeyCode::BackTab));
        for _ in 0..3 {
            panel.handle_key(press(KeyCode::Backspace));
        }
        type_text(&mut panel, "36");
        assert!(!text_of(&panel.body(60, 20, &theme())).contains("at most"));
    }

    #[test]
    fn a_valid_form_submits_its_typed_values() {
        let (mut panel, _rx) = open(name_and_age());
        type_text(&mut panel, "36");
        panel.handle_key(press(KeyCode::Tab));
        type_text(&mut panel, "Ada");
        let Some(ElicitReply::Accept(answer)) = panel.handle_key(press(KeyCode::Enter)) else {
            panic!("the form is valid");
        };
        assert_eq!(answer["name"], "Ada");
        assert_eq!(answer["age"], 36);
    }

    #[test]
    fn an_empty_optional_field_is_left_out() {
        let (mut panel, _rx) = open(name_and_age());
        panel.handle_key(press(KeyCode::Tab));
        type_text(&mut panel, "Ada");
        let Some(ElicitReply::Accept(answer)) = panel.handle_key(press(KeyCode::Enter)) else {
            panic!("the form is valid");
        };
        assert!(!answer.contains_key("age"), "{answer:?}");
    }

    #[test]
    fn esc_cancels_and_ctrl_d_declines() {
        let (mut panel, _rx) = open(name_and_age());
        assert_eq!(
            panel.handle_key(press(KeyCode::Esc)),
            Some(ElicitReply::Cancel)
        );
        let (mut panel, _rx) = open(name_and_age());
        assert_eq!(panel.handle_key(ctrl('d')), Some(ElicitReply::Decline));
    }

    #[test]
    fn keys_that_answer_wait_out_the_grace_period_and_navigation_does_not() {
        let (tx, _rx) = oneshot::channel();
        let mut panel = ElicitationPanel::new(
            ElicitForm {
                server: "srv".into(),
                id: String::new(),
                message: String::new(),
                schema: name_and_age(),
            },
            tx,
        );
        let now = Instant::now();
        assert_eq!(panel.handle_key_at(press(KeyCode::Esc), now), None);
        assert_eq!(panel.handle_key_at(press(KeyCode::Char('x')), now), None);
        assert!(text_of(&panel.body(60, 20, &theme())).contains("age"));
        panel.handle_key_at(press(KeyCode::Tab), now);
        assert_eq!(panel.focus, 1, "navigation is immediate");
        match &panel.entries[0] {
            Entry::Text(buffer) => assert!(buffer.is_empty(), "typing waited too"),
            _ => panic!("a text field"),
        }
    }

    #[test]
    fn a_paste_goes_to_the_focused_field_as_one_line() {
        let (mut panel, _rx) = open(name_and_age());
        panel.handle_key(press(KeyCode::Tab));
        panel.handle_paste("Ada\nLovelace");
        let Entry::Text(buffer) = &panel.entries[1] else {
            panic!("a text field");
        };
        assert_eq!(buffer.text(), "Ada Lovelace");
    }

    #[test]
    fn defaults_prefill_booleans_numbers_and_choices() {
        let schema = ElicitationSchema::builder()
            .optional_bool("ok", true)
            .optional_integer_with("n", |s| s.range(0, 10).with_default(4))
            .build()
            .unwrap();
        let (mut panel, _rx) = open(schema);
        let Some(ElicitReply::Accept(answer)) = panel.handle_key(press(KeyCode::Enter)) else {
            panic!("the defaults are valid");
        };
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["n"], 4);
    }

    #[test]
    fn a_checkbox_toggles_with_space() {
        let schema = ElicitationSchema::builder()
            .required_bool("ok")
            .build()
            .unwrap();
        let (mut panel, _rx) = open(schema);
        panel.handle_key(press(KeyCode::Char(' ')));
        let Some(ElicitReply::Accept(answer)) = panel.handle_key(press(KeyCode::Enter)) else {
            panic!("valid");
        };
        assert_eq!(answer["ok"], true);
    }

    #[test]
    fn a_single_select_is_picked_with_space_and_required_until_then() {
        use fontaine_tools::mcp::elicitation::ElicitationSchema as Schema;
        let schema: Schema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {"pick": {
                "type": "string",
                "oneOf": [{"const": "a", "title": "Alpha"}, {"const": "b", "title": "Beta"}],
            }},
            "required": ["pick"],
        }))
        .unwrap();
        let (mut panel, _rx) = open(schema);
        assert!(text_of(&panel.body(60, 20, &theme())).contains("Alpha"));
        assert_eq!(panel.handle_key(press(KeyCode::Enter)), None);
        assert!(text_of(&panel.body(60, 20, &theme())).contains("pick is required"));
        panel.handle_key(press(KeyCode::Right));
        panel.handle_key(press(KeyCode::Char(' ')));
        let Some(ElicitReply::Accept(answer)) = panel.handle_key(press(KeyCode::Enter)) else {
            panic!("valid");
        };
        assert_eq!(answer["pick"], "b");
    }

    #[test]
    fn a_multi_select_honours_its_bounds() {
        let schema: ElicitationSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {"many": {
                "type": "array",
                "minItems": 1,
                "maxItems": 2,
                "items": {"type": "string", "enum": ["x", "y", "z"]},
            }},
            "required": ["many"],
        }))
        .unwrap();
        let (mut panel, _rx) = open(schema);
        assert_eq!(panel.handle_key(press(KeyCode::Enter)), None);
        for _ in 0..3 {
            panel.handle_key(press(KeyCode::Char(' ')));
            panel.handle_key(press(KeyCode::Right));
        }
        assert_eq!(panel.handle_key(press(KeyCode::Enter)), None);
        assert!(text_of(&panel.body(60, 20, &theme())).contains("at most 2"));
        // The cursor wrapped back to the first choice; clearing it leaves two.
        panel.handle_key(press(KeyCode::Char(' ')));
        let Some(ElicitReply::Accept(answer)) = panel.handle_key(press(KeyCode::Enter)) else {
            panic!("valid");
        };
        assert_eq!(answer["many"], serde_json::json!(["y", "z"]));
    }

    #[test]
    fn a_tall_form_is_windowed_around_the_focused_field() {
        let mut builder = ElicitationSchema::builder();
        for i in 0..12 {
            builder = builder.optional_string(format!("f{i:02}"));
        }
        let (mut panel, _rx) = open(builder.build().unwrap());
        for _ in 0..11 {
            panel.handle_key(press(KeyCode::Tab));
        }
        let lines = panel.body(60, 8, &theme());
        assert!(lines.len() <= 8, "{}", lines.len());
        assert!(text_of(&lines).contains("▸ f11"));
    }

    #[test]
    fn the_reply_reaches_the_waiting_connection_once() {
        let (mut panel, mut rx) = open(name_and_age());
        assert!(!panel.is_closed());
        panel.reply(ElicitReply::Decline);
        panel.reply(ElicitReply::Cancel);
        assert_eq!(rx.try_recv().unwrap(), ElicitReply::Decline);
        assert!(panel.is_closed());
    }
}
