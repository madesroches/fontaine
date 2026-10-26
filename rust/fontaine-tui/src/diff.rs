//! A diff as painted rows, shared by the approval prompt and the transcript's tool cards so the
//! two cannot drift.

use fontaine_core::preview::{DiffKind, DiffPreview};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::theme::Theme;

/// The diff as rows: a header, then each line behind a line-number gutter and a sign. Wrapped
/// text continues under itself with a blank gutter.
///
/// At most `max_rows` of the preview's lines are drawn (all of them for `None`); what the cap
/// and the preview's own limit leave out is counted in a closing row that ends with `suffix`.
/// Returns whether anything was left out.
pub fn diff_rows(
    preview: &DiffPreview,
    width: usize,
    theme: &Theme,
    max_rows: Option<usize>,
    suffix: &str,
    out: &mut Vec<Line<'static>>,
) -> bool {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let header = format!("{}  +{} -{}", preview.path, preview.added, preview.removed);
    for piece in wrap(&header, width) {
        out.push(Line::styled(
            piece,
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    let shown = max_rows.map_or(preview.lines.len(), |max| max.min(preview.lines.len()));
    let digits = preview
        .lines
        .iter()
        .flat_map(|l| [l.old_no, l.new_no])
        .flatten()
        .max()
        .map_or(1, |n| n.to_string().len());
    let text_width = width.saturating_sub(digits + 2).max(1);
    for line in &preview.lines[..shown] {
        let (sign, style) = match line.kind {
            DiffKind::Added => ('+', Style::default().fg(theme.success)),
            DiffKind::Removed => ('-', Style::default().fg(theme.error)),
            DiffKind::Context | DiffKind::Gap => (' ', dim),
        };
        let number = match line.kind {
            DiffKind::Removed => line.old_no,
            _ => line.new_no,
        };
        let no = number.map_or_else(|| " ".repeat(digits), |n| format!("{n:>digits$}"));
        for (i, piece) in wrap(&line.text, text_width).into_iter().enumerate() {
            let (no, sign) = if i == 0 {
                (no.clone(), sign)
            } else {
                (" ".repeat(digits), ' ')
            };
            out.push(Line::styled(format!("{no} {sign}{piece}"), style));
        }
    }
    let cut = &preview.lines[shown..];
    let more = preview.hidden
        + cut
            .iter()
            .filter(|l| matches!(l.kind, DiffKind::Added | DiffKind::Removed))
            .count();
    if more > 0 {
        out.push(Line::styled(
            format!("… {more} more changed lines{suffix}"),
            dim,
        ));
    }
    more > 0 || !cut.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::preview::DiffLine;

    fn preview(n: usize, hidden: usize) -> DiffPreview {
        DiffPreview {
            path: "a.rs".into(),
            added: n,
            removed: 0,
            lines: (1..=n)
                .map(|i| DiffLine {
                    kind: DiffKind::Added,
                    old_no: None,
                    new_no: Some(i),
                    text: format!("l{i}"),
                })
                .collect(),
            hidden,
        }
    }

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn an_uncapped_diff_draws_every_line() {
        let mut out = Vec::new();
        let cut = diff_rows(&preview(3, 0), 40, &Theme::default(), None, "", &mut out);
        assert!(!cut);
        assert_eq!(plain(&out), ["a.rs  +3 -0", "1 +l1", "2 +l2", "3 +l3"]);
    }

    #[test]
    fn the_cap_counts_the_changed_lines_it_leaves_out() {
        let mut out = Vec::new();
        let cut = diff_rows(&preview(5, 2), 40, &Theme::default(), Some(2), "", &mut out);
        assert!(cut);
        assert_eq!(plain(&out).last().unwrap(), "… 5 more changed lines");
        assert_eq!(out.len(), 1 + 2 + 1);
    }

    #[test]
    fn the_suffix_ends_the_closing_row() {
        let mut out = Vec::new();
        diff_rows(
            &preview(1, 4),
            40,
            &Theme::default(),
            None,
            " (not shown)",
            &mut out,
        );
        assert_eq!(
            plain(&out).last().unwrap(),
            "… 4 more changed lines (not shown)"
        );
    }
}
