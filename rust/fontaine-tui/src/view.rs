//! Draws the interface into a frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::input::{InputBuffer, wrap};
use crate::model_picker::PickerPurpose;
use crate::statusbar;

const MAX_INPUT_ROWS: usize = 6;

/// The wrapped rows of the message box and where the cursor sits in them.
struct InputView {
    rows: Vec<String>,
    cursor: (usize, usize),
}

fn input_view(buffer: &InputBuffer, width: usize) -> InputView {
    let width = width.max(1);
    let (cursor_line, cursor_col) = buffer.cursor_position();
    let mut rows = Vec::new();
    let mut cursor_row = 0;
    for (i, line) in buffer.lines().iter().enumerate() {
        if i == cursor_line {
            cursor_row = rows.len() + cursor_col / width;
        }
        rows.extend(wrap(line, width));
    }
    // A cursor at the end of a full row sits on the row after it.
    while rows.len() < cursor_row + 1 {
        rows.push(String::new());
    }
    let first = (cursor_row + 1).saturating_sub(MAX_INPUT_ROWS);
    let visible: Vec<String> = rows.into_iter().skip(first).take(MAX_INPUT_ROWS).collect();
    InputView {
        rows: visible,
        cursor: (cursor_col % width, cursor_row - first),
    }
}

/// The rows the live area needs for the next frame, at most `max_height`.
pub fn height(app: &App, width: u16, max_height: u16) -> u16 {
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let Overlay { body, footer, .. } = overlay(app, inner_width, max_height);
    let overlay_rows = if body.is_empty() {
        0
    } else {
        body.len() + footer.len() + 2
    };
    let overlay_rows = overlay_rows + queued_lines(app, inner_width).len();
    let input_rows = input_view(&app.input, inner_width).rows.len() + 2;
    let live = app
        .transcript
        .live_lines(usize::from(width).max(1), &app.theme)
        .len();
    let total = live + overlay_rows + input_rows + 1;
    u16::try_from(total).unwrap_or(u16::MAX).min(max_height)
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let inner_width = usize::from(area.width.saturating_sub(2)).max(1);
    let Overlay {
        title,
        body,
        footer,
        approval: is_approval,
        ..
    } = overlay(app, inner_width, area.height);
    let view = input_view(&app.input, inner_width);
    let input_height = view.rows.len() as u16 + 2;
    let queued = queued_lines(app, inner_width);
    let approval_height = if body.is_empty() {
        0
    } else {
        ((body.len() + footer.len()) as u16 + 2).min(
            area.height
                .saturating_sub(input_height + queued.len() as u16 + 1),
        )
    };
    let [transcript, approval, queued_area, input, status] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(approval_height),
        Constraint::Length(queued.len() as u16),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, transcript, app);
    if approval_height > 0 {
        if is_approval {
            draw_approval(frame, approval, app, body, footer);
        } else {
            draw_overlay(frame, approval, title, body, footer);
        }
    }
    frame.render_widget(Paragraph::new(queued), queued_area);
    draw_input(frame, input, app, &view);
    statusbar::render(frame, status, app);
}

/// The messages waiting for the agent's next tool result, one dimmed row each.
fn queued_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    if app.queued.is_empty() {
        return Vec::new();
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut lines: Vec<Line<'static>> = app
        .queued
        .iter()
        .map(|message| {
            let first = message.lines().next().unwrap_or("");
            let shown: String = format!("queued: {first}").chars().take(width).collect();
            Line::styled(shown, dim)
        })
        .collect();
    lines.push(Line::styled("  ↑ to edit", dim));
    lines
}

/// Rows a dialog body may use so its header and the selection stay inside what `draw` allows.
fn body_rows(app: &App, width: usize, height: u16, footer_len: usize) -> usize {
    let reserved = input_view(&app.input, width).rows.len()
        + 2
        + queued_lines(app, width).len()
        + 1
        + 2
        + footer_len
        + 1;
    usize::from(height).saturating_sub(reserved).max(1)
}

/// What sits above the message box.
pub(crate) struct Overlay {
    pub title: &'static str,
    pub body: Vec<Line<'static>>,
    pub footer: Vec<Line<'static>>,
    /// The approval prompt is the overlay, so it needs the scrolling and cursor handling only it
    /// has.
    pub approval: bool,
    /// Tells two overlays with the same title apart, for a renderer that announces when one
    /// opens.
    pub instance: Option<u64>,
    /// A text field while it is open (the approval's deny reason, a provider dialog input): its prompt and the text typed.
    pub field: Option<(&'static str, String)>,
}

impl Overlay {
    fn new(title: &'static str, body: Vec<Line<'static>>, footer: Vec<Line<'static>>) -> Overlay {
        Overlay {
            title,
            body,
            footer,
            approval: false,
            instance: None,
            field: None,
        }
    }
}

/// What sits above the message box, if anything: the denied list, the unavailable-guardian
/// prompt, the MCP server prompt, the model picker, the provider dialog, the agents panel, the approval prompt, the
/// history search or the file completion list, then the `/` menu, in that order of precedence,
/// which is the order `App::handle_key` routes keys in.
pub(crate) fn overlay(app: &App, width: usize, height: u16) -> Overlay {
    let theme = &app.theme;
    if let Some(list) = &app.denied {
        return Overlay::new("denied", list.body(width, theme), list.footer());
    }
    if let Some(prompt) = &app.unavailable {
        return Overlay::new(
            "guardian unavailable",
            prompt.body(width, theme),
            prompt.footer(),
        );
    }
    if let Some(prompt) = &app.server {
        return Overlay::new("MCP server", prompt.body(width, theme), prompt.footer());
    }
    if let Some(picker) = &app.models {
        let title = match picker.purpose() {
            PickerPurpose::Main => "model",
            PickerPurpose::Guardian => "guardian model",
        };
        let footer = picker.footer();
        let rows = body_rows(app, width, height, footer.len());
        return Overlay::new(title, picker.body(width, rows, theme), footer);
    }
    if let Some(dialog) = &app.providers {
        let footer = dialog.footer();
        let rows = body_rows(app, width, height, footer.len());
        return Overlay {
            field: dialog.field(),
            ..Overlay::new("providers", dialog.body(width, rows, theme), footer)
        };
    }
    if let Some(panel) = &app.agents {
        return Overlay::new("agents", panel.body(width, theme), panel.footer());
    }
    if let Some(approval) = &app.approval {
        return Overlay {
            approval: true,
            instance: Some(approval.id),
            field: approval.field(),
            ..Overlay::new(
                "approval",
                approval.body(width, theme),
                approval.footer(width),
            )
        };
    }
    if let Some(search) = &app.search {
        let found = app.search_match().map_or_else(
            || Line::styled("no match", Style::default().add_modifier(Modifier::DIM)),
            |text| Line::raw(text.lines().next().unwrap_or("").to_string()),
        );
        return Overlay::new(
            "history search",
            vec![
                Line::raw(format!("history search: {}", search.query)),
                found,
            ],
            vec![Line::styled(
                "Enter/Tab use · Ctrl-R older · Esc cancel",
                Style::default().add_modifier(Modifier::DIM),
            )],
        );
    }
    if let Some(list) = &app.completion {
        let body = list
            .matches
            .iter()
            .enumerate()
            .map(|(i, path)| {
                if i == list.selected {
                    Line::styled(
                        format!("> {path}"),
                        Style::default().add_modifier(Modifier::BOLD),
                    )
                } else {
                    Line::raw(format!("  {path}"))
                }
            })
            .collect();
        return Overlay::new(
            "files",
            body,
            vec![Line::styled(
                "↑/↓ select · Tab/Enter complete · Esc close",
                Style::default().add_modifier(Modifier::DIM),
            )],
        );
    }
    let menu = menu_lines(app, width);
    if !menu.is_empty() {
        return Overlay::new("commands", menu, Vec::new());
    }
    Overlay::new("", Vec::new(), Vec::new())
}

/// The `/` menu: the commands and skills the text typed so far could be. Shown while the
/// first word of the message is a bare `/name`.
fn menu_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let text = app.input.text();
    let Some(typed) = text.strip_prefix('/') else {
        return Vec::new();
    };
    if typed.contains(char::is_whitespace) {
        return Vec::new();
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    app.menu
        .iter()
        .filter(|entry| entry.name.starts_with(typed))
        .map(|entry| {
            let hint = entry
                .hint
                .as_deref()
                .map_or(String::new(), |h| format!(" {h}"));
            let head = format!("/{}{hint}", entry.name);
            let room = width.saturating_sub(head.chars().count() + 3);
            let description: String = entry.description.chars().take(room).collect();
            Line::from(vec![
                ratatui::text::Span::styled(head, bold),
                ratatui::text::Span::styled(format!("  {description}"), dim),
            ])
        })
        .collect()
}

pub(crate) fn draw_overlay(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    body: Vec<Line<'static>>,
    footer: Vec<Line<'static>>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    let [body_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer.len() as u16)])
            .areas(inner);
    frame.render_widget(block, area);
    // A long list shows its tail, where the newest entry and the selection start out.
    let hidden = body.len().saturating_sub(usize::from(body_area.height));
    frame.render_widget(Paragraph::new(body).scroll((hidden as u16, 0)), body_area);
    frame.render_widget(Paragraph::new(footer), footer_area);
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &App) {
    let lines = app
        .transcript
        .live_lines(usize::from(area.width).max(1), &app.theme);
    let start = lines.len().saturating_sub(usize::from(area.height));
    frame.render_widget(Paragraph::new(lines[start..].to_vec()), area);
}

fn draw_approval(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    body: Vec<Line<'static>>,
    footer: Vec<Line<'static>>,
) {
    let Some(approval) = app.approval.as_ref() else {
        return;
    };
    let mut block = Block::default().borders(Borders::ALL).title(" approval ");
    let inner = block.inner(area);
    let [body_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer.len() as u16)])
            .areas(inner);
    let max_scroll = body.len().saturating_sub(usize::from(body_area.height));
    approval.set_max_scroll(max_scroll);
    if max_scroll > 0 {
        block = block.title(" approval · PgUp/PgDn scroll the call ");
    }
    frame.render_widget(block, area);
    let offset = approval.scroll().min(max_scroll) as u16;
    frame.render_widget(Paragraph::new(body).scroll((offset, 0)), body_area);
    let count = footer.len();
    frame.render_widget(Paragraph::new(footer), footer_area);
    if let Some(x) = approval.feedback_cursor() {
        let row = count
            .saturating_sub(1)
            .min(usize::from(footer_area.height.saturating_sub(1)));
        let x = x.min(usize::from(footer_area.width.saturating_sub(1)));
        frame.set_cursor_position(Position::new(
            footer_area.x + x as u16,
            footer_area.y + row as u16,
        ));
    }
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App, view: &InputView) {
    let covered = app.covered() || app.search.is_some();
    let style = if covered {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    let (title, style) = if app.shell_mode() {
        (
            " shell · Enter runs · output goes to the agent with your next message ",
            style.fg(app.theme.shell),
        )
    } else {
        (" message · Enter sends · Alt+Enter new line ", style)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = view.rows.iter().map(|r| Line::raw(r.clone())).collect();
    frame.render_widget(Paragraph::new(lines), inner);
    if !covered {
        frame.set_cursor_position(Position::new(
            inner.x + view.cursor.0 as u16,
            inner.y + view.cursor.1 as u16,
        ));
    }
}
