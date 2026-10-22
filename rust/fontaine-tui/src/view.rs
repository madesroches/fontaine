//! Draws the interface into a frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::input::{InputBuffer, wrap};
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
    let (_, body, footer) = overlay(app, inner_width);
    let overlay_rows = if body.is_empty() {
        0
    } else {
        body.len() + footer.len() + 2
    };
    let input_rows = input_view(&app.input, inner_width).rows.len() + 2;
    let live = app.transcript.live_lines(usize::from(width).max(1)).len();
    let total = live + overlay_rows + input_rows + 1;
    u16::try_from(total).unwrap_or(u16::MAX).min(max_height)
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let inner_width = usize::from(area.width.saturating_sub(2)).max(1);
    let (title, body, footer) = overlay(app, inner_width);
    let view = input_view(&app.input, inner_width);
    let input_height = view.rows.len() as u16 + 2;
    let approval_height = if body.is_empty() {
        0
    } else {
        ((body.len() + footer.len()) as u16 + 2).min(area.height.saturating_sub(input_height + 1))
    };
    let [transcript, approval, input, status] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(approval_height),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, transcript, app);
    if approval_height > 0 {
        if app.approval.is_some() {
            draw_approval(frame, approval, app, body, footer);
        } else {
            draw_overlay(frame, approval, title, body, footer);
        }
    }
    draw_input(frame, input, app, &view);
    statusbar::render(frame, status, app);
}

/// What sits above the message box, if anything: the approval prompt, the denied list or the
/// unavailable-guardian prompt, in that order of precedence.
fn overlay(app: &App, width: usize) -> (&'static str, Vec<Line<'static>>, Vec<Line<'static>>) {
    if let Some(approval) = &app.approval {
        return ("approval", approval.body(width), approval.footer(width));
    }
    if let Some(prompt) = &app.unavailable {
        return ("guardian unavailable", prompt.body(width), prompt.footer());
    }
    if let Some(list) = &app.denied {
        return ("denied", list.body(width), list.footer());
    }
    ("", Vec::new(), Vec::new())
}

fn draw_overlay(
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
    let lines = app.transcript.live_lines(usize::from(area.width).max(1));
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
    let covered = app.approval.is_some() || app.denied.is_some() || app.unavailable.is_some();
    let style = if covered {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" message · Enter sends · Alt+Enter new line ")
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
