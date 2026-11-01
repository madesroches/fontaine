//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `dontAsk`.

use fontaine_core::config::PermissionMode;
use fontaine_core::goal;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::agents_panel::status_label;
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
    // In an agent's view the state is the agent's: the main turn is out of sight, and Esc does
    // not stop it from here.
    let viewed_state = app.viewing.as_ref().map(|id| {
        let state = match app.viewed_row().and_then(|row| row.status) {
            Some(status) if status.is_running() => "working".to_string(),
            Some(status) => status_label(status).to_string(),
            None => "no longer listed".to_string(),
        };
        format!("viewing {id} · {state} · ↓ then Enter on main to go back · shift+tab mode")
    });
    // Only while the turn runs: some paths clear `running` without settling the turn.
    let refusal = app.refusal.as_deref().filter(|_| app.running);
    let hint = if let Some(text) = refusal {
        text
    } else if let Some(text) = &app.attach_notice {
        text.as_str()
    } else if let Some(text) = &viewed_state {
        text.as_str()
    } else if app.running {
        "esc to interrupt · shift+tab mode"
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
    let mut spans = vec![Span::styled(
        format!(" {} ", mode_label(app.mode)),
        mode_style(app.mode, &app.theme),
    )];
    if app.guardian_off {
        spans.push(Span::styled(
            " ⚠ guardian off ",
            Style::default()
                .fg(app.theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(
        format!("· {}{agents}{scheduled}{goal} · {hint}", app.model),
        dim,
    ));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn bar(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), app))
            .expect("draw the status bar");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn the_refusal_replaces_only_the_hint_and_only_while_running() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.running = true;
        app.refusal = Some("a turn is running".into());
        let text = bar(&app);
        assert!(text.contains("a turn is running"), "{text}");
        assert!(!text.contains("esc to interrupt"), "{text}");
        assert!(text.contains(mode_label(app.mode).trim()), "{text}");

        app.running = false;
        let text = bar(&app);
        assert!(!text.contains("a turn is running"), "{text}");
    }

    #[test]
    fn the_running_hint_offers_interrupt_without_the_word_working() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.running = true;
        let text = bar(&app);
        assert!(text.contains("esc to interrupt · shift+tab mode"), "{text}");
        assert!(!text.contains("working"), "{text}");
    }

    #[test]
    fn an_attach_notice_shows_idle_and_yields_to_a_running_refusal() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.attach_notice = Some("no image on the clipboard".into());
        let text = bar(&app);
        assert!(text.contains("no image on the clipboard"), "{text}");

        app.running = true;
        app.refusal = Some("a turn is running".into());
        let text = bar(&app);
        assert!(text.contains("a turn is running"), "{text}");
        assert!(!text.contains("no image on the clipboard"), "{text}");
    }

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
