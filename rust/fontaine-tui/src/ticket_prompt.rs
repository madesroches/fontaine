//! The dialog that shows a remote-inbox ticket once. The ticket is a bearer credential, so it
//! lives only here: not in the transcript, the ledger, the history or an export. Dismissing the
//! dialog wipes it.

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
            Line::styled("Remote inbox ticket (shown once)", bold.fg(theme.heading)),
            Line::raw(format!(
                "Valid for {}, good for one message. Anyone holding it can send this session a message as you.",
                self.lifetime
            )),
            Line::raw("On the other device: fontaine send --ticket-stdin \"your message\""),
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
        // Best effort: the credential should not outlive the dialog in this buffer.
        self.ticket.clear();
    }
}
