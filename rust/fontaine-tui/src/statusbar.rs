//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `dontAsk`.

use fontaine_core::config::PermissionMode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

pub fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "manual",
        PermissionMode::AcceptEdits => "⏵⏵ accept edits on",
        PermissionMode::Plan => "⏸ plan mode on",
        PermissionMode::Auto => "⏵⏵ auto mode on",
        PermissionMode::DontAsk => "dontAsk",
    }
}

/// The modes Shift+Tab walks through, in order. `auto` joins the end of this list once it can
/// run in the interactive UI.
const CYCLE: &[PermissionMode] = &[
    PermissionMode::Manual,
    PermissionMode::AcceptEdits,
    PermissionMode::Plan,
];

/// The mode after `mode` in the Shift+Tab cycle; a mode outside the cycle returns to the first.
pub fn next_mode(mode: PermissionMode) -> PermissionMode {
    match CYCLE.iter().position(|m| *m == mode) {
        Some(i) => CYCLE[(i + 1) % CYCLE.len()],
        None => CYCLE[0],
    }
}

fn mode_style(mode: PermissionMode) -> Style {
    let color = match mode {
        PermissionMode::Manual | PermissionMode::DontAsk => Color::Gray,
        PermissionMode::AcceptEdits => Color::Green,
        PermissionMode::Plan => Color::Cyan,
        PermissionMode::Auto => Color::Yellow,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let hint = if app.running {
        "working · esc to interrupt · shift+tab mode"
    } else {
        "shift+tab mode · /add-dir · /exit"
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", mode_label(app.mode)), mode_style(app.mode)),
        Span::styled(format!("· {} · {hint}", app.model), dim),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cycle_wraps_without_auto() {
        let mut mode = PermissionMode::Manual;
        let mut seen = Vec::new();
        for _ in 0..4 {
            mode = next_mode(mode);
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
                PermissionMode::AcceptEdits,
                PermissionMode::Plan,
                PermissionMode::Manual,
                PermissionMode::AcceptEdits
            ]
        );
        assert_eq!(next_mode(PermissionMode::DontAsk), PermissionMode::Manual);
    }
}
