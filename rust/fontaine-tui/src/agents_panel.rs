//! The background-agents panel: one row per subagent with its type, model, status and spend
//! against budget, and keys to look inside a row, to kill it and to see what it is waiting for.
//! A background agent that needs an approval is listed here, so it never hides behind the main
//! conversation's transcript.

use fontaine_agents::runner::ChildStatus;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;

/// One row, as the driver reads it from the agent tree.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelRow {
    pub id: String,
    pub agent_type: String,
    pub model: String,
    pub status: ChildStatus,
    pub tokens: u64,
    pub token_budget: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_budget: Option<f64>,
    pub depth: usize,
    /// Why it stopped early, when its budget ran out.
    pub stopped_because: Option<String>,
}

/// What the panel shows for the row that is open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detail {
    pub id: String,
    /// The calls it is waiting for the human to decide.
    pub pending: Vec<String>,
    /// Its gate decisions, tier and audit entries included, oldest first.
    pub decisions: Vec<String>,
    /// What it has said and done.
    pub log: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelKey {
    None,
    Close,
    Kill(String),
}

#[derive(Default)]
pub struct AgentsPanel {
    rows: Vec<PanelRow>,
    selected: usize,
    open: bool,
    pub detail: Option<Detail>,
}

pub fn status_label(status: ChildStatus) -> &'static str {
    match status {
        ChildStatus::Running => "running",
        ChildStatus::WaitingOnPermission => "waiting-on-permission",
        ChildStatus::Done => "done",
        ChildStatus::Killed => "killed",
        ChildStatus::BudgetExhausted => "budget-exhausted",
        ChildStatus::Failed => "failed",
    }
}

fn spend_text(row: &PanelRow) -> String {
    let tokens = match row.token_budget {
        Some(cap) => format!("{} / {} tok", row.tokens, cap),
        None => format!("{} tok", row.tokens),
    };
    let cost = match (row.cost_usd, row.cost_budget) {
        (Some(spent), Some(cap)) => format!("${spent:.4} / ${cap:.2}"),
        (Some(spent), None) => format!("${spent:.4}"),
        (None, Some(_)) => "cost n/a".to_string(),
        (None, None) => String::new(),
    };
    if cost.is_empty() {
        tokens
    } else {
        format!("{tokens} · {cost}")
    }
}

impl AgentsPanel {
    pub fn new() -> AgentsPanel {
        AgentsPanel::default()
    }

    /// Replaces the rows, keeping the selection on the same agent.
    pub fn set_rows(&mut self, rows: Vec<PanelRow>) {
        let id = self.selected_id();
        self.rows = rows;
        self.selected = id
            .and_then(|id| self.rows.iter().position(|r| r.id == id))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    pub fn rows(&self) -> &[PanelRow] {
        &self.rows
    }

    pub fn selected_id(&self) -> Option<String> {
        self.rows.get(self.selected).map(|r| r.id.clone())
    }

    /// Whether a row is open and the driver should fill in its detail.
    pub fn is_inspecting(&self) -> bool {
        self.open
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PanelKey {
        if key.kind == KeyEventKind::Release {
            return PanelKey::None;
        }
        let n = self.rows.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') if self.open => {
                self.open = false;
                self.detail = None;
            }
            KeyCode::Esc | KeyCode::Char('q') => return PanelKey::Close,
            KeyCode::Up | KeyCode::Char('k') if n > 0 && !self.open => {
                self.selected = (self.selected + n - 1) % n;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab if n > 0 && !self.open => {
                self.selected = (self.selected + 1) % n;
            }
            KeyCode::Enter | KeyCode::Char('i') if n > 0 => self.open = true,
            KeyCode::Char('x') | KeyCode::Char('K')
                if n > 0 && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let row = &self.rows[self.selected];
                if row.status.is_running() {
                    return PanelKey::Kill(row.id.clone());
                }
            }
            _ => {}
        }
        PanelKey::None
    }

    pub fn body(&self, width: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        if self.rows.is_empty() {
            return vec![Line::styled("No subagents have been started.", dim)];
        }
        if self.open
            && let Some(detail) = &self.detail
        {
            return self.detail_lines(detail, width);
        }
        let mut out = vec![Line::styled("Subagents", bold.fg(Color::Yellow))];
        for (i, row) in self.rows.iter().enumerate() {
            let marker = if i == self.selected { ">" } else { " " };
            let indent = "  ".repeat(row.depth.saturating_sub(1));
            let head = format!(
                "{marker} {indent}{} {} {} {}",
                row.id,
                row.agent_type,
                row.model,
                status_label(row.status)
            );
            let style = if i == self.selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            for piece in wrap(&head, width) {
                out.push(Line::styled(piece, style));
            }
            for piece in wrap(&format!("    {}", spend_text(row)), width) {
                out.push(Line::styled(piece, dim));
            }
            if let Some(why) = &row.stopped_because {
                for piece in wrap(&format!("    stopped: {why}"), width) {
                    out.push(Line::styled(piece, dim));
                }
            }
        }
        out
    }

    fn detail_lines(&self, detail: &Detail, width: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = vec![Line::styled(
            format!("{} in detail", detail.id),
            bold.fg(Color::Yellow),
        )];
        let mut section = |title: &str, items: &[String], empty: &str| {
            out.push(Line::styled(title.to_string(), bold));
            if items.is_empty() {
                out.push(Line::styled(format!("  {empty}"), dim));
            }
            for item in items {
                for piece in wrap(&format!("  {item}"), width) {
                    out.push(Line::raw(piece));
                }
            }
        };
        section("waiting for you", &detail.pending, "nothing");
        section("gate decisions", &detail.decisions, "none yet");
        section("transcript", &detail.log, "nothing yet");
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let hint = if self.rows.is_empty() {
            "Esc closes"
        } else if self.open {
            "Esc goes back · x kills this agent"
        } else {
            "up/down select · Enter looks inside · x kills · Esc closes"
        };
        vec![Line::styled(hint, dim)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, status: ChildStatus) -> PanelRow {
        PanelRow {
            id: id.into(),
            agent_type: "worker".into(),
            model: "m/x".into(),
            status,
            tokens: 1200,
            token_budget: Some(200_000),
            cost_usd: Some(0.0042),
            cost_budget: Some(2.0),
            depth: 1,
            stopped_because: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn a_row_shows_status_and_spend_against_budget() {
        let mut panel = AgentsPanel::new();
        panel.set_rows(vec![row("agent-1", ChildStatus::Killed)]);
        let text: String = panel
            .body(80)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("killed"), "{text}");
        assert!(text.contains("1200 / 200000 tok"), "{text}");
        assert!(text.contains("$0.0042 / $2.00"), "{text}");
    }

    #[test]
    fn only_a_running_agent_can_be_killed_and_selection_survives_a_refresh() {
        let mut panel = AgentsPanel::new();
        panel.set_rows(vec![
            row("agent-1", ChildStatus::Done),
            row("agent-2", ChildStatus::Running),
        ]);
        assert_eq!(panel.handle_key(key(KeyCode::Char('x'))), PanelKey::None);
        panel.handle_key(key(KeyCode::Down));
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('x'))),
            PanelKey::Kill("agent-2".into())
        );
        panel.set_rows(vec![
            row("agent-0", ChildStatus::Running),
            row("agent-1", ChildStatus::Done),
            row("agent-2", ChildStatus::Running),
        ]);
        assert_eq!(panel.selected_id().as_deref(), Some("agent-2"));
    }

    #[test]
    fn a_waiting_agent_is_labelled_with_what_it_waits_on() {
        assert_eq!(
            status_label(ChildStatus::WaitingOnPermission),
            "waiting-on-permission"
        );
    }
}
