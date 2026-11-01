//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `dontAsk`.

use std::path::Path;

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

/// Longest directory the bar shows, in columns, and longest branch.
const DIR_COLUMNS: usize = 40;
const BRANCH_COLUMNS: usize = 24;

/// `cwd` with `home` written `~`, cut from the left with `…` to `DIR_COLUMNS`.
fn dir_label(cwd: &Path, home: Option<&Path>) -> String {
    let shown = match home.and_then(|home| cwd.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => cwd.display().to_string(),
    };
    let shown = shown.strip_prefix(r"\\?\").unwrap_or(&shown);
    let chars: Vec<char> = shown.chars().collect();
    if chars.len() <= DIR_COLUMNS {
        return shown.to_string();
    }
    let kept: String = chars[chars.len() - (DIR_COLUMNS - 1)..].iter().collect();
    format!("…{kept}")
}

/// ` · <dir> (<branch>)`, or nothing before the first draw has set a directory.
fn location(app: &App) -> String {
    if app.cwd.as_os_str().is_empty() {
        return String::new();
    }
    let dir = dir_label(&app.cwd, app.home.as_deref());
    match &app.branch {
        Some(branch) => format!(
            "· {dir} ({}) ",
            crate::transcript::truncate(branch, BRANCH_COLUMNS)
        ),
        None => format!("· {dir} "),
    }
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
        format!(
            "{}· {}{agents}{scheduled}{goal} · {hint}",
            location(app),
            app.model
        ),
        dim,
    ));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    fn located(cwd: &str, branch: Option<&str>) -> App {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.cwd = PathBuf::from(cwd);
        app.branch = branch.map(str::to_string);
        app
    }

    #[test]
    fn the_directory_and_branch_sit_between_the_mode_and_the_model() {
        let text = bar(&located("/work/proj", Some("main")));
        assert!(
            text.contains("manual · /work/proj (main) · m · shift+tab"),
            "{text}"
        );
        let text = bar(&located("/work/proj", None));
        assert!(text.contains("· /work/proj · m ·"), "{text}");
        let text = bar(&located("/work/proj", Some("0123456")));
        assert!(text.contains("(0123456)"), "{text}");
        let text = bar(&App::new(PermissionMode::Manual, "m".into()));
        assert!(text.contains("manual · m ·"), "{text}");
    }

    #[test]
    fn a_long_directory_is_cut_from_the_left_and_home_is_a_tilde() {
        let mut app = located(
            "/home/u/some/deeply/nested/project/directory/with/a/long/path",
            None,
        );
        app.home = Some(PathBuf::from("/elsewhere"));
        let text = bar(&app);
        let shown = text.split("· ").nth(1).unwrap();
        assert!(shown.starts_with('…'), "{text}");
        assert!(shown.contains("long/path"), "{text}");
        assert!(shown.trim_end().chars().count() >= DIR_COLUMNS, "{text}");

        let mut app = located("/home/u/proj", Some("main"));
        app.home = Some(PathBuf::from("/home/u"));
        assert!(bar(&app).contains("· ~/proj (main) ·"));
        app.cwd = PathBuf::from("/home/u");
        assert!(bar(&app).contains("· ~ (main) ·"));
    }

    #[test]
    fn a_long_branch_is_cut_and_the_hint_still_comes_last() {
        let branch = "feature/a-very-long-branch-name-that-goes-on";
        let text = bar(&located("/w", Some(branch)));
        assert!(text.contains("(feature/a-very-long-bra…)"), "{text}");
        assert!(text.trim_end().ends_with("/exit"), "{text}");
    }

    #[test]
    fn viewing_a_subagent_keeps_the_main_directory() {
        let mut app = located("/w", Some("main"));
        app.viewing = Some("explore-1".into());
        assert!(bar(&app).contains("· /w (main) ·"));
    }

    #[test]
    fn the_branch_is_read_again_at_every_draw() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(dir.join(".git/objects")).unwrap();
        std::fs::create_dir_all(dir.join(".git/refs")).unwrap();
        let head = dir.join(".git/HEAD");
        std::fs::write(&head, "ref: refs/heads/main\n").unwrap();
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.set_cwd(&dir);
        assert_eq!(app.branch.as_deref(), Some("main"));
        std::fs::write(&head, "ref: refs/heads/scratch\n").unwrap();
        app.set_cwd(&dir);
        assert_eq!(app.branch.as_deref(), Some("scratch"));
        std::fs::remove_dir_all(dir.join(".git")).unwrap();
        app.set_cwd(&dir);
        assert_eq!(app.branch, None);
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
