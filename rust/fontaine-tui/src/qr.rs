//! Draws a payload as a QR code out of half-block cells, for a phone to scan off the terminal.

use qrcode::{Color as Module, EcLevel, QrCode};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Modules of light margin on each side. The spec asks for four; two scans reliably against the
/// light frame drawn here and halves what the margin costs in rows.
const QUIET_ZONE: usize = 2;

/// The code as text rows, or `None` when it needs more than `max_width` columns or `max_height`
/// rows. Each row carries two module rows as half blocks, drawn as fixed black on white and never
/// in theme colors: a theme-inverted code fails on scanners that do not read inverted modules.
pub(crate) fn render(
    payload: &str,
    max_width: usize,
    max_height: usize,
) -> Option<Vec<Line<'static>>> {
    // A screen has no damage to tolerate, so the lowest level gives the smallest code.
    let code = QrCode::with_error_correction_level(payload, EcLevel::L).ok()?;
    let side = code.width() + 2 * QUIET_ZONE;
    if side > max_width || side.div_ceil(2) > max_height {
        return None;
    }
    let colors = code.to_colors();
    let dark = |row: usize, col: usize| -> bool {
        let inside = |v: usize, n: usize| v >= QUIET_ZONE && v < QUIET_ZONE + n;
        let (r, c) = (row, col);
        inside(r, code.width())
            && inside(c, code.width())
            && colors[(r - QUIET_ZONE) * code.width() + (c - QUIET_ZONE)] == Module::Dark
    };
    let style = Style::default().fg(Color::Black).bg(Color::White);
    let rows = (0..side)
        .step_by(2)
        .map(|top| {
            let spans = (0..side).map(|col| {
                let glyph = match (dark(top, col), dark(top + 1, col)) {
                    (true, true) => "█",
                    (true, false) => "▀",
                    (false, true) => "▄",
                    (false, false) => " ",
                };
                Span::styled(glyph, style)
            });
            Line::from(spans.collect::<Vec<_>>())
        })
        .collect();
    Some(rows)
}

#[cfg(test)]
/// A payload of `len` characters in the QR alphanumeric set, shaped like a ticket.
pub(crate) fn sample_payload(len: usize) -> String {
    let mut text = String::from("FONTAINE-ATTACH1");
    let mut state = 7u32;
    while text.len() < len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        text.push(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"[(state >> 27) as usize] as char);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_match_measured_ticket_sizes() {
        for (len, version, rows) in [(400, 11, 33), (477, 12, 35), (639, 14, 39)] {
            let text = sample_payload(len);
            let code = QrCode::with_error_correction_level(&text, EcLevel::L).unwrap();
            assert_eq!(code.width(), 17 + 4 * version, "{len} chars");
            let lines = render(&text, 78, rows).expect("fits at its own height");
            assert_eq!(lines.len(), rows, "{len} chars");
        }
    }

    #[test]
    fn it_fits_the_long_ticket_at_39_rows_and_not_at_38() {
        let text = sample_payload(639);
        assert!(render(&text, 78, 39).is_some());
        assert!(render(&text, 78, 38).is_none());
        assert!(render(&text, 72, 60).is_none());
        assert!(render(&text, 73 + 4, 60).is_some());
    }

    #[test]
    fn the_cells_rebuild_the_module_grid() {
        let text = sample_payload(120);
        let code = QrCode::with_error_correction_level(&text, EcLevel::L).unwrap();
        let lines = render(&text, 200, 200).unwrap();
        let n = code.width();
        let side = n + 2 * QUIET_ZONE;
        let mut grid = vec![vec![false; side]; lines.len() * 2];
        for (r, line) in lines.iter().enumerate() {
            assert_eq!(line.spans.len(), side);
            for (c, span) in line.spans.iter().enumerate() {
                assert_eq!(span.style.fg, Some(Color::Black));
                assert_eq!(span.style.bg, Some(Color::White));
                let (top, bottom) = match span.content.as_ref() {
                    "█" => (true, true),
                    "▀" => (true, false),
                    "▄" => (false, true),
                    " " => (false, false),
                    other => panic!("unexpected glyph {other:?}"),
                };
                grid[2 * r][c] = top;
                grid[2 * r + 1][c] = bottom;
            }
        }
        let expected = code.to_colors();
        for row in 0..grid.len() {
            for col in 0..side {
                let want = row >= QUIET_ZONE
                    && col >= QUIET_ZONE
                    && row < QUIET_ZONE + n
                    && col < QUIET_ZONE + n
                    && expected[(row - QUIET_ZONE) * n + (col - QUIET_ZONE)] == Module::Dark;
                assert_eq!(grid[row][col], want, "module ({row}, {col})");
            }
        }
    }
}
