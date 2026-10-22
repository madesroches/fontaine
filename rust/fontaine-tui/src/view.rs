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

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let inner_width = usize::from(area.width.saturating_sub(2)).max(1);
    let approval_lines = app
        .approval
        .as_ref()
        .map(|a| a.lines(inner_width))
        .unwrap_or_default();
    let approval_height = if approval_lines.is_empty() {
        0
    } else {
        (approval_lines.len() as u16 + 2).min(area.height.saturating_sub(4))
    };
    let view = input_view(&app.input, inner_width);
    let input_height = view.rows.len() as u16 + 2;
    let [transcript, approval, input, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(approval_height),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, transcript, app);
    if approval_height > 0 {
        draw_approval(frame, approval, app, approval_lines);
    }
    draw_input(frame, input, app, &view);
    statusbar::render(frame, status, app);
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &App) {
    let lines = app.transcript.lines(usize::from(area.width).max(1));
    let height = usize::from(area.height);
    let max_scroll = lines.len().saturating_sub(height);
    let end = lines.len() - app.scroll.min(max_scroll);
    let start = end.saturating_sub(height);
    let visible: Vec<Line> = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(visible), area);
}

fn draw_approval(frame: &mut Frame, area: Rect, app: &App, lines: Vec<Line<'static>>) {
    let block = Block::default().borders(Borders::ALL).title(" approval ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let count = lines.len();
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some(x) = app.approval.as_ref().and_then(|a| a.feedback_cursor()) {
        let row = count
            .saturating_sub(1)
            .min(usize::from(inner.height.saturating_sub(1)));
        let x = x.min(usize::from(inner.width.saturating_sub(1)));
        frame.set_cursor_position(Position::new(inner.x + x as u16, inner.y + row as u16));
    }
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App, view: &InputView) {
    let covered = app.approval.is_some();
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
