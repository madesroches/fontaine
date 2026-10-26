//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `dontAsk`.

use fontaine_core::config::PermissionMode;
use fontaine_core::goal;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::Theme;

pub fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "manual",
        PermissionMode::AcceptEdits => "⏵⏵ accept edits on",
        PermissionMode::Plan => "⏸ plan mode on",
        PermissionMode::Auto => "⏵⏵ auto mode on",
        PermissionMode::DontAsk => "dontAsk",
    }
}

/// The modes Shift+Tab walks through, in order.
const CYCLE: &[PermissionMode] = &[
    PermissionMode::Manual,
    PermissionMode::AcceptEdits,
    PermissionMode::Plan,
    PermissionMode::Auto,
];

/// The mode after `mode` in the Shift+Tab cycle; a mode outside the cycle returns to the first.
pub fn next_mode(mode: PermissionMode) -> PermissionMode {
    match CYCLE.iter().position(|m| *m == mode) {
        Some(i) => CYCLE[(i + 1) % CYCLE.len()],
        None => CYCLE[0],
    }
}

fn mode_style(mode: PermissionMode, theme: &Theme) -> Style {
    let color = match mode {
        PermissionMode::Manual | PermissionMode::DontAsk => theme.mode_manual,
        PermissionMode::AcceptEdits => theme.mode_accept,
        PermissionMode::Plan => theme.mode_plan,
        PermissionMode::Auto => theme.mode_auto,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let hint = if app.running {
        "working · esc to interrupt · shift+tab mode"
    } else {
        "shift+tab mode · /add-dir · /denied · /model · ctrl+t agents · /exit"
    };
    let agents = match app.running_agents {
        0 => String::new(),
        1 => " · 1 agent running".to_string(),
        n => format!(" · {n} agents running"),
    };
    let scheduled = match app.scheduled {
        0 => String::new(),
        n => format!(" · ⏱ {n}"),
    };
    let goal = match app.goal {
        None => String::new(),
        Some((_, true)) => " · ◎ goal paused".to_string(),
        Some((age, false)) => format!(" · ◎ goal {}", goal::duration_text(age)),
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", mode_label(app.mode)),
            mode_style(app.mode, &app.theme),
        ),
        Span::styled(
            format!("· {}{agents}{scheduled}{goal} · {hint}", app.model),
            dim,
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cycle_includes_auto_and_wraps() {
        let mut mode = PermissionMode::Manual;
        let mut seen = Vec::new();
        for _ in 0..5 {
            mode = next_mode(mode);
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
                PermissionMode::AcceptEdits,
                PermissionMode::Plan,
                PermissionMode::Auto,
                PermissionMode::Manual,
                PermissionMode::AcceptEdits
            ]
        );
        assert_eq!(next_mode(PermissionMode::DontAsk), PermissionMode::Manual);
    }
}
