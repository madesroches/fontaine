//! The working indicator: one animated row above the message box while the shown conversation
//! is working. Pure functions of `App`, which carries the frame and the turn clock, so nothing
//! here reads a clock.

use fontaine_core::turn::SessionEvent;
use fontaine_tools::display::compact_count;
use fontaine_tools::jobs::format_age;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::Duration;

use crate::app::App;
use crate::theme::{Glyphs, Theme};

/// Milliseconds per animation frame, also the redraw tick while the spinner shows.
pub const FRAME_MS: u64 = 80;

const LABEL: &str = "Working…";

/// A full turn of the glyph's hue.
const HUE_PERIOD_MS: u64 = 4_000;
/// How long a named-colour glyph stays on one accent.
const ACCENT_STEP_FRAMES: u64 = 9;
/// Frames the highlight takes to move one label column.
const SWEEP_FRAMES: u64 = 2;
/// Columns of rest between one sweep leaving the label and the next entering it.
const SWEEP_GAP: usize = 8;
/// Columns either side of the highlight that it still reaches.
const SWEEP_RADIUS: usize = 4;
/// Characters of streamed text that stand for a token before the provider reports the count.
const CHARS_PER_TOKEN: usize = 4;

/// The output tokens of the main conversation's current turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnTokens {
    settled: u64,
    inflight_chars: usize,
}

impl TurnTokens {
    pub fn observe(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::AssistantDelta(text) | SessionEvent::ReasoningDelta(text) => {
                self.inflight_chars += text.chars().count();
            }
            SessionEvent::Usage { usage, .. } => {
                self.settled += usage
                    .output_tokens
                    .unwrap_or((self.inflight_chars / CHARS_PER_TOKEN) as u64);
                self.inflight_chars = 0;
            }
            _ => {}
        }
    }

    /// What the row shows: the settled calls plus an estimate for the one still streaming.
    pub fn shown(&self) -> u64 {
        self.settled + (self.inflight_chars / CHARS_PER_TOKEN) as u64
    }
}

/// Whether the shown conversation is working with nothing waiting on the human. Reads the
/// prompt fields directly rather than `overlay()`, whose body sizing calls `lines`.
pub fn active(app: &App) -> bool {
    if app.linear
        || app.approval.is_some()
        || !app.questions.is_empty()
        || app.unavailable.is_some()
    {
        return false;
    }
    if app.viewing.is_some() {
        app.viewed_row()
            .and_then(|row| row.status)
            .is_some_and(|status| status.is_running())
    } else {
        app.running
    }
}

pub fn lines(app: &App, width: usize) -> Vec<Line<'static>> {
    if !active(app) || width == 0 {
        return Vec::new();
    }
    let theme = &app.theme;
    let frame = app.spinner_frame;
    let mut cells: Vec<(char, Style)> = Vec::new();
    for c in glyph(frame, app.transcript.glyphs).chars() {
        cells.push((c, glyph_style(theme, frame)));
    }
    cells.push((' ', Style::default()));
    let label_len = LABEL.chars().count();
    for (i, c) in LABEL.chars().enumerate() {
        cells.push((c, shimmer_style(theme, frame, i, label_len)));
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    for c in suffix(app).chars() {
        cells.push((c, dim));
    }
    cells.truncate(width);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = None;
    for (c, style) in cells {
        if run_style.is_some_and(|s| s != style) {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                run_style.take().expect("a run has a style"),
            ));
        }
        run_style = Some(style);
        run.push(c);
    }
    if let Some(style) = run_style {
        spans.push(Span::styled(run, style));
    }
    vec![Line::from(spans)]
}

/// ` (4m12s · ↓ 1.2k tokens)` with only the clauses that exist, or nothing.
fn suffix(app: &App) -> String {
    let (elapsed, tokens) = if app.viewing.is_some() {
        (app.viewed_row().map(|row| row.elapsed), 0)
    } else {
        (app.turn_elapsed, app.turn_tokens.shown())
    };
    let mut parts = Vec::new();
    if let Some(elapsed) = elapsed {
        parts.push(format_age(elapsed));
    }
    if tokens > 0 {
        parts.push(format!("↓ {} tokens", compact_count(tokens)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(" · "))
    }
}

pub fn glyph(frame: u64, glyphs: Glyphs) -> &'static str {
    let frames = glyphs.spinner;
    frames[(frame % frames.len() as u64) as usize]
}

/// The accent fields of the theme, in order, without repeats: the colours a 16-colour preset
/// cycles through.
fn accents(theme: &Theme) -> Vec<Color> {
    let mut out: Vec<Color> = Vec::new();
    for color in [
        theme.user,
        theme.success,
        theme.heading,
        theme.mode_plan,
        theme.shell,
    ] {
        if !out.contains(&color) {
            out.push(color);
        }
    }
    out
}

pub fn glyph_style(theme: &Theme, frame: u64) -> Style {
    let color = match theme.spinner {
        Color::Rgb(r, g, b) => {
            let (hue, saturation, lightness) = rgb_to_hsl(r, g, b);
            let turn = (frame * FRAME_MS % HUE_PERIOD_MS) as f32 / HUE_PERIOD_MS as f32;
            let (r, g, b) = hsl_to_rgb((hue + turn * 360.0) % 360.0, saturation, lightness);
            Color::Rgb(r, g, b)
        }
        _ => {
            let accents = accents(theme);
            accents[(frame / ACCENT_STEP_FRAMES % accents.len() as u64) as usize]
        }
    };
    Style::default().fg(color)
}

pub fn shimmer_style(theme: &Theme, frame: u64, index: usize, len: usize) -> Style {
    let period = len + SWEEP_GAP;
    let center = (frame / SWEEP_FRAMES) as usize % period;
    let distance = index.abs_diff(center);
    if let (Color::Rgb(pr, pg, pb), Color::Rgb(fr, fg, fb)) = (theme.spinner, theme.mode_manual) {
        let near = SWEEP_RADIUS.saturating_sub(distance) as f32 / SWEEP_RADIUS as f32;
        let mix =
            |far: u8, peak: u8| (far as f32 + (peak as f32 - far as f32) * near).round() as u8;
        return Style::default().fg(Color::Rgb(mix(fr, pr), mix(fg, pg), mix(fb, pb)));
    }
    let style = Style::default().fg(theme.spinner);
    match distance {
        0..=1 => style.add_modifier(Modifier::BOLD),
        2..=3 => style,
        _ => style.add_modifier(Modifier::DIM),
    }
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta == 0.0 {
        return (0.0, 0.0, lightness);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    (hue * 60.0, saturation, lightness)
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    let byte = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (byte(r), byte(g), byte(b))
}

/// The tick the redraw loops wait on while the spinner shows.
pub fn tick(app: &App) -> Option<Duration> {
    active(app).then(|| Duration::from_millis(FRAME_MS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_list::ListRow;
    use fontaine_agents::runner::ChildStatus;
    use fontaine_core::config::{PermissionMode, ThemeName};
    use rig_core::completion::Usage;

    fn running_app() -> App {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.running = true;
        app
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn usage(output: Option<u64>) -> SessionEvent {
        let mut usage = Usage::default();
        usage.output_tokens = output;
        SessionEvent::Usage {
            usage,
            calls_tool: false,
        }
    }

    fn delta(chars: usize) -> SessionEvent {
        SessionEvent::AssistantDelta("x".repeat(chars))
    }

    fn rgb_theme() -> Theme {
        Theme::preset(ThemeName::DarkDaltonized)
    }

    fn fg(style: Style) -> Color {
        style.fg.expect("a foreground")
    }

    #[test]
    fn the_sweep_peaks_at_the_centre_and_rests_at_the_far_end() {
        let theme = rgb_theme();
        // Frame 6 puts the centre on column 3 of an eight-column label.
        assert_eq!(fg(shimmer_style(&theme, 6, 3, 8)), theme.spinner);
        assert_eq!(fg(shimmer_style(&theme, 6, 7, 8)), theme.mode_manual);
        let mid = fg(shimmer_style(&theme, 6, 5, 8));
        assert_ne!(mid, theme.spinner);
        assert_ne!(mid, theme.mode_manual);
    }

    #[test]
    fn the_sweep_wraps_after_the_label_and_its_gap() {
        let theme = rgb_theme();
        let period = (8 + SWEEP_GAP) as u64;
        for index in 0..8 {
            assert_eq!(
                shimmer_style(&theme, 4, index, 8),
                shimmer_style(&theme, 4 + period * SWEEP_FRAMES, index, 8)
            );
        }
        // In the gap no column is lit.
        let gap_frame = 9 * SWEEP_FRAMES + 4 * SWEEP_FRAMES;
        for index in 0..8 {
            assert_eq!(
                fg(shimmer_style(&theme, gap_frame, index, 8)),
                theme.mode_manual
            );
        }
    }

    #[test]
    fn a_named_colour_steps_through_modifiers() {
        let theme = Theme::default();
        let at = |index| shimmer_style(&theme, 0, index, 8);
        assert!(at(0).add_modifier.contains(Modifier::BOLD));
        assert_eq!(at(2).add_modifier, Modifier::empty());
        assert!(at(6).add_modifier.contains(Modifier::DIM));
        assert_eq!(at(0).fg, Some(theme.spinner));
    }

    #[test]
    fn a_zero_length_label_does_not_panic() {
        for theme in [Theme::default(), rgb_theme()] {
            shimmer_style(&theme, 7, 0, 0);
        }
    }

    #[test]
    fn the_estimate_grows_with_deltas_and_rounds_down() {
        let mut tokens = TurnTokens::default();
        tokens.observe(&delta(3));
        assert_eq!(tokens.shown(), 0);
        tokens.observe(&delta(5));
        assert_eq!(tokens.shown(), 2);
        tokens.observe(&SessionEvent::ReasoningDelta("y".repeat(4)));
        assert_eq!(tokens.shown(), 3);
    }

    #[test]
    fn usage_replaces_the_estimate_and_calls_sum() {
        let mut tokens = TurnTokens::default();
        tokens.observe(&delta(400));
        assert_eq!(tokens.shown(), 100);
        tokens.observe(&usage(Some(80)));
        assert_eq!(tokens.shown(), 80);
        tokens.observe(&delta(40));
        tokens.observe(&usage(Some(5)));
        assert_eq!(tokens.shown(), 85);
    }

    #[test]
    fn a_call_without_a_reported_count_settles_on_its_estimate() {
        let mut tokens = TurnTokens::default();
        tokens.observe(&delta(120));
        tokens.observe(&usage(None));
        assert_eq!(tokens.shown(), 30);
        tokens.observe(&usage(None));
        assert_eq!(tokens.shown(), 30);
    }

    #[test]
    fn a_new_turn_starts_from_zero_and_the_row_hides_the_clause() {
        let mut app = running_app();
        app.turn_tokens.observe(&usage(Some(900)));
        assert!(text(&lines(&app, 80)[0]).contains("↓ 900 tokens"));
        app.set_running(false);
        app.set_running(true);
        assert_eq!(app.turn_tokens, TurnTokens::default());
        assert!(!text(&lines(&app, 80)[0]).contains('↓'));
    }

    #[test]
    fn the_rgb_hue_advances_and_wraps_after_a_full_turn() {
        let theme = rgb_theme();
        let frames_per_turn = HUE_PERIOD_MS / FRAME_MS;
        assert_ne!(glyph_style(&theme, 0), glyph_style(&theme, 5));
        assert_eq!(
            glyph_style(&theme, 3),
            glyph_style(&theme, 3 + frames_per_turn)
        );
        assert_eq!(fg(glyph_style(&theme, 0)), theme.spinner);
    }

    #[test]
    fn a_named_colour_cycles_the_deduped_accents() {
        for (name, hues) in [(ThemeName::DarkAnsi, 4), (ThemeName::LightAnsi, 3)] {
            let theme = Theme::preset(name);
            assert_eq!(accents(&theme).len(), hues, "{name:?}");
            let seen: Vec<Color> = (0..hues as u64)
                .map(|i| fg(glyph_style(&theme, i * ACCENT_STEP_FRAMES)))
                .collect();
            for (i, color) in seen.iter().enumerate() {
                assert!(!seen[..i].contains(color), "{name:?} repeats {color:?}");
            }
            assert_eq!(
                glyph_style(&theme, 0),
                glyph_style(&theme, hues as u64 * ACCENT_STEP_FRAMES)
            );
        }
    }

    #[test]
    fn every_frame_is_one_column_and_the_set_cycles() {
        for glyphs in [Glyphs::UNICODE, Glyphs::ASCII] {
            let n = glyphs.spinner.len() as u64;
            for frame in 0..n {
                assert_eq!(Line::from(glyph(frame, glyphs)).width(), 1, "{frame}");
                assert_eq!(glyph(frame, glyphs), glyph(frame + n, glyphs));
            }
            let all: std::collections::HashSet<_> = (0..n).map(|f| glyph(f, glyphs)).collect();
            assert_eq!(all.len(), 4);
        }
        assert_eq!(Glyphs::from_env(Some("1")).spinner.concat(), ".oO0Oo");
        assert_eq!(Glyphs::UNICODE.spinner.concat(), "·∙•●•∙");
    }

    fn row(status: Option<ChildStatus>, elapsed: u64) -> ListRow {
        ListRow {
            id: Some("agent-1".into()),
            name: "worker-1".into(),
            depth: 1,
            agent_type: "worker".into(),
            status,
            elapsed: Duration::from_secs(elapsed),
            spend: String::new(),
            writing: 0,
        }
    }

    #[test]
    fn running_shows_one_row_and_idle_none() {
        let mut app = running_app();
        assert_eq!(lines(&app, 80).len(), 1);
        assert!(text(&lines(&app, 80)[0]).contains("Working…"));
        app.running = false;
        assert!(lines(&app, 80).is_empty());
    }

    #[test]
    fn the_clauses_show_only_when_present() {
        let mut app = running_app();
        assert_eq!(text(&lines(&app, 80)[0]), "· Working…");
        app.turn_elapsed = Some(Duration::from_secs(252));
        assert_eq!(text(&lines(&app, 80)[0]), "· Working… (4m12s)");
        app.turn_tokens.observe(&usage(Some(1_234)));
        assert_eq!(
            text(&lines(&app, 80)[0]),
            "· Working… (4m12s · ↓ 1.2k tokens)"
        );
        app.turn_elapsed = None;
        assert_eq!(text(&lines(&app, 80)[0]), "· Working… (↓ 1.2k tokens)");
    }

    #[test]
    fn the_row_truncates_to_the_width() {
        let mut app = running_app();
        app.turn_elapsed = Some(Duration::from_secs(5));
        assert_eq!(text(&lines(&app, 6)[0]), "· Work");
        assert!(lines(&app, 0).is_empty());
    }

    #[test]
    fn prompts_that_wait_on_the_human_hide_it_and_a_picker_does_not() {
        let mut app = running_app();
        let (reply, _choice) = tokio::sync::oneshot::channel();
        let info = fontaine_core::permissions::GuardianUnavailable {
            model: "a/b".into(),
            reason: "no key".into(),
            tool: "shell".into(),
        };
        app.unavailable = Some(crate::denials::UnavailablePrompt::new(
            info,
            Vec::new(),
            reply,
        ));
        assert!(!active(&app));
        app.unavailable = None;
        app.resume = Some(crate::resume_picker::ResumePicker::new(
            Vec::new(),
            std::time::SystemTime::now(),
        ));
        assert!(active(&app));
    }

    #[test]
    fn the_screen_reader_view_has_no_spinner() {
        let mut app = running_app();
        app.linear = true;
        assert!(!active(&app));
    }

    #[test]
    fn an_agent_view_follows_the_agents_status() {
        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.viewing = Some("agent-1".into());
        app.agent_list
            .set_rows(vec![row(Some(ChildStatus::Running), 37)]);
        app.turn_tokens.observe(&usage(Some(900)));
        assert_eq!(text(&lines(&app, 80)[0]), "· Working… (37s)");
        app.agent_list
            .set_rows(vec![row(Some(ChildStatus::Done), 37)]);
        assert!(lines(&app, 80).is_empty());
        app.agent_list.set_rows(Vec::new());
        assert!(lines(&app, 80).is_empty());
    }

    #[test]
    fn the_tick_exists_only_while_active() {
        let mut app = running_app();
        assert_eq!(tick(&app), Some(Duration::from_millis(FRAME_MS)));
        app.running = false;
        assert_eq!(tick(&app), None);
    }
}
