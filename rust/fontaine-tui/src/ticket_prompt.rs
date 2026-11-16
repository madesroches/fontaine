//! The dialog that shows a remote pairing ticket once. The ticket is a bearer credential, so it
//! lives only here: not in the transcript, the ledger, the history or an export. Dismissing the
//! dialog removes it from the screen and drops the dialog's own copy.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

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
    lifetime: String,
}

impl TicketPrompt {
    pub fn new(ticket: String, lifetime: String) -> TicketPrompt {
        TicketPrompt {
            id: 0,
            ticket,
            lifetime,
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

    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut out = vec![
            Line::styled("Remote ticket (shown once)", bold.fg(theme.heading)),
            Line::raw(format!(
                "Valid for {}, good for one pairing. Whoever presents it first can ask this terminal to pair a device; you confirm the device here before it is paired.",
                self.lifetime
            )),
            Line::raw("On the other device: FONTAINE_TICKET=<ticket> fontaine attach"),
            Line::raw(""),
        ];
        let width = width.max(1);
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

impl Drop for TicketPrompt {
    fn drop(&mut self) {
        // Best effort: the dialog's buffer should not keep the credential; copies made elsewhere (clipboard, encode) are not reached.
        self.ticket.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // An interactive attach refuses --ticket-stdin without --message (attach_cmd's terminal_refusal),
    // so the hint must name the FONTAINE_TICKET form that attach_cmd accepts.
    #[test]
    fn hint_uses_the_env_form_an_interactive_attach_accepts() {
        let body = TicketPrompt::new("t".into(), "10m".into()).body(80, &Theme::default());
        let text: Vec<String> = body.iter().map(|l| l.to_string()).collect();
        assert!(
            text.iter()
                .any(|l| l == "On the other device: FONTAINE_TICKET=<ticket> fontaine attach")
        );
        assert!(!text.iter().any(|l| l.contains("--ticket-stdin")));
    }
}
