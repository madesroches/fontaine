//! The dialog that shows a remote pairing ticket once. The ticket is a bearer credential, so it
//! lives only here: not in the transcript, the ledger, the history or an export. Dismissing the
//! dialog removes it from the screen and drops the dialog's own copy.

use std::sync::OnceLock;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::qr::Grid;
#[cfg(test)]
use crate::qr::sample_payload;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketKey {
    None,
    Dismiss,
    Copy,
}

pub struct TicketPrompt {
    /// Set by the app; tells two consecutive dialogs apart.
    pub(crate) id: u64,
    ticket: String,
    /// What the QR code encodes; the dialog keeps it because `body` cannot reach the `Ticket`.
    qr_payload: String,
    lifetime: String,
    /// Encoded on the first draw: the payload never changes and encoding costs milliseconds.
    qr_grid: OnceLock<Option<Grid>>,
}

impl TicketPrompt {
    pub fn new(ticket: String, qr_payload: String, lifetime: String) -> TicketPrompt {
        TicketPrompt {
            id: 0,
            ticket,
            qr_payload,
            lifetime,
            qr_grid: OnceLock::new(),
        }
    }

    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TicketKey {
        if key.kind != KeyEventKind::Press || key.modifiers.contains(KeyModifiers::CONTROL) {
            return TicketKey::None;
        }
        match key.code {
            KeyCode::Char('c') => TicketKey::Copy,
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => TicketKey::Dismiss,
            _ => TicketKey::None,
        }
    }

    /// `height` is the rows the body may use. The screen-reader renderer (`linear`) and `NO_COLOR`
    /// get the text ticket only; so does a terminal too small for the code.
    pub fn body(
        &self,
        width: usize,
        height: usize,
        linear: bool,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let heading = Line::styled("Remote ticket (shown once)", bold.fg(theme.heading));
        let width = width.max(1);
        let mut reason: Option<String> = None;
        if linear {
            // The screen reader would read the modules aloud; the text ticket is what it gets.
        } else if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            reason = Some("The QR code is off because NO_COLOR is set.".into());
        } else {
            let validity = Line::raw(format!(
                "Valid for {}, good for one pairing; you confirm the device here.",
                self.lifetime
            ));
            let waiting = Line::raw("Waiting for a device to connect; this closes when one does.");
            let text_rows = 2 + validity.to_string().chars().count().div_ceil(width).max(1);
            let grid = self.qr_grid.get_or_init(|| Grid::encode(&self.qr_payload));
            let code_rows = height.saturating_sub(text_rows);
            match grid {
                Some(g) => match g.render(width, code_rows) {
                    Some(code) => {
                        let mut out = vec![heading, validity];
                        out.extend(code);
                        out.push(waiting);
                        return out;
                    }
                    None => {
                        let (cols, rows) = g.size();
                        reason = Some(too_small_reason(
                            cols.saturating_sub(width),
                            (rows + text_rows).saturating_sub(height),
                        ));
                    }
                },
                None => reason = Some("The ticket is too long for a QR code.".into()),
            }
        }
        let mut out = vec![
            heading,
            Line::raw(format!(
                "Valid for {}, good for one pairing. Whoever presents it first can ask this terminal to pair a device; you confirm the device here before it is paired.",
                self.lifetime
            )),
        ];
        out.extend(reason.map(Line::raw));
        out.push(Line::raw(
            "On the other device: FONTAINE_TICKET=<ticket> fontaine attach",
        ));
        out.push(Line::raw(""));
        let chars: Vec<char> = self.ticket.chars().collect();
        for piece in chars.chunks(width) {
            out.push(Line::raw(piece.iter().collect::<String>()));
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![Line::styled(
            "c copies the ticket · Enter dismisses it for good",
            dim,
        )]
    }
}

fn too_small_reason(more_columns: usize, more_rows: usize) -> String {
    let mut clauses = Vec::new();
    if more_rows > 0 {
        clauses.push(format!(
            "The terminal is too short for the QR code: it needs {more_rows} more rows."
        ));
    }
    if more_columns > 0 {
        clauses.push(format!(
            "The terminal is too narrow for the QR code: it needs {more_columns} more columns."
        ));
    }
    clauses.join(" ")
}

impl Drop for TicketPrompt {
    fn drop(&mut self) {
        // Best effort: the dialog's buffer should not keep the credential; copies made elsewhere (clipboard, encode) are not reached.
        self.ticket.clear();
        self.qr_payload.clear();
        if let Some(Some(grid)) = self.qr_grid.get_mut() {
            grid.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    fn prompt(len: usize) -> TicketPrompt {
        TicketPrompt::new(
            "fontaine-attach1abc".into(),
            sample_payload(len),
            "10m".into(),
        )
    }

    // An interactive attach refuses --ticket-stdin without --message (attach_cmd's terminal_refusal),
    // so the hint must name the FONTAINE_TICKET form that attach_cmd accepts.
    #[test]
    fn hint_uses_the_env_form_an_interactive_attach_accepts() {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return;
        }
        let body = prompt(639).body(78, 20, false, &Theme::default());
        let text = text(&body);
        assert!(
            text.iter()
                .any(|l| l == "On the other device: FONTAINE_TICKET=<ticket> fontaine attach")
        );
        assert!(!text.iter().any(|l| l.contains("--ticket-stdin")));
        assert!(text.iter().any(|l| l == "fontaine-attach1abc"));
    }

    #[test]
    fn the_code_shows_when_it_fits_and_the_hint_goes() {
        // Not asserted under NO_COLOR, which is read from the real environment.
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return;
        }
        let body = prompt(639).body(78, 42, false, &Theme::default());
        let text = text(&body);
        assert_eq!(text[0], "Remote ticket (shown once)");
        assert!(text.iter().any(|l| l.contains('█')));
        assert!(!text.iter().any(|l| l.contains("FONTAINE_TICKET")));
        assert!(!text.iter().any(|l| l.contains("fontaine-attach1abc")));
        assert!(text.last().unwrap().starts_with("Waiting for a device"));
        assert_eq!(body.len(), 1 + 1 + 39 + 1);
        let small = prompt(639).body(78, 41, false, &Theme::default());
        assert!(text_has(&small, "too short for the QR code"));
    }

    fn reason_for(len: usize, width: usize, height: usize) -> Option<String> {
        let body = prompt(len).body(width, height, false, &Theme::default());
        let lines = text(&body);
        lines
            .into_iter()
            .find(|l| l.starts_with("The ") && l.contains("QR code"))
    }

    fn skip_no_color() -> bool {
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
    }

    #[test]
    fn a_wide_short_terminal_blames_the_rows_only() {
        if skip_no_color() {
            return;
        }
        let reason = reason_for(639, 190, 20).expect("a reason");
        assert!(reason.contains("too short") && reason.contains("more rows"));
        assert!(!reason.contains("narrow") && !reason.contains("columns"));
        // 39 code rows + 3 text rows needed, 20 given.
        assert!(reason.contains("needs 22 more rows"), "{reason}");
    }

    #[test]
    fn a_narrow_tall_terminal_blames_the_columns_only() {
        if skip_no_color() {
            return;
        }
        let reason = reason_for(639, 40, 80).expect("a reason");
        assert!(reason.contains("too narrow") && reason.contains("more columns"));
        assert!(!reason.contains("rows"));
    }

    #[test]
    fn a_small_terminal_names_both() {
        if skip_no_color() {
            return;
        }
        let reason = reason_for(639, 40, 20).expect("a reason");
        assert!(reason.contains("more rows") && reason.contains("more columns"));
    }

    #[test]
    fn no_reason_when_the_code_fits() {
        if skip_no_color() {
            return;
        }
        assert_eq!(reason_for(639, 190, 60), None);
    }

    #[test]
    fn a_payload_beyond_qr_capacity_says_the_ticket_is_too_long() {
        if skip_no_color() {
            return;
        }
        let reason = reason_for(5000, 190, 80).expect("a reason");
        assert_eq!(reason, "The ticket is too long for a QR code.");
    }

    #[test]
    fn the_screen_reader_gets_the_text_ticket_only() {
        let body = prompt(120).body(78, 200, true, &Theme::default());
        let text = text(&body);
        assert!(!text.iter().any(|l| l.contains('█') || l.contains('▀')));
        assert!(text.iter().any(|l| l == "fontaine-attach1abc"));
    }

    fn text_has(lines: &[Line<'static>], needle: &str) -> bool {
        lines.iter().any(|l| l.to_string().contains(needle))
    }
}
