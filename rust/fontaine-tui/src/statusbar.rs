//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `dontAsk`.

use std::path::Path;
use std::time::SystemTime;

use fontaine_core::config::PermissionMode;
use fontaine_core::goal;
use fontaine_core::quota::{AccountUsage, UsageRole};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::agents_panel::row_label;
use crate::app::App;
use crate::theme::Theme;
use fontaine_core::keybindings::TuiAction;

/// The keystrokes bound to `action` as the hint strings show them; `None` when the action is
/// unbound, so its hint is dropped.
fn binding(app: &App, action: TuiAction) -> Option<String> {
    let keys = app.keymap.keys_for(action);
    if keys.is_empty() {
        return None;
    }
    Some(
        keys.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// `<keys> mode`, or nothing when `cycle_mode` is unbound.
fn cycle_hint(app: &App) -> Option<String> {
    binding(app, TuiAction::CycleMode).map(|keys| format!("{keys} mode"))
}

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

/// A non-headline window shows once it is this used.
const HOT_RATIO: f64 = 0.70;

/// The status colour changes once any shown window is this used.
const WARN_RATIO: f64 = 0.90;

fn percent(ratio: f64) -> u32 {
    (ratio.min(1.0) * 100.0).round() as u32
}

fn left_text(until: std::time::Duration) -> String {
    let secs = until.as_secs();
    match secs {
        s if s >= 86_400 => format!("{}d left", s / 86_400),
        s if s >= 3_600 => format!("{}h left", s / 3_600),
        s => format!("{}m left", (s / 60).max(1)),
    }
}

/// `<name> <pct>% · <Nd|Nh|Nm> left`, then any other window that is running hot as
/// `<label> <pct>%`; and whether any shown window is past the warning level. The guardian's
/// segment carries a `guardian ` prefix and shows only once a window is running hot. `None`
/// when there is nothing to show: no data, or no window.
fn quota_segment(usage: &AccountUsage, now: SystemTime) -> Option<(String, bool)> {
    let quota = usage.quota.as_ref()?;
    let (headline, rest) = quota.windows.split_first()?;
    let guardian = usage.role == UsageRole::Guardian;
    if guardian && !quota.windows.iter().any(|w| w.used_ratio >= HOT_RATIO) {
        return None;
    }
    let prefix = if guardian { "guardian " } else { "" };
    let mut text = format!("{prefix}{} {}%", usage.label, percent(headline.used_ratio));
    let left = headline
        .reset_at
        .and_then(|reset| reset.duration_since(now).ok())
        .filter(|until| !until.is_zero());
    if let Some(until) = left {
        text.push_str(" · ");
        text.push_str(&left_text(until));
    }
    let mut warn = headline.used_ratio >= WARN_RATIO;
    for window in rest.iter().filter(|w| w.used_ratio >= HOT_RATIO) {
        text.push_str(&format!(
            " · {} {}%",
            window.label,
            percent(window.used_ratio)
        ));
        warn |= window.used_ratio >= WARN_RATIO;
    }
    Some((text, warn))
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // In an agent's view the state is the agent's: the main turn is out of sight, and Esc does
    // not stop it from here.
    let viewed_state = app.viewing.as_ref().map(|id| {
        let state = match app
            .viewed_row()
            .and_then(|row| Some((row.status?, row.idle)))
        {
            Some((status, _)) if status.is_running() => "working".to_string(),
            Some((status, idle)) => row_label(status, idle).to_string(),
            None => "no longer listed".to_string(),
        };
        let who = match app.viewed_row() {
            Some(row) if row.name != *id => format!("{} ({id})", row.name),
            _ => id.clone(),
        };
        let cycle = cycle_hint(app).unwrap_or_default();
        format!("viewing {who} · {state} · ↓ then Enter on main to go back · {cycle}")
    });
    // Only while the turn runs: some paths clear `running` without settling the turn.
    let refusal = app.refusal.as_deref().filter(|_| app.running);
    let hint = if let Some(text) = refusal {
        text.to_string()
    } else if let Some(text) = &app.attach_notice {
        text.clone()
    } else if let Some(text) = &viewed_state {
        text.clone()
    } else if app.running {
        match cycle_hint(app) {
            Some(cycle) => format!("esc to interrupt · {cycle}"),
            None => "esc to interrupt".to_string(),
        }
    } else {
        let mut parts = Vec::new();
        if let Some(cycle) = cycle_hint(app) {
            parts.push(cycle);
        }
        parts.extend([
            "/add-dir".to_string(),
            "/denied".to_string(),
            "/model".to_string(),
        ]);
        if let Some(keys) = binding(app, TuiAction::AgentsPanel) {
            parts.push(format!("{keys} agents"));
        }
        parts.push("/exit".to_string());
        parts.join(" · ")
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
    if let Some(vim) = app.input.vim_label() {
        spans.push(Span::styled(
            format!(" {vim} "),
            Style::default().fg(app.theme.mode_plan),
        ));
    }
    if app.guardian_off {
        spans.push(Span::styled(
            " ⚠ guardian off ",
            Style::default()
                .fg(app.theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let effort = app
        .effort
        .map_or_else(String::new, |level| format!(" · effort {level}"));
    spans.push(Span::styled(
        format!("{}· {}", location(app), app.model),
        dim,
    ));
    for (text, warn) in app
        .usage
        .iter()
        .filter_map(|usage| quota_segment(usage, app.usage_as_of))
    {
        let style = if warn {
            Style::default().fg(app.theme.warning)
        } else {
            dim
        };
        spans.push(Span::styled(format!(" · {text}"), style));
    }
    spans.push(Span::styled(
        format!("{effort}{agents}{scheduled}{goal} · {hint}"),
        dim,
    ));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use fontaine_core::quota::{Quota, QuotaWindow};

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
        let tilde_proj = format!("· ~{}proj (main) ·", std::path::MAIN_SEPARATOR);
        assert!(bar(&app).contains(&tilde_proj), "{}", bar(&app));
        app.cwd = PathBuf::from("/home/u");
        assert!(bar(&app).contains("· ~ (main) ·"));
    }

    #[test]
    fn a_set_effort_follows_the_model_and_an_unset_one_shows_nothing() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        assert!(!bar(&app).contains("effort"), "{}", bar(&app));
        app.effort = Some(fontaine_core::effort::Effort::High);
        assert!(bar(&app).contains("· m · effort high ·"), "{}", bar(&app));
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
    fn the_vim_state_shows_and_unbound_hint_parts_drop() {
        use fontaine_core::config::EditorMode;

        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.input.mode = EditorMode::Vim;
        let text = bar(&app);
        assert!(text.contains("vim:insert"), "{text}");
        app.input
            .handle_key(ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Esc,
                ratatui::crossterm::event::KeyModifiers::NONE,
            ));
        let text = bar(&app);
        assert!(text.contains("vim:normal"), "{text}");

        // An unbound cycle key drops the "shift+tab mode" hint part.
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.keymap = crate::keymap::Keymap::resolve(
            &[("shift+tab".to_string(), String::new())]
                .into_iter()
                .collect(),
        )
        .unwrap();
        let text = bar(&app);
        assert!(!text.contains("shift+tab"), "{text}");
        assert!(text.contains("/add-dir"), "{text}");
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

    fn window(label: &str, used_ratio: f64, reset_in: Option<u64>, now: SystemTime) -> QuotaWindow {
        QuotaWindow {
            label: label.into(),
            used_ratio,
            reset_at: reset_in.map(|s| now + Duration::from_secs(s)),
        }
    }

    fn usage(role: UsageRole, windows: Vec<QuotaWindow>) -> AccountUsage {
        AccountUsage {
            role,
            account: "moon".into(),
            label: "kimi".into(),
            quota: Some(Quota { windows }),
        }
    }

    fn view(windows: Vec<QuotaWindow>) -> AccountUsage {
        usage(UsageRole::Main, windows)
    }

    #[test]
    fn the_quota_headline_shows_the_rounded_percent_and_the_time_left() {
        let now = SystemTime::now();
        let day = 86_400;
        let segment = |ratio, reset| {
            quota_segment(&view(vec![window("month", ratio, reset, now)]), now)
                .unwrap()
                .0
        };
        assert_eq!(segment(0.164, Some(29 * day + 5)), "kimi 16% · 29d left");
        assert_eq!(
            segment(0.165, Some(3 * 3_600 + 59 * 60)),
            "kimi 17% · 3h left"
        );
        assert_eq!(segment(0.5, Some(5 * 60 + 30)), "kimi 50% · 5m left");
        assert_eq!(segment(0.5, Some(10)), "kimi 50% · 1m left");
        assert_eq!(segment(0.5, None), "kimi 50%");
        assert_eq!(segment(0.5, Some(0)), "kimi 50%");
        assert_eq!(segment(1.3, Some(day)), "kimi 100% · 1d left");
    }

    #[test]
    fn a_reset_in_the_past_shows_no_time_left() {
        let now = SystemTime::now();
        let mut v = view(vec![window("month", 0.2, None, now)]);
        v.quota.as_mut().unwrap().windows[0].reset_at = Some(now - Duration::from_secs(60));
        assert_eq!(quota_segment(&v, now).unwrap().0, "kimi 20%");
    }

    #[test]
    fn another_window_shows_only_from_seventy_percent() {
        let now = SystemTime::now();
        let shown = |ratio| {
            quota_segment(
                &view(vec![
                    window("month", 0.16, Some(86_400), now),
                    window("5h", ratio, None, now),
                ]),
                now,
            )
            .unwrap()
            .0
        };
        assert_eq!(shown(0.69), "kimi 16% · 1d left");
        assert_eq!(shown(0.70), "kimi 16% · 1d left · 5h 70%");
        assert_eq!(shown(0.82), "kimi 16% · 1d left · 5h 82%");
    }

    #[test]
    fn the_quota_warns_from_ninety_percent_on_any_shown_window() {
        let now = SystemTime::now();
        let warns = |month, hot| {
            quota_segment(
                &view(vec![
                    window("month", month, None, now),
                    window("5h", hot, None, now),
                ]),
                now,
            )
            .unwrap()
            .1
        };
        assert!(!warns(0.89, 0.5));
        assert!(warns(0.90, 0.5));
        assert!(warns(0.1, 0.95));
        assert!(!warns(0.1, 0.89));
    }

    #[test]
    fn the_quota_sits_after_the_model_and_before_the_effort() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.effort = Some(fontaine_core::effort::Effort::High);
        assert!(!bar(&app).contains("kimi"), "{}", bar(&app));
        let now = SystemTime::now();
        app.usage = vec![view(vec![window(
            "month",
            0.16,
            Some(29 * 86_400 + 5),
            now,
        )])];
        app.usage_as_of = now;
        let text = bar(&app);
        assert!(
            text.contains("· m · kimi 16% · 29d left · effort high ·"),
            "{text}"
        );
        app.usage = vec![view(vec![])];
        assert!(!bar(&app).contains("kimi"), "{}", bar(&app));
    }

    #[test]
    fn the_guardian_segment_shows_from_seventy_percent_on_any_window() {
        let now = SystemTime::now();
        let shown = |month, hot| {
            quota_segment(
                &usage(
                    UsageRole::Guardian,
                    vec![
                        window("month", month, Some(12 * 86_400 + 5), now),
                        window("5h", hot, None, now),
                    ],
                ),
                now,
            )
            .map(|segment| segment.0)
        };
        assert_eq!(shown(0.16, 0.69), None);
        assert_eq!(
            shown(0.70, 0.1).as_deref(),
            Some("guardian kimi 70% · 12d left")
        );
        assert_eq!(
            shown(0.16, 0.82).as_deref(),
            Some("guardian kimi 16% · 12d left · 5h 82%")
        );
    }

    #[test]
    fn a_guardian_without_data_shows_nothing() {
        let now = SystemTime::now();
        let mut none = usage(UsageRole::Guardian, vec![]);
        assert_eq!(quota_segment(&none, now), None);
        none.quota = None;
        assert_eq!(quota_segment(&none, now), None);
    }

    #[test]
    fn the_guardian_segment_follows_the_main_one_before_the_effort() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.effort = Some(fontaine_core::effort::Effort::High);
        let now = SystemTime::now();
        app.usage = vec![
            view(vec![window("month", 0.16, None, now)]),
            usage(UsageRole::Guardian, vec![window("month", 0.8, None, now)]),
        ];
        let text = bar(&app);
        assert!(
            text.contains("· m · kimi 16% · guardian kimi 80% · effort high ·"),
            "{text}"
        );
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
