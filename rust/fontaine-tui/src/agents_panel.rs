//! The background-agents panel: one row per subagent with its type, model, status and spend
//! against budget, and keys to look inside a row, to kill it and to see what it is waiting for.
//! A background agent that needs an approval is listed here, so it never hides behind the main
//! conversation's transcript.

use std::time::Duration;

use fontaine_agents::runner::ChildStatus;
use fontaine_tools::jobs::format_age;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::input::wrap;
use crate::theme::Theme;

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

/// One background job or monitor of the main agent, as the driver reads it from the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub command: String,
    pub monitor: bool,
    pub age: Duration,
    pub running: bool,
    pub state: String,
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
    KillJob(String),
}

#[derive(Default)]
pub struct AgentsPanel {
    rows: Vec<PanelRow>,
    /// The background jobs, listed below the agents; the selection runs through both.
    jobs: Vec<JobRow>,
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
        let id = self.selected_entry_id();
        self.rows = rows;
        self.reselect(id);
    }

    /// Replaces the job rows, keeping the selection on the same entry.
    pub fn set_jobs(&mut self, jobs: Vec<JobRow>) {
        let id = self.selected_entry_id();
        self.jobs = jobs;
        self.reselect(id);
    }

    fn reselect(&mut self, id: Option<String>) {
        let at = id.and_then(|id| {
            self.rows.iter().position(|r| r.id == id).or_else(|| {
                self.jobs
                    .iter()
                    .position(|j| j.id == id)
                    .map(|at| at + self.rows.len())
            })
        });
        self.selected = at.unwrap_or_else(|| self.selected.min(self.len().saturating_sub(1)));
    }

    fn len(&self) -> usize {
        self.rows.len() + self.jobs.len()
    }

    fn selected_entry_id(&self) -> Option<String> {
        match self.selected.checked_sub(self.rows.len()) {
            None => self.rows.get(self.selected).map(|r| r.id.clone()),
            Some(job) => self.jobs.get(job).map(|j| j.id.clone()),
        }
    }

    pub fn jobs(&self) -> &[JobRow] {
        &self.jobs
    }

    pub fn rows(&self) -> &[PanelRow] {
        &self.rows
    }

    /// The selected agent; none when a job is selected.
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
        let n = self.len();
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
            KeyCode::Enter | KeyCode::Char('i') if self.selected < self.rows.len() => {
                self.open = true;
            }
            KeyCode::Char('x') | KeyCode::Char('K')
                if n > 0 && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                match self.selected.checked_sub(self.rows.len()) {
                    None => {
                        let row = &self.rows[self.selected];
                        if row.status.is_running() {
                            return PanelKey::Kill(row.id.clone());
                        }
                    }
                    Some(job) => {
                        if let Some(row) = self.jobs.get(job).filter(|row| row.running) {
                            return PanelKey::KillJob(row.id.clone());
                        }
                    }
                }
            }
            _ => {}
        }
        PanelKey::None
    }

    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        if self.len() == 0 {
            return vec![Line::styled(
                "No subagents or background jobs have been started.",
                dim,
            )];
        }
        if self.open
            && let Some(detail) = &self.detail
        {
            return self.detail_lines(detail, width, theme);
        }
        let mut out = Vec::new();
        if !self.rows.is_empty() {
            out.push(Line::styled("Subagents", bold.fg(theme.heading)));
        }
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
        if !self.jobs.is_empty() {
            out.push(Line::styled("Background jobs", bold.fg(theme.heading)));
        }
        for (i, job) in self.jobs.iter().enumerate() {
            let selected = self.rows.len() + i == self.selected;
            let marker = if selected { ">" } else { " " };
            let kind = if job.monitor { "monitor " } else { "" };
            let head = format!(
                "{marker} {} {kind}{} {}",
                job.id,
                job.state,
                format_age(job.age)
            );
            let style = if selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            for piece in wrap(&head, width) {
                out.push(Line::styled(piece, style));
            }
            let command = job.command.lines().next().unwrap_or_default();
            for piece in wrap(&format!("    {command}"), width) {
                out.push(Line::styled(piece, dim));
            }
        }
        out
    }

    fn detail_lines(&self, detail: &Detail, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = vec![Line::styled(
            format!("{} in detail", detail.id),
            bold.fg(theme.heading),
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
        let hint = if self.len() == 0 {
            "Esc closes"
        } else if self.open {
            "Esc goes back · x kills this agent"
        } else {
            "up/down select · Enter looks inside an agent · x kills · Esc closes"
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
            .body(80, &Theme::default())
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

    fn job(id: &str, running: bool) -> JobRow {
        JobRow {
            id: id.into(),
            command: "npm start".into(),
            monitor: false,
            age: Duration::from_secs(252),
            running,
            state: if running { "running" } else { "exit code 0" }.into(),
        }
    }

    #[test]
    fn jobs_are_listed_below_the_agents_and_only_a_running_one_is_killed_by_its_own_key() {
        let mut panel = AgentsPanel::new();
        panel.set_rows(vec![row("agent-1", ChildStatus::Running)]);
        panel.set_jobs(vec![job("job-3f2a", true), job("job-9c01", false)]);
        let text: String = panel
            .body(80, &Theme::default())
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Background jobs"), "{text}");
        assert!(text.contains("job-3f2a running 4m12s"), "{text}");
        assert!(text.contains("npm start"), "{text}");
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('x'))),
            PanelKey::Kill("agent-1".into())
        );
        panel.handle_key(key(KeyCode::Down));
        assert_eq!(panel.selected_id(), None);
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('x'))),
            PanelKey::KillJob("job-3f2a".into())
        );
        panel.handle_key(key(KeyCode::Down));
        assert_eq!(panel.handle_key(key(KeyCode::Char('x'))), PanelKey::None);
        // A refresh keeps the selection on the same job.
        panel.set_jobs(vec![job("job-0000", true), job("job-9c01", false)]);
        assert_eq!(panel.selected_entry_id().as_deref(), Some("job-9c01"));
    }

    #[test]
    fn a_waiting_agent_is_labelled_with_what_it_waits_on() {
        assert_eq!(
            status_label(ChildStatus::WaitingOnPermission),
            "waiting-on-permission"
        );
    }
}
