//! The task list the model keeps with `todo_write`: one line above the input while collapsed,
//! every item while expanded, nothing while the list is empty.

use fontaine_tools::todo::{TodoItem, TodoStatus};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

#[derive(Default)]
pub struct TodoPanel {
    items: Vec<TodoItem>,
    pub expanded: bool,
}

pub fn marker(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "[ ]",
        TodoStatus::InProgress => "[~]",
        TodoStatus::Completed => "[x]",
    }
}

fn clip(text: &str, width: usize) -> String {
    let first = text.lines().next().unwrap_or("");
    if first.chars().count() <= width {
        return first.to_string();
    }
    let kept: String = first.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

impl TodoPanel {
    pub fn set_items(&mut self, items: Vec<TodoItem>) {
        self.items = items;
    }

    pub fn items(&self) -> &[TodoItem] {
        &self.items
    }

    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    /// The rows to draw in `width` columns, at most `max_rows` of them; none for an empty list.
    pub fn lines(&self, width: usize, max_rows: usize) -> Vec<Line<'static>> {
        if self.items.is_empty() {
            return Vec::new();
        }
        let dim = Style::default().add_modifier(Modifier::DIM);
        if !self.expanded || max_rows < 2 {
            return vec![Line::styled(clip(&self.summary(), width), dim)];
        }
        // The last row is the overflow line when the items do not all fit.
        let shown = if self.items.len() <= max_rows {
            self.items.len()
        } else {
            max_rows - 1
        };
        let mut lines: Vec<Line<'static>> = self.items[..shown]
            .iter()
            .map(|item| {
                let style = match item.status {
                    TodoStatus::Pending => Style::default(),
                    TodoStatus::InProgress => Style::default().add_modifier(Modifier::BOLD),
                    TodoStatus::Completed => dim,
                };
                let text = format!("{} {}", marker(item.status), item.content);
                Line::styled(clip(&text, width), style)
            })
            .collect();
        if shown < self.items.len() {
            lines.push(Line::styled(
                format!("+{} more", self.items.len() - shown),
                dim,
            ));
        }
        lines
    }

    fn summary(&self) -> String {
        let done = self
            .items
            .iter()
            .filter(|item| item.status == TodoStatus::Completed)
            .count();
        let mut text = format!("Tasks {done}/{}", self.items.len());
        if let Some(current) = self
            .items
            .iter()
            .find(|item| item.status == TodoStatus::InProgress)
        {
            text.push_str(" · ▶ ");
            text.push_str(&current.content);
        }
        text
    }
}

/// The list as plain lines, for the renderer that has no panel.
pub fn plain_lines(items: &[TodoItem]) -> Vec<String> {
    if items.is_empty() {
        return vec!["Tasks: none".to_string()];
    }
    let mut lines = vec!["Tasks:".to_string()];
    lines.extend(items.iter().map(|item| {
        format!(
            "{} {}",
            marker(item.status),
            item.content.replace('\n', " ")
        )
    }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
        }
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn panel() -> TodoPanel {
        let mut panel = TodoPanel::default();
        panel.set_items(vec![
            item("one", TodoStatus::Completed),
            item("two", TodoStatus::InProgress),
            item("three", TodoStatus::Pending),
        ]);
        panel
    }

    #[test]
    fn an_empty_list_draws_nothing() {
        assert!(TodoPanel::default().lines(80, 10).is_empty());
    }

    #[test]
    fn collapsed_is_one_line_with_the_current_item() {
        assert_eq!(text(&panel().lines(80, 10)), ["Tasks 1/3 · ▶ two"]);
        let mut idle = TodoPanel::default();
        idle.set_items(vec![item("a", TodoStatus::Pending)]);
        assert_eq!(text(&idle.lines(80, 10)), ["Tasks 0/1"]);
    }

    #[test]
    fn expanded_lists_every_item_with_a_marker() {
        let mut panel = panel();
        panel.toggle();
        assert_eq!(
            text(&panel.lines(80, 10)),
            ["[x] one", "[~] two", "[ ] three"]
        );
    }

    #[test]
    fn an_overflowing_list_ends_with_a_count_of_the_rest() {
        let mut panel = panel();
        panel.toggle();
        assert_eq!(text(&panel.lines(80, 2)), ["[x] one", "+2 more"]);
        assert_eq!(text(&panel.lines(80, 1)), ["Tasks 1/3 · ▶ two"]);
    }

    #[test]
    fn long_text_is_cut_to_the_width() {
        let line = text(&panel().lines(10, 1));
        assert_eq!(line[0].chars().count(), 10);
        assert!(line[0].ends_with('…'));
    }

    #[test]
    fn plain_lines_name_the_state_of_each_item() {
        assert_eq!(plain_lines(&[]), ["Tasks: none"]);
        assert_eq!(
            plain_lines(panel().items()),
            ["Tasks:", "[x] one", "[~] two", "[ ] three"]
        );
    }
}
