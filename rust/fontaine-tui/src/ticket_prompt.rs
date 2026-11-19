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
    /// get the text ticket only; so does a terminal too narrow for the code. A code too tall for
    /// `height` is drawn whole, with a notice, since the window can be enlarged.
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
            match grid {
                Some(g) => {
                    let (cols, rows) = g.size();
                    if cols > width {
                        reason = Some(too_narrow_reason(cols - width));
                    } else {
                        // A code taller than the dialog is still drawn whole: the window can be
                        // enlarged, and the notice sits above the code so a clipped bottom
                        // does not hide it.
                        let short = (rows + text_rows).saturating_sub(height);
                        let mut out = vec![heading];
                        if short > 0 {
                            out.push(Line::raw(too_short_notice(short)));
                        }
                        out.push(validity);
                        out.extend(g.render(width, usize::MAX).expect("the width was checked"));
                        out.push(waiting);
                        return out;
                    }
                }
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

fn plural(n: usize, unit: &str) -> String {
    if n == 1 {
        format!("1 more {unit}")
    } else {
        format!("{n} more {unit}s")
    }
}

// The figure is a minimum: an expanded todo or agent list can absorb added height.
fn too_short_notice(more_rows: usize) -> String {
    format!(
        "Part of the QR code is cut off: enlarge the window by at least {} to scan it.",
        plural(more_rows, "row")
    )
}

fn too_narrow_reason(more_columns: usize) -> String {
    format!(
        "The terminal is too narrow for the QR code: it needs at least {}.",
        plural(more_columns, "column")
    )
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
        if skip_no_color() {
            return;
        }
        let body = prompt(639).body(40, 20, false, &Theme::default());
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
        if skip_no_color() {
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
        assert!(!text_has(&body, "cut off"));
    }

    // The reported bug: a wide but short terminal got the text ticket and a "too small" line.
    #[test]
    fn a_wide_short_terminal_still_gets_the_whole_code_and_a_notice() {
        if skip_no_color() {
            return;
        }
        let body = prompt(639).body(190, 20, false, &Theme::default());
        let lines = text(&body);
        assert_eq!(lines.len(), 1 + 1 + 1 + 39 + 1);
        // 39 code rows + 3 text rows needed, 20 given; the notice comes before the code.
        assert!(
            lines[1].contains("enlarge the window by at least 22 rows"),
            "{}",
            lines[1]
        );
        assert!(!lines.iter().any(|l| l.contains("fontaine-attach1abc")));
    }

    #[test]
    fn a_narrow_terminal_falls_back_to_the_text_ticket_and_says_why() {
        if skip_no_color() {
            return;
        }
        let body = prompt(639).body(40, 80, false, &Theme::default());
        let lines = text(&body);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("too narrow") && l.contains("more columns"))
        );
        assert!(!lines.iter().any(|l| l.contains('█')));
        assert!(lines.iter().any(|l| l == "fontaine-attach1abc"));
    }

    #[test]
    fn a_payload_beyond_qr_capacity_says_the_ticket_is_too_long() {
        if skip_no_color() {
            return;
        }
        let body = prompt(5000).body(190, 80, false, &Theme::default());
        assert!(text_has(&body, "The ticket is too long for a QR code."));
    }

    fn skip_no_color() -> bool {
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
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
