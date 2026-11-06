//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `dontAsk`.

use std::fmt::Display;
use std::path::Path;

use chrono::{DateTime, Local, TimeZone, Utc};
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

/// The keystrokes bound to `cycle_mode`, for the notices that tell the human how to leave AFK
/// mode; `None` when it is unbound.
pub(crate) fn cycle_keys(app: &App) -> Option<String> {
    binding(app, TuiAction::CycleMode)
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
        PermissionMode::Afk => "⏵⏵⏵ AFK mode on",
        PermissionMode::DontAsk => "dontAsk",
    }
}

/// The modes Shift+Tab walks through, by increasing autonomy.
const CYCLE: &[PermissionMode] = &[
    PermissionMode::Manual,
    PermissionMode::Plan,
    PermissionMode::AcceptEdits,
    PermissionMode::Auto,
    PermissionMode::Afk,
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
        PermissionMode::Afk => theme.mode_afk,
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

/// The guardian's segment shows once any of its windows is this used.
const HOT_RATIO: f64 = 0.70;

/// The status colour changes once any window is this used.
const WARN_RATIO: f64 = 0.90;

fn percent(ratio: f64) -> u32 {
    (ratio.min(1.0) * 100.0).round() as u32
}

/// When `reset` falls, as read on `now`'s calendar: the clock time today, `tomorrow`, the
/// weekday within the week, the date beyond. Seven days out is today's weekday again, so it gets
/// the date rather than a weekday that reads as this week.
fn reset_label<Tz: TimeZone>(reset: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: Display,
{
    let days = (reset.date_naive() - now.date_naive()).num_days();
    match days {
        ..=0 => reset.format("%H:%M").to_string(),
        1 => "tomorrow".to_string(),
        2..=6 => reset.format("%a").to_string(),
        _ => reset.format("%b %-d").to_string(),
    }
}

/// `<label> · <window> <pct>% (<reset>) · …`, shortest window first, and whether any window is
/// past the warning level. The reset is left out when the provider gives none or it has passed.
/// The guardian's segment carries a `guardian ` prefix and shows only once a window is running
/// hot. `None` when there is nothing to show: no data, or no window.
fn quota_segment<Tz: TimeZone>(usage: &AccountUsage, now: &DateTime<Tz>) -> Option<(String, bool)>
where
    Tz::Offset: Display,
{
    let quota = usage.quota.as_ref()?;
    if quota.windows.is_empty() {
        return None;
    }
    let guardian = usage.role == UsageRole::Guardian;
    if guardian && !quota.windows.iter().any(|w| w.used_ratio >= HOT_RATIO) {
        return None;
    }
    let prefix = if guardian { "guardian " } else { "" };
    let mut text = format!("{prefix}{}", usage.label);
    let mut warn = false;
    for window in quota.windows.iter().rev() {
        text.push_str(&format!(
            " · {} {}%",
            window.label,
            percent(window.used_ratio)
        ));
        let reset = window
            .reset_at
            .map(|at| DateTime::<Utc>::from(at).with_timezone(&now.timezone()))
            .filter(|reset| reset > now);
        if let Some(reset) = reset {
            text.push_str(&format!(" ({})", reset_label(&reset, now)));
        }
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
        let mut parts = vec![
            format!("viewing {who}"),
            state,
            "↓ then Enter on main to go back".to_string(),
        ];
        parts.extend(cycle_hint(app));
        parts.join(" · ")
    });
    // Only while the turn runs: some paths clear `running` without settling the turn.
    let refusal = app.refusal.as_deref().filter(|_| app.running);
    let message = refusal
        .map(str::to_string)
        .or_else(|| app.attach_notice.clone())
        .or_else(|| viewed_state.clone());
    let hint = if message.is_some() {
        message.clone()
    } else if app.running {
        None
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
        Some(parts.join(" · "))
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
    if app.afk_stopped {
        spans.push(Span::styled(
            " AFK STOPPED ",
            Style::default()
                .fg(app.theme.warning)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ));
    }
    let effort = app
        .effort
        .map_or_else(String::new, |level| format!(" · effort {level}"));
    spans.push(Span::styled(
        format!("{}· {}", location(app), app.model),
        dim,
    ));
    let usage_as_of = DateTime::<Local>::from(app.usage_as_of);
    for (text, warn) in app
        .usage
        .iter()
        .filter_map(|usage| quota_segment(usage, &usage_as_of))
    {
        let style = if warn {
            Style::default().fg(app.theme.warning)
        } else {
            dim
        };
        spans.push(Span::styled(format!(" · {text}"), style));
    }
    // The custom line replaces the static hint but never a message the user must see.
    if let (Some(statusline), None) = (&app.statusline, &message) {
        spans.push(Span::styled(
            format!("{effort}{agents}{scheduled}{goal} · "),
            dim,
        ));
        let custom = truncate_row(
            &statusline.line,
            area.width.saturating_sub(width_used(&spans)),
        );
        spans.push(Span::styled(custom, dim));
    } else {
        let trailer = match hint {
            Some(h) => format!("{effort}{agents}{scheduled}{goal} · {h}"),
            None => format!("{effort}{agents}{scheduled}{goal}"),
        };
        spans.push(Span::styled(trailer, dim));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// The displayed width of the spans already assembled, in columns.
fn width_used(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| s.width() as u16).sum()
}

/// Cuts `text` to fit in the remaining `width`, adding a leading ellipsis when truncated.
fn truncate_row(text: &str, width: u16) -> String {
    let cw = |c: char| Span::raw(c.to_string()).width();
    let width = usize::from(width);
    let total: usize = text.chars().map(cw).sum();
    if total <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = cw(c);
        if used + w > width - 1 {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.reverse();
    format!("…{}", kept.into_iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use chrono::FixedOffset;
    use fontaine_core::quota::{Quota, QuotaWindow};

    use super::*;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn truncate_row_keeps_short_text_and_cuts_by_display_width() {
        assert_eq!(truncate_row("hi", 10), "hi");
        assert_eq!(truncate_row("hello world", 5), "…orld");
        assert_eq!(truncate_row("x", 1), "x");
        assert_eq!(truncate_row("xy", 1), "…");
        assert_eq!(truncate_row("日本語日本語", 12), "日本語日本語");
        let cut = truncate_row("日本語日本語", 6);
        assert_eq!(cut, "…本語");
        assert!(Span::raw(cut).width() <= 6);
    }

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
    fn the_running_state_leaves_the_hint_segment_empty_and_has_no_trailing_separator() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.running = true;
        let text = bar(&app);
        assert!(!text.contains("esc to interrupt"), "{text}");
        assert!(!text.contains("shift+tab mode"), "{text}");
        assert!(text.trim_end().ends_with('m'), "{text}");
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

    /// Friday 2026-11-06 10:00 UTC.
    fn friday() -> DateTime<FixedOffset> {
        at("2026-11-06T10:00:00+00:00")
    }

    fn at(rfc3339: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(rfc3339).expect("a valid test instant")
    }

    fn window(label: &str, used_ratio: f64, reset: Option<&str>) -> QuotaWindow {
        QuotaWindow {
            label: label.into(),
            used_ratio,
            reset_at: reset.map(|r| SystemTime::from(at(r))),
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

    /// The four Kimi windows, longest first as the provider lists them.
    fn kimi(month: f64, code: f64, week: f64, five: f64) -> Vec<QuotaWindow> {
        vec![
            window("month", month, Some("2026-12-01T00:00:00+00:00")),
            window("code", code, None),
            window("7d", week, Some("2026-11-08T09:00:00+00:00")),
            window("5h", five, Some("2026-11-06T15:00:00+00:00")),
        ]
    }

    #[test]
    fn a_reset_today_shows_the_clock_time_and_tomorrow_its_name() {
        let now = friday();
        assert_eq!(reset_label(&at("2026-11-06T15:00:00+00:00"), &now), "15:00");
        assert_eq!(
            reset_label(&at("2026-11-07T09:00:00+00:00"), &now),
            "tomorrow"
        );
        let late = at("2026-11-06T23:50:00+00:00");
        assert_eq!(
            reset_label(&at("2026-11-07T00:30:00+00:00"), &late),
            "tomorrow"
        );
    }

    #[test]
    fn a_reset_within_the_week_shows_the_weekday_and_beyond_it_the_date() {
        let now = friday();
        assert_eq!(reset_label(&at("2026-11-08T09:00:00+00:00"), &now), "Sun");
        assert_eq!(reset_label(&at("2026-11-12T09:00:00+00:00"), &now), "Thu");
        assert_eq!(
            reset_label(&at("2026-11-13T09:00:00+00:00"), &now),
            "Nov 13"
        );
        assert_eq!(reset_label(&at("2026-12-01T00:00:00+00:00"), &now), "Dec 1");
    }

    #[test]
    fn the_reset_compares_local_dates_not_utc_ones() {
        // 00:30 local is still the 6th in UTC, but it is tomorrow here.
        let now = at("2026-11-06T23:30:00+05:00");
        assert_eq!(
            reset_label(&at("2026-11-07T00:30:00+05:00"), &now),
            "tomorrow"
        );
        // 20:00 local is already the 7th in UTC, but it is today here.
        let now = at("2026-11-06T10:00:00-08:00");
        assert_eq!(reset_label(&at("2026-11-06T20:00:00-08:00"), &now), "20:00");
        let segment = quota_segment(
            &view(vec![window("5h", 0.6, Some("2026-11-07T04:00:00+00:00"))]),
            &now,
        )
        .unwrap()
        .0;
        assert_eq!(segment, "kimi · 5h 60% (20:00)");
    }

    #[test]
    fn every_window_shows_shortest_first_with_its_reset() {
        let now = friday();
        assert_eq!(
            quota_segment(&view(kimi(0.51, 0.05, 0.30, 0.60)), &now)
                .unwrap()
                .0,
            "kimi · 5h 60% (15:00) · 7d 30% (Sun) · code 5% · month 51% (Dec 1)"
        );
        assert_eq!(
            quota_segment(&view(kimi(0.01, 0.0, 0.02, 0.03)), &now)
                .unwrap()
                .0,
            "kimi · 5h 3% (15:00) · 7d 2% (Sun) · code 0% · month 1% (Dec 1)"
        );
    }

    #[test]
    fn a_missing_or_past_reset_has_no_parentheses() {
        let now = friday();
        let segment = quota_segment(
            &view(vec![
                window("month", 0.2, None),
                window("5h", 0.4, Some("2026-11-06T09:00:00+00:00")),
                window("1h", 0.4, Some("2026-11-06T10:00:00+00:00")),
            ]),
            &now,
        )
        .unwrap()
        .0;
        assert_eq!(segment, "kimi · 1h 40% · 5h 40% · month 20%");
    }

    #[test]
    fn the_percent_is_rounded_and_capped_at_one_hundred() {
        let now = friday();
        let segment = |ratio| {
            quota_segment(&view(vec![window("month", ratio, None)]), &now)
                .unwrap()
                .0
        };
        assert_eq!(segment(0.164), "kimi · month 16%");
        assert_eq!(segment(0.165), "kimi · month 17%");
        assert_eq!(segment(1.3), "kimi · month 100%");
    }

    #[test]
    fn no_window_or_no_data_shows_nothing() {
        let now = friday();
        let mut none = view(vec![]);
        assert_eq!(quota_segment(&none, &now), None);
        none.quota = None;
        assert_eq!(quota_segment(&none, &now), None);
    }

    #[test]
    fn the_quota_warns_from_ninety_percent_on_any_window() {
        let now = friday();
        let warns = |month, code, week, five| {
            quota_segment(&view(kimi(month, code, week, five)), &now)
                .unwrap()
                .1
        };
        assert!(!warns(0.89, 0.89, 0.89, 0.89));
        assert!(warns(0.90, 0.1, 0.1, 0.1));
        assert!(warns(0.1, 0.1, 0.95, 0.1));
        assert!(warns(0.1, 0.1, 0.1, 0.90));
    }

    #[test]
    fn the_guardian_segment_shows_every_window_from_seventy_percent_on_any() {
        let now = friday();
        let shown = |month, five| {
            quota_segment(
                &usage(UsageRole::Guardian, kimi(month, 0.05, 0.3, five)),
                &now,
            )
            .map(|segment| segment.0)
        };
        assert_eq!(shown(0.16, 0.69), None);
        assert_eq!(
            shown(0.16, 0.82).as_deref(),
            Some("guardian kimi · 5h 82% (15:00) · 7d 30% (Sun) · code 5% · month 16% (Dec 1)")
        );
        assert_eq!(
            shown(0.70, 0.1).as_deref(),
            Some("guardian kimi · 5h 10% (15:00) · 7d 30% (Sun) · code 5% · month 70% (Dec 1)")
        );
    }

    #[test]
    fn a_guardian_without_data_shows_nothing() {
        let now = friday();
        let mut none = usage(UsageRole::Guardian, vec![]);
        assert_eq!(quota_segment(&none, &now), None);
        none.quota = None;
        assert_eq!(quota_segment(&none, &now), None);
    }

    #[test]
    fn the_quota_sits_after_the_model_and_before_the_effort() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.effort = Some(fontaine_core::effort::Effort::High);
        assert!(!bar(&app).contains("kimi"), "{}", bar(&app));
        app.usage = vec![view(vec![
            window("month", 0.16, None),
            window("5h", 0.4, None),
        ])];
        let text = bar(&app);
        assert!(
            text.contains("· m · kimi · 5h 40% · month 16% · effort high ·"),
            "{text}"
        );
        app.usage = vec![view(vec![])];
        assert!(!bar(&app).contains("kimi"), "{}", bar(&app));
    }

    #[test]
    fn the_guardian_segment_follows_the_main_one_before_the_effort() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.effort = Some(fontaine_core::effort::Effort::High);
        app.usage = vec![
            view(vec![window("month", 0.16, None)]),
            usage(UsageRole::Guardian, vec![window("month", 0.8, None)]),
        ];
        let text = bar(&app);
        assert!(
            text.contains("· m · kimi · month 16% · guardian kimi · month 80% · effort high ·"),
            "{text}"
        );
    }

    #[test]
    fn the_cycle_walks_by_increasing_autonomy_through_afk_and_wraps() {
        let mut mode = PermissionMode::Manual;
        let mut seen = Vec::new();
        for _ in 0..5 {
            mode = next_mode(mode);
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
                PermissionMode::Plan,
                PermissionMode::AcceptEdits,
                PermissionMode::Auto,
                PermissionMode::Afk,
                PermissionMode::Manual
            ]
        );
        assert_eq!(next_mode(PermissionMode::DontAsk), PermissionMode::Manual);
        assert_eq!(mode_label(PermissionMode::Afk), "⏵⏵⏵ AFK mode on");
    }

    #[test]
    fn a_custom_line_replaces_the_built_in_hint() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.statusline = Some(fontaine_core::statusline::StatuslineState {
            line: "CUSTOM".into(),
        });
        let text = bar(&app);
        assert!(text.contains("CUSTOM"), "{text}");
        assert!(!text.contains("/add-dir"), "{text}");
    }

    #[test]
    fn a_message_replaces_the_custom_line() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.statusline = Some(fontaine_core::statusline::StatuslineState {
            line: "CUSTOM".into(),
        });
        app.attach_notice = Some("NOTICE".into());
        let text = bar(&app);
        assert!(
            text.contains("NOTICE") && !text.contains("CUSTOM"),
            "{text}"
        );
    }

    #[test]
    fn a_custom_line_keeps_the_guardian_off_badge_first() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.guardian_off = true;
        app.statusline = Some(fontaine_core::statusline::StatuslineState {
            line: "custom".into(),
        });
        let text = bar(&app);
        let guardian_pos = text.find("guardian off").expect("guardian badge");
        let custom_pos = text.find("custom").expect("custom line");
        assert!(guardian_pos < custom_pos);
    }

    #[test]
    fn a_long_custom_line_is_truncated_from_the_left() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.statusline = Some(fontaine_core::statusline::StatuslineState {
            line: "a".repeat(200),
        });
        let text = bar(&app);
        assert!(text.contains('…'), "{text}");
        assert!(!text.contains(&"a".repeat(200)), "{text}");
    }
}
