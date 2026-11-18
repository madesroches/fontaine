//! The status bar and the labels the modes show under. This is the one place that names
//! them: `manual`, `⏵⏵ accept edits on`, `⏵⏵ auto mode on`, `dontAsk`.
//!
//! The bar is a Rune script's line: [`snapshot`] preformats every segment the script can place,
//! the script (the embedded default or the user's) turns it into styled runs, and [`render`] draws
//! the safety badges and then those runs.

use std::path::Path;
use std::sync::OnceLock;

use fontaine_sdk::core::config::PermissionMode;
use fontaine_sdk::core::goal;
use fontaine_sdk::core::quota::{AccountUsage, UsageRole};
use fontaine_sdk::core::statusline::{
    DEFAULT_SCRIPT, Role, StatuslineProgram, StatuslineSnapshot, StatuslineState, StyledText,
    Values, used_percentage,
};
use jiff::{Timestamp, Zoned, tz::TimeZone};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::agents_panel::row_label;
use crate::app::App;
use crate::theme::Theme;
use fontaine_sdk::core::keybindings::TuiAction;

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
        PermissionMode::Auto => "⏵⏵ auto mode on",
        PermissionMode::Afk => "⏵⏵⏵ AFK mode on",
        PermissionMode::DontAsk => "dontAsk",
    }
}

/// The modes Shift+Tab walks through, by increasing autonomy.
const CYCLE: &[PermissionMode] = &[
    PermissionMode::Manual,
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

/// The role the mode chip is drawn in.
fn mode_role(mode: PermissionMode) -> Role {
    match mode {
        PermissionMode::Manual | PermissionMode::DontAsk => Role::ModeManual,
        PermissionMode::AcceptEdits => Role::ModeAccept,
        PermissionMode::Auto => Role::ModeAuto,
        PermissionMode::Afk => Role::ModeAfk,
    }
}

/// The terminal style a role is drawn in. Colours come from the theme; `dim` and `bold` are
/// modifiers on the terminal's own foreground.
pub fn role_style(theme: &Theme, role: Role) -> Style {
    let colored = |color| Style::default().fg(color);
    let bold = |color| colored(color).add_modifier(Modifier::BOLD);
    match role {
        Role::Normal => Style::default(),
        Role::Dim => Style::default().add_modifier(Modifier::DIM),
        Role::Bold => Style::default().add_modifier(Modifier::BOLD),
        Role::Accent => colored(theme.accent),
        Role::Warning => colored(theme.warning),
        Role::WarningBold => bold(theme.warning),
        Role::Success => colored(theme.success),
        Role::Error => colored(theme.error),
        Role::User => colored(theme.user),
        Role::Heading => colored(theme.heading),
        Role::Link => colored(theme.link),
        Role::Shell => colored(theme.shell),
        Role::ModeManual => bold(theme.mode_manual),
        Role::ModeAccept => bold(theme.mode_accept),
        Role::ModeAuto => bold(theme.mode_auto),
        Role::ModeAfk => bold(theme.mode_afk),
    }
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

/// `<dir> (<branch>)`, or nothing before the first draw has set a directory.
fn location(app: &App) -> String {
    if app.cwd.as_os_str().is_empty() {
        return String::new();
    }
    let dir = dir_label(&app.cwd, app.home.as_deref());
    match &app.branch {
        Some(branch) => format!(
            "{dir} ({})",
            crate::transcript::truncate(branch, BRANCH_COLUMNS)
        ),
        None => dir,
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
fn reset_label(reset: &Zoned, now: &Zoned) -> String {
    let days = now
        .date()
        .until(reset.date())
        .map_or(0, |span| span.get_days());
    match days {
        ..=0 => reset.strftime("%H:%M").to_string(),
        1 => "tomorrow".to_string(),
        2..=6 => reset.strftime("%a").to_string(),
        _ => reset.strftime("%b %-d").to_string(),
    }
}

/// `<label> · <window> <pct>% (<reset>) · …`, shortest window first, and whether any window is
/// past the warning level. The reset is left out when the provider gives none or it has passed.
/// The guardian's segment carries a `guardian ` prefix and shows only once a window is running
/// hot. `None` when there is nothing to show: no data, or no window.
fn quota_segment(usage: &AccountUsage, now: &Zoned) -> Option<(String, bool)> {
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
            .and_then(|at| Timestamp::try_from(at).ok())
            .map(|at| at.to_zoned(now.time_zone().clone()))
            .filter(|reset| reset > now);
        if let Some(reset) = reset {
            text.push_str(&format!(" ({})", reset_label(&reset, now)));
        }
        warn |= window.used_ratio >= WARN_RATIO;
    }
    Some((text, warn))
}

/// `1 shell, 2 monitors running (<keys> to manage)`, or nothing with no job. The key hint is
/// left out when `panel_keys` is `None`.
fn jobs_clause(shells: usize, monitors: usize, panel_keys: Option<&str>) -> String {
    let noun = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let counts = match (shells, monitors) {
        (0, 0) => return String::new(),
        (s, 0) => noun(s, "shell", "shells"),
        (0, m) => noun(m, "monitor", "monitors"),
        (s, m) => format!(
            "{}, {}",
            noun(s, "shell", "shells"),
            noun(m, "monitor", "monitors")
        ),
    };
    match panel_keys {
        Some(keys) => format!("{counts} running ({keys} to manage)"),
        None => format!("{counts} running"),
    }
}

/// What the snapshot needs that is not on `App`.
pub struct SnapshotData {
    pub session_id: String,
    pub version: String,
    /// The session's spend, when any call was priced.
    pub cost_total_usd: Option<f64>,
    /// Whether any of the spend is a list-price estimate.
    pub cost_estimated: bool,
    /// Whether a `[statusline] script` renders the line, so `{hint}` leaves the command's line
    /// to `{command}`.
    pub scripted: bool,
}

impl Default for SnapshotData {
    fn default() -> Self {
        SnapshotData {
            session_id: String::new(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            cost_total_usd: None,
            cost_estimated: false,
            scripted: false,
        }
    }
}

/// The state of the agent the human is viewing, which replaces the hint there.
fn viewed_state(app: &App) -> Option<String> {
    // In an agent's view the state is the agent's: the main turn is out of sight, and Esc does
    // not stop it from here.
    let id = app.viewing.as_ref()?;
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
    Some(parts.join(" · "))
}

/// The `hint` segment: a message the human must see (a refusal while a turn runs, an attach
/// notice, the viewed agent's state), else the `command`'s line when no script places it, else
/// nothing while a turn runs, else the idle hint. The command's line is the one run cut from
/// the left.
pub(crate) fn hint(app: &App, command_line: Option<&str>, scripted: bool) -> Option<StyledText> {
    // Only while the turn runs: some paths clear `running` without settling the turn.
    let refusal = app.refusal.as_deref().filter(|_| app.running);
    let message = refusal
        .map(str::to_string)
        .or_else(|| app.attach_notice.clone())
        .or_else(|| viewed_state(app));
    if let Some(message) = message {
        return Some(StyledText::new(message, Role::Dim));
    }
    if let (Some(line), false) = (command_line, scripted) {
        return Some(StyledText::new(line, Role::Dim).tail());
    }
    if app.running {
        return None;
    }
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
    Some(StyledText::new(parts.join(" · "), Role::Dim))
}

/// The quota segment of one role: every account's windows, the warning colour past the warning
/// level.
fn quota_runs(app: &App, role: UsageRole) -> Vec<StyledText> {
    let as_of = Timestamp::try_from(app.usage_as_of)
        .expect("the system clock is within jiff's range")
        .to_zoned(TimeZone::system());
    let mut runs = Vec::new();
    for usage in app.usage.iter().filter(|usage| usage.role == role) {
        let Some((text, warn)) = quota_segment(usage, &as_of) else {
            continue;
        };
        if !runs.is_empty() {
            runs.push(StyledText::new(" · ", Role::Dim));
        }
        runs.push(StyledText::new(
            text,
            if warn { Role::Warning } else { Role::Dim },
        ));
    }
    runs
}

/// The share of the context window in use, rounded; unknown until the window is.
fn context_percentage(app: &App) -> Option<u32> {
    let figures = app.context.as_ref()?;
    used_percentage(figures.used, figures.window?)
}

/// Everything a status-line script can read, preformatted from `app`.
pub fn snapshot(app: &App, data: &SnapshotData) -> StatuslineSnapshot {
    let mut snapshot = StatuslineSnapshot::default();
    let mut put = |name: &'static str, runs: Vec<StyledText>| {
        snapshot.segments.insert(name, runs);
    };
    let dim = |text: String| vec![StyledText::new(text, Role::Dim)];
    let dir = if app.cwd.as_os_str().is_empty() {
        String::new()
    } else {
        dir_label(&app.cwd, app.home.as_deref())
    };
    let branch = app
        .branch
        .as_deref()
        .map(|branch| crate::transcript::truncate(branch, BRANCH_COLUMNS));
    let panel_keys = binding(app, TuiAction::AgentsPanel).filter(|_| !app.attached_device);
    let context = context_percentage(app);
    let used = context.map(|used| used.min(100));
    let cost = data.cost_total_usd.map(|cost| {
        format!(
            "${cost:.4}{}",
            if data.cost_estimated { " est." } else { "" }
        )
    });
    put(
        "mode",
        vec![StyledText::new(mode_label(app.mode), mode_role(app.mode))],
    );
    put(
        "vim",
        app.input
            .vim_label()
            .map(|vim| vec![StyledText::new(vim, Role::Accent)])
            .unwrap_or_default(),
    );
    put("location", dim(location(app)));
    put("dir", dim(dir.clone()));
    put("branch", dim(branch.clone().unwrap_or_default()));
    put("model", dim(app.model.clone()));
    put("quota", quota_runs(app, UsageRole::Main));
    put("quota.guardian", quota_runs(app, UsageRole::Guardian));
    put(
        "effort",
        dim(app
            .effort
            .map_or_else(String::new, |level| format!("effort {level}"))),
    );
    put(
        "agents",
        dim(match app.running_agents {
            0 => String::new(),
            1 => "1 agent running".to_string(),
            n => format!("{n} agents running"),
        }),
    );
    put(
        "jobs",
        dim(jobs_clause(
            app.background_shells,
            app.background_monitors,
            panel_keys.as_deref(),
        )),
    );
    put(
        "scheduled",
        dim(match app.scheduled {
            0 => String::new(),
            n => format!("⏱ {n}"),
        }),
    );
    put(
        "bug_drafts",
        dim(app
            .bug_drafts
            .map_or_else(String::new, |n| format!("{n} bug draft(s): /bug"))),
    );
    put(
        "goal",
        dim(match app.goal {
            None => String::new(),
            Some((_, true)) => "◎ goal paused".to_string(),
            Some((age, false)) => format!("◎ goal {}", goal::duration_text(age)),
        }),
    );
    put(
        "remote",
        dim(app.remote_indicator.clone().unwrap_or_default()),
    );
    put(
        "hint",
        hint(app, app.command_line.as_deref(), data.scripted)
            .into_iter()
            .collect(),
    );
    put(
        "context",
        dim(context.map_or_else(String::new, |pct| format!("ctx {pct}%"))),
    );
    put("cost", dim(cost.unwrap_or_default()));
    put("session_id", dim(data.session_id.clone()));
    put("version", dim(data.version.clone()));
    put(
        "command",
        app.command_line
            .iter()
            .map(|line| StyledText::new(line.as_str(), Role::Dim).tail())
            .collect(),
    );
    // Runs with no text are dropped here so every segment with nothing to show is empty.
    for runs in snapshot.segments.values_mut() {
        runs.retain(|run| !run.text.is_empty());
    }
    snapshot.values = Values {
        context_used_percentage: context,
        context_remaining_percentage: used.map(|used| 100 - used),
        context_total_tokens: app.context.as_ref().map(|figures| figures.used),
        context_window_size: app.context.as_ref().and_then(|figures| figures.window),
        cost_total_cost_usd: data.cost_total_usd,
    };
    snapshot.mode = app.mode.as_str().to_string();
    snapshot.model = app.model.clone();
    snapshot.dir = dir;
    snapshot.branch = branch;
    snapshot.cwd = app.cwd.display().to_string();
    snapshot.project_dir = app.primary_root.display().to_string();
    snapshot.session_id = data.session_id.clone();
    snapshot.version = data.version.clone();
    snapshot.effort = app.effort.map(|level| level.as_str().to_string());
    snapshot.vim = app.input.vim_label().map(str::to_string);
    snapshot.afk = app.afk;
    snapshot.afk_stopped = app.afk_stopped;
    snapshot.guardian_off = app.guardian_off;
    snapshot.running_agents = app.running_agents;
    snapshot.background_shells = app.background_shells;
    snapshot.background_monitors = app.background_monitors;
    snapshot.scheduled = app.scheduled;
    snapshot.bug_drafts = app.bug_drafts;
    snapshot.goal = app.goal.map(|(age, paused)| (age.as_secs(), paused));
    snapshot.remote = app.remote_indicator.clone();
    snapshot.cost_is_estimate = data.cost_estimated;
    snapshot.command_line = app.command_line.clone();
    snapshot
}

/// The embedded default program, compiled once: the line when nothing is configured and the
/// fallback when a script fails.
pub(crate) fn default_program() -> &'static StatuslineProgram {
    static PROGRAM: OnceLock<StatuslineProgram> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        StatuslineProgram::compile(DEFAULT_SCRIPT).expect("the embedded status line compiles")
    })
}

/// Runs a program with the terminal's panic hook told to leave the terminal alone, so a panic
/// the render catches is a failed render and not a broken screen.
pub(crate) fn render_program(
    program: &StatuslineProgram,
    snapshot: &StatuslineSnapshot,
) -> Result<Vec<StyledText>, String> {
    let _rendering = crate::StatuslineRendering::enter();
    program.render(snapshot)
}

/// The default program's line over `app` alone, for a surface that has no config and no session
/// to read the rest from.
fn default_runs(app: &App) -> Vec<StyledText> {
    render_program(default_program(), &snapshot(app, &SnapshotData::default()))
        .expect("the embedded status line renders over any snapshot")
}

/// Caches the default program's line in `app`, for a surface that draws the bar without a
/// `Ui` to do it.
pub(crate) fn render_default(app: &mut App) {
    app.statusline = Some(StatuslineState {
        runs: default_runs(app),
    });
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let runs = app
        .statusline
        .as_ref()
        .map_or(&[][..], |state| state.runs.as_slice());
    draw(frame, area, app, runs);
}

/// The safety badges, which a script cannot drop, then `runs`.
fn draw(frame: &mut Frame, area: Rect, app: &App, runs: &[StyledText]) {
    let mut spans = Vec::new();
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
    // The cut runs get what the badges and every other run leave of the row.
    let fixed = width_used(&spans)
        + runs
            .iter()
            .filter(|run| !run.tail)
            .map(|run| Span::raw(run.text.as_str()).width())
            .sum::<usize>();
    let mut room = usize::from(area.width).saturating_sub(fixed);
    for run in runs {
        let text = if run.tail {
            let cut = truncate_row(&run.text, u16::try_from(room).unwrap_or(u16::MAX));
            room = room.saturating_sub(Span::raw(cut.as_str()).width());
            cut
        } else {
            run.text.clone()
        };
        spans.push(Span::styled(text, role_style(&app.theme, run.role)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The displayed width of the spans already assembled, in columns.
fn width_used(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
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

    use fontaine_sdk::core::quota::{Quota, QuotaWindow};

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

    fn draw_row(app: &App, runs: &[StyledText], width: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), app, runs))
            .expect("draw the status bar");
        terminal.backend().buffer().clone()
    }

    fn text_of(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content().iter().map(|c| c.symbol()).collect()
    }

    /// The bar the default program draws over `app`, 120 columns wide.
    fn bar(app: &App) -> String {
        text_of(&draw_row(app, &default_runs(app), 120))
    }

    fn bar_with(app: &App, data: &SnapshotData) -> String {
        let runs = render_program(default_program(), &snapshot(app, data)).unwrap();
        text_of(&draw_row(app, &runs, 120))
    }

    /// What a user script renders over `app`, as the bar draws it at `width` columns.
    fn scripted(app: &App, source: &str, width: u16) -> String {
        text_of(&draw_row(app, &script_runs(app, source), width))
    }

    fn script_runs(app: &App, source: &str) -> Vec<StyledText> {
        let data = SnapshotData {
            scripted: true,
            ..SnapshotData::default()
        };
        render_program(
            &StatuslineProgram::compile(source).expect("the test script compiles"),
            &snapshot(app, &data),
        )
        .expect("the test script renders")
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
        app.effort = Some(fontaine_sdk::core::effort::Effort::High);
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
        use fontaine_sdk::core::config::EditorMode;

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
    fn friday() -> Zoned {
        at("2026-11-06T10:00:00+00:00")
    }

    // Parsed with the offset as its own zone, because a `Timestamp` would drop it.
    fn at(rfc3339: &str) -> Zoned {
        let offset = &rfc3339[rfc3339.len() - 6..];
        format!("{rfc3339}[{offset}]")
            .parse()
            .expect("a valid test instant")
    }

    fn window(label: &str, used_ratio: f64, reset: Option<&str>) -> QuotaWindow {
        QuotaWindow {
            label: label.into(),
            used_ratio,
            reset_at: reset.map(|r| SystemTime::from(at(r).timestamp())),
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
        app.effort = Some(fontaine_sdk::core::effort::Effort::High);
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
        app.effort = Some(fontaine_sdk::core::effort::Effort::High);
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
        for _ in 0..4 {
            mode = next_mode(mode);
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
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
    fn the_jobs_clause_counts_with_grammar_and_an_optional_key_hint() {
        assert_eq!(jobs_clause(0, 0, Some("ctrl+g")), "");
        assert_eq!(jobs_clause(1, 0, None), "1 shell running");
        assert_eq!(jobs_clause(2, 0, None), "2 shells running");
        assert_eq!(jobs_clause(0, 1, None), "1 monitor running");
        assert_eq!(jobs_clause(0, 3, None), "3 monitors running");
        assert_eq!(jobs_clause(1, 2, None), "1 shell, 2 monitors running");
        assert_eq!(
            jobs_clause(1, 1, Some("ctrl+g")),
            "1 shell, 1 monitor running (ctrl+g to manage)"
        );
    }

    fn bound_panel(app: &mut App) {
        app.keymap = crate::keymap::Keymap::resolve(
            &[("ctrl+g".to_string(), "agents_panel".to_string())]
                .into_iter()
                .collect(),
        )
        .expect("a valid binding");
    }

    #[test]
    fn a_background_job_shows_idle_and_during_a_turn_with_the_panel_key() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        assert!(!bar(&app).contains("running"), "{}", bar(&app));
        app.background_shells = 1;
        let text = bar(&app);
        assert!(text.contains("· 1 shell running"), "{text}");
        assert!(!text.contains("to manage"), "{text}");

        bound_panel(&mut app);
        app.running = true;
        let text = bar(&app);
        assert!(
            text.contains("· 1 shell running (ctrl+g to manage)"),
            "{text}"
        );

        app.attached_device = true;
        let text = bar(&app);
        assert!(text.contains("· 1 shell running"), "{text}");
        assert!(!text.contains("to manage"), "{text}");
    }

    #[test]
    fn a_background_job_follows_the_agents_and_shows_beside_a_command_line() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.running_agents = 2;
        app.background_monitors = 1;
        app.command_line = Some("CUSTOM".into());
        let text = bar(&app);
        assert!(
            text.contains("· 2 agents running · 1 monitor running · CUSTOM"),
            "{text}"
        );
    }

    #[test]
    fn a_command_line_replaces_the_idle_hint_and_shows_while_a_turn_runs() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.command_line = Some("CUSTOM".into());
        let text = bar(&app);
        assert!(text.contains("CUSTOM"), "{text}");
        assert!(!text.contains("/add-dir"), "{text}");
        app.running = true;
        assert!(bar(&app).contains("CUSTOM"), "{}", bar(&app));
    }

    #[test]
    fn a_message_replaces_the_command_line() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.command_line = Some("CUSTOM".into());
        app.attach_notice = Some("NOTICE".into());
        let text = bar(&app);
        assert!(
            text.contains("NOTICE") && !text.contains("CUSTOM"),
            "{text}"
        );
        app.attach_notice = None;
        app.running = true;
        app.refusal = Some("REFUSED".into());
        let text = bar(&app);
        assert!(
            text.contains("REFUSED") && !text.contains("CUSTOM"),
            "{text}"
        );
    }

    #[test]
    fn a_command_line_keeps_the_guardian_off_badge_first() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.guardian_off = true;
        app.command_line = Some("custom".into());
        let text = bar(&app);
        let guardian_pos = text.find("guardian off").expect("guardian badge");
        let custom_pos = text.find("custom").expect("custom line");
        assert!(guardian_pos < custom_pos);
    }

    #[test]
    fn a_long_command_line_is_truncated_from_the_left() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.command_line = Some("a".repeat(200));
        let text = bar(&app);
        assert!(text.contains('…'), "{text}");
        assert!(!text.contains(&"a".repeat(200)), "{text}");
        assert_eq!(text.trim_end().chars().count(), 120, "{text}");
    }

    #[test]
    fn a_scripted_bar_leaves_the_command_line_to_the_command_segment() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.command_line = Some("CUSTOM".into());
        let hinted = scripted(
            &app,
            r#"pub fn statusline(ctx) { ctx.format("{hint}") }"#,
            120,
        );
        assert!(
            hinted.contains("/add-dir") && !hinted.contains("CUSTOM"),
            "{hinted}"
        );
        let placed = scripted(
            &app,
            r#"pub fn statusline(ctx) {
                let line = ctx.line();
                line.push(ctx.format("{model} → {command}"));
                line.push_text(format!(" [{}]", ctx.command_line().unwrap()));
                line
            }"#,
            120,
        );
        assert!(placed.starts_with("m → CUSTOM [CUSTOM]"), "{placed}");
    }

    fn populated() -> App {
        let mut app = located("/work/proj", Some("main"));
        app.input.mode = fontaine_sdk::core::config::EditorMode::Vim;
        app.effort = Some(fontaine_sdk::core::effort::Effort::High);
        app.usage = vec![
            view(vec![window("5h", 0.4, None)]),
            usage(UsageRole::Guardian, vec![window("5h", 0.8, None)]),
        ];
        app.running_agents = 1;
        app.background_shells = 1;
        app.scheduled = 2;
        app.bug_drafts = Some(3);
        app.goal = Some((std::time::Duration::from_secs(240), false));
        app.remote_indicator = Some("attached".into());
        app.context = Some(crate::app::ContextFigures {
            used: 420,
            window: Some(1000),
        });
        app.command_line = Some("CMD".into());
        app
    }

    fn data() -> SnapshotData {
        SnapshotData {
            session_id: "s1".into(),
            version: "1.2.3".into(),
            cost_total_usd: Some(0.5),
            cost_estimated: false,
            scripted: false,
        }
    }

    #[test]
    fn every_segment_is_present_when_there_is_something_to_show_and_absent_when_not() {
        let present = snapshot(&populated(), &data());
        let bare = snapshot(
            &App::new(PermissionMode::Manual, "m".into()),
            &SnapshotData {
                session_id: String::new(),
                version: String::new(),
                ..SnapshotData::default()
            },
        );
        let shown = |snapshot: &StatuslineSnapshot, name: &str| -> String {
            snapshot
                .segment(name)
                .unwrap_or_else(|| panic!("{name} is a segment"))
                .iter()
                .map(|run| run.text.as_str())
                .collect()
        };
        let expected = [
            ("mode", "manual"),
            ("vim", "vim:insert"),
            ("location", "/work/proj (main)"),
            ("dir", "/work/proj"),
            ("branch", "main"),
            ("model", "m"),
            ("quota", "kimi · 5h 40%"),
            ("quota.guardian", "guardian kimi · 5h 80%"),
            ("effort", "effort high"),
            ("agents", "1 agent running"),
            ("jobs", "1 shell running"),
            ("scheduled", "⏱ 2"),
            ("bug_drafts", "3 bug draft(s): /bug"),
            ("goal", "◎ goal 4m"),
            ("remote", "attached"),
            ("context", "ctx 42%"),
            ("cost", "$0.5000"),
            ("session_id", "s1"),
            ("version", "1.2.3"),
            ("command", "CMD"),
        ];
        for (name, text) in expected {
            assert_eq!(shown(&present, name), text, "{name}");
        }
        assert!(shown(&present, "hint").starts_with("CMD"));
        for name in fontaine_sdk::core::statusline::SEGMENTS {
            assert!(
                present.segment(name).is_some_and(|runs| !runs.is_empty()),
                "{name} has nothing to show in the populated app"
            );
        }
        let always = ["mode", "model", "hint"];
        for name in fontaine_sdk::core::statusline::SEGMENTS {
            let runs = bare.segment(name).expect("a segment");
            if always.contains(name) {
                assert!(!runs.is_empty(), "{name}");
            } else {
                assert!(runs.is_empty(), "{name} shows {runs:?} in a bare app");
            }
        }
    }

    #[test]
    fn the_values_follow_the_context_and_the_cost() {
        let snapshot = snapshot(&populated(), &data());
        assert_eq!(snapshot.values.context_used_percentage, Some(42));
        assert_eq!(snapshot.values.context_remaining_percentage, Some(58));
        assert_eq!(snapshot.values.context_total_tokens, Some(420));
        assert_eq!(snapshot.values.context_window_size, Some(1000));
        assert_eq!(snapshot.values.cost_total_cost_usd, Some(0.5));
        assert_eq!(snapshot.mode, "manual");
        assert_eq!(snapshot.effort.as_deref(), Some("high"));
        assert_eq!(snapshot.goal, Some((240, false)));
        let unknown = super::snapshot(
            &App::new(PermissionMode::Manual, "m".into()),
            &SnapshotData::default(),
        );
        assert_eq!(unknown.values, Values::default());
    }

    #[test]
    fn the_context_shows_once_the_window_is_known_and_the_cost_once_a_call_was_priced() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        let text = bar(&app);
        assert!(!text.contains("ctx") && !text.contains('$'), "{text}");
        app.context = Some(crate::app::ContextFigures {
            used: 500,
            window: None,
        });
        assert!(!bar(&app).contains("ctx"), "{}", bar(&app));
        app.context = Some(crate::app::ContextFigures {
            used: 420,
            window: Some(1000),
        });
        let text = bar(&app);
        assert!(text.contains("· m · ctx 42% · shift+tab mode"), "{text}");
        let priced = SnapshotData {
            cost_total_usd: Some(0.1234),
            ..SnapshotData::default()
        };
        let text = bar_with(&app, &priced);
        assert!(
            text.contains("· ctx 42% · $0.1234 · shift+tab mode"),
            "{text}"
        );
        assert!(!text.contains("est."), "{text}");
        let estimated = SnapshotData {
            cost_estimated: true,
            ..priced
        };
        let text = bar_with(&app, &estimated);
        assert!(text.contains("· $0.1234 est. · shift+tab mode"), "{text}");
        app.context = None;
        let text = bar_with(&app, &estimated);
        assert!(text.contains("· m · $0.1234 est. · "), "{text}");
        assert!(!text.contains("ctx"), "{text}");
    }

    #[test]
    fn the_default_bar_reads_like_the_hand_built_one() {
        let app = populated();
        let text = text_of(&draw_row(&app, &default_runs(&app), 250));
        assert!(
            text.starts_with(
                " manual vim:insert · /work/proj (main) · m · kimi · 5h 40% · guardian kimi · 5h 80% · effort high · 1 agent running · 1 shell running · ⏱ 2 · 3 bug draft(s): /bug · ◎ goal 4m · attached · ctx 42% · CMD"
            ),
            "{text}"
        );
        let idle = bar(&App::new(PermissionMode::Manual, "m".into()));
        assert!(
            idle.starts_with(" manual · m · shift+tab mode · /add-dir"),
            "{idle}"
        );
    }

    #[test]
    fn the_quota_is_warning_coloured_past_ninety_percent_and_dim_below() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.usage = vec![view(vec![window("5h", 0.5, None)])];
        let buffer = draw_row(&app, &default_runs(&app), 120);
        let text = text_of(&buffer);
        let at = text.find("kimi").expect("the quota");
        let col = text[..at].chars().count() as u16;
        assert_eq!(buffer[(col, 0)].fg, ratatui::style::Color::Reset);
        assert!(buffer[(col, 0)].modifier.contains(Modifier::DIM));
        app.usage = vec![view(vec![window("5h", 0.95, None)])];
        let buffer = draw_row(&app, &default_runs(&app), 120);
        assert_eq!(buffer[(col, 0)].fg, app.theme.warning);
    }

    #[test]
    fn the_mode_chip_is_bold_in_the_mode_colour_and_the_vim_badge_the_accent() {
        let mut app = App::new(PermissionMode::Auto, "m".into());
        app.input.mode = fontaine_sdk::core::config::EditorMode::Vim;
        let buffer = draw_row(&app, &default_runs(&app), 120);
        assert_eq!(buffer[(1, 0)].fg, app.theme.mode_auto);
        assert!(buffer[(1, 0)].modifier.contains(Modifier::BOLD));
        let text = text_of(&buffer);
        let at = text.find("vim:").expect("the vim badge");
        let col = text[..at].chars().count() as u16;
        assert_eq!(buffer[(col, 0)].fg, app.theme.accent);
    }

    #[test]
    fn a_script_may_drop_a_segment_reorder_two_and_style_one() {
        let app = populated();
        let source = r#"pub fn statusline(ctx) {
            let line = ctx.line();
            line.push(ctx.format("{model} · {mode}"));
            let style = if ctx.context_used_percentage().unwrap() >= 40 {
                ctx.style("warning")
            } else {
                ctx.style("dim")
            };
            line.push_styled(" ctx!", style);
            line
        }"#;
        let runs = script_runs(&app, source);
        let buffer = draw_row(&app, &runs, 120);
        let text = text_of(&buffer);
        assert!(text.starts_with("m · manual ctx!"), "{text}");
        assert!(
            !text.contains("/work/proj") && !text.contains("kimi"),
            "{text}"
        );
        let at = text.find("ctx!").unwrap();
        assert_eq!(buffer[(at as u16, 0)].fg, app.theme.warning);
    }

    #[test]
    fn the_two_badges_come_before_any_script_and_the_row_clips_on_the_right() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.guardian_off = true;
        app.afk_stopped = true;
        let text = scripted(&app, r#"pub fn statusline(ctx) { "SCRIPT" }"#, 120);
        assert!(
            text.starts_with(" ⚠ guardian off  AFK STOPPED SCRIPT"),
            "{text}"
        );
        let long = scripted(
            &app,
            &format!(r#"pub fn statusline(ctx) {{ "{}" }}"#, "x".repeat(200)),
            40,
        );
        assert!(long.starts_with(" ⚠ guardian off  AFK STOPPED x"), "{long}");
        assert!(!long.contains('…'), "{long}");
        assert_eq!(long.chars().count(), 40);
    }

    #[test]
    fn a_row_with_no_cached_line_draws_only_the_badges() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        let draw_cache = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
            terminal.draw(|f| render(f, f.area(), app)).unwrap();
            text_of(terminal.backend().buffer())
        };
        assert_eq!(draw_cache(&app).trim(), "");
        app.guardian_off = true;
        assert_eq!(draw_cache(&app).trim(), "⚠ guardian off");
        render_default(&mut app);
        assert!(draw_cache(&app).contains("manual"), "{}", draw_cache(&app));
    }

    #[test]
    fn the_command_run_is_cut_to_what_the_other_runs_leave() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.command_line = Some("0123456789".repeat(10));
        app.guardian_off = true;
        let text = scripted(
            &app,
            r#"pub fn statusline(ctx) { ctx.format("{command} · {model}") }"#,
            50,
        );
        // The badge, the separator and the model keep their place; the command keeps its end.
        assert!(text.starts_with(" ⚠ guardian off …"), "{text}");
        assert!(text.ends_with("789 · m"), "{text}");
        assert_eq!(text.chars().count(), 50);
    }

    #[test]
    fn the_default_program_renders_a_fully_populated_bar_within_its_budget() {
        let mut app = populated();
        app.running = true;
        app.guardian_off = true;
        app.afk_stopped = true;
        let runs = render_program(default_program(), &snapshot(&app, &data()));
        assert!(runs.is_ok(), "{runs:?}");
    }

    #[test]
    fn a_role_maps_to_one_style_and_the_modes_to_their_colours() {
        let theme = Theme::default();
        assert_eq!(role_style(&theme, Role::Normal), Style::default());
        assert_eq!(
            role_style(&theme, Role::Dim),
            Style::default().add_modifier(Modifier::DIM)
        );
        assert_eq!(
            role_style(&theme, Role::Warning),
            Style::default().fg(theme.warning)
        );
        assert_eq!(
            role_style(&theme, mode_role(PermissionMode::DontAsk)),
            Style::default()
                .fg(theme.mode_manual)
                .add_modifier(Modifier::BOLD)
        );
        for (name, role) in fontaine_sdk::core::statusline::STYLE_NAMES {
            let _ = role_style(&theme, *role);
            assert!(!name.is_empty());
        }
    }
}
