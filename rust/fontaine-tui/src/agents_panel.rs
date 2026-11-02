//! The agents panel: every subagent, ended ones included, with its type, model, status
//! and spend against budget, and the background jobs. Enter on an agent shows its transcript, x
//! kills a row. An agent that ended and left the list below the status line is reached from
//! here.

use std::time::Duration;

use fontaine_agents::runner::ChildStatus;
use fontaine_tools::display::compact_count;
use fontaine_tools::jobs::format_age;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::agent_list::line_dimming_id;
use crate::input::wrap;
use crate::theme::Theme;

/// One row, as the driver reads it from the agent tree.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelRow {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub model: String,
    pub status: ChildStatus,
    pub tokens: u64,
    pub token_budget: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_budget: Option<f64>,
    /// `cost_usd` leaves out calls that carried no price.
    pub cost_partial: bool,
    pub depth: usize,
    /// Characters the model has written in its current step.
    pub writing: u64,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelKey {
    None,
    Close,
    Kill(String),
    KillJob(String),
    /// Enter on an agent: close the panel and show its transcript.
    View(String),
}

#[derive(Default)]
pub struct AgentsPanel {
    rows: Vec<PanelRow>,
    /// The background jobs, listed below the agents; the selection runs through both.
    jobs: Vec<JobRow>,
    selected: usize,
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

/// The figures `spend_text` renders.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpendView {
    pub tokens: u64,
    pub token_budget: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_budget: Option<f64>,
    pub cost_partial: bool,
}

/// What an agent has spent, against its budget when it has one.
pub(crate) fn spend_text(view: &SpendView) -> String {
    let SpendView {
        tokens,
        token_budget,
        cost_usd,
        cost_budget,
        cost_partial,
    } = *view;
    let tokens = match token_budget {
        Some(cap) => format!("{} / {} tok", compact_count(tokens), compact_count(cap)),
        None => format!("{} tok", compact_count(tokens)),
    };
    let mut cost = match (cost_usd, cost_budget) {
        (Some(spent), Some(cap)) => format!("${spent:.4} / ${cap:.2}"),
        (Some(spent), None) => format!("${spent:.4}"),
        (None, Some(_)) => "cost n/a".to_string(),
        (None, None) => String::new(),
    };
    if cost_partial && cost_usd.is_some() {
        cost.push_str(" + unpriced");
    }
    if cost.is_empty() {
        tokens
    } else {
        format!("{tokens} · {cost}")
    }
}

/// `writing 4.2k chars`: the model's progress through a step that has not reported usage yet.
pub(crate) fn writing_text(chars: u64) -> String {
    format!("writing {} chars", compact_count(chars))
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

    pub fn handle_key(&mut self, key: KeyEvent) -> PanelKey {
        if key.kind == KeyEventKind::Release {
            return PanelKey::None;
        }
        let n = self.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return PanelKey::Close,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                self.selected = (self.selected + n - 1) % n;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab if n > 0 => {
                self.selected = (self.selected + 1) % n;
            }
            KeyCode::Enter | KeyCode::Char('i') if self.selected < self.rows.len() => {
                return PanelKey::View(self.rows[self.selected].id.clone());
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
        let mut out = Vec::new();
        if !self.rows.is_empty() {
            out.push(Line::styled("Subagents", bold.fg(theme.heading)));
        }
        for (i, row) in self.rows.iter().enumerate() {
            let marker = if i == self.selected { ">" } else { " " };
            let indent = "  ".repeat(row.depth.saturating_sub(1));
            let head = format!(
                "{marker} {indent}{} ({}) {} {} {}",
                row.name,
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
                out.push(line_dimming_id(piece, &row.id, style));
            }
            let spend = spend_text(&SpendView {
                tokens: row.tokens,
                token_budget: row.token_budget,
                cost_usd: row.cost_usd,
                cost_budget: row.cost_budget,
                cost_partial: row.cost_partial,
            });
            let spend = if row.status.is_running() && row.writing > 0 {
                format!("{spend} · {}", writing_text(row.writing))
            } else {
                spend
            };
            for piece in wrap(&format!("    {spend}"), width) {
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

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let hint = if self.len() == 0 {
            "Esc closes"
        } else {
            "up/down select · Enter views an agent · x kills · Esc closes"
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
            name: id.into(),
            agent_type: "worker".into(),
            model: "m/x".into(),
            status,
            tokens: 1200,
            token_budget: Some(200_000),
            cost_usd: Some(0.0042),
            cost_budget: Some(2.0),
            cost_partial: false,
            depth: 1,
            writing: 0,
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
        assert!(text.contains("1.2k / 200.0k tok"), "{text}");
        assert!(text.contains("$0.0042 / $2.00"), "{text}");
    }

    fn view(cost_budget: Option<f64>, cost_partial: bool) -> SpendView {
        SpendView {
            tokens: 4_863_024,
            token_budget: None,
            cost_usd: Some(0.0519),
            cost_budget,
            cost_partial,
        }
    }

    #[test]
    fn a_partial_cost_is_marked_with_and_without_a_cost_budget() {
        assert_eq!(
            spend_text(&view(None, true)),
            "4.9M tok · $0.0519 + unpriced"
        );
        assert_eq!(
            spend_text(&view(Some(2.0), true)),
            "4.9M tok · $0.0519 / $2.00 + unpriced"
        );
    }

    #[test]
    fn a_complete_cost_is_not_marked() {
        assert_eq!(spend_text(&view(None, false)), "4.9M tok · $0.0519");
        let no_price = SpendView {
            cost_usd: None,
            cost_budget: Some(2.0),
            ..view(None, true)
        };
        assert_eq!(spend_text(&no_price), "4.9M tok · cost n/a");
    }

    #[test]
    fn an_uncapped_row_shows_only_what_it_spent() {
        let mut panel = AgentsPanel::new();
        let mut uncapped = row("agent-1", ChildStatus::Running);
        uncapped.token_budget = None;
        uncapped.cost_budget = None;
        panel.set_rows(vec![uncapped]);
        let text: String = panel
            .body(80, &Theme::default())
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("1.2k tok"), "{text}");
        assert!(!text.contains("1.2k /"), "{text}");
        assert!(text.contains("$0.0042"), "{text}");
        assert!(!text.contains("/ $"), "{text}");
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

    #[test]
    fn writing_is_counted_in_chars_then_thousands() {
        assert_eq!(writing_text(999), "writing 999 chars");
        assert_eq!(writing_text(1000), "writing 1.0k chars");
        assert_eq!(writing_text(12_345), "writing 12.3k chars");
    }

    #[test]
    fn a_running_row_shows_writing_on_the_spend_line_and_a_done_row_does_not() {
        let mut panel = AgentsPanel::new();
        let mut running = row("agent-1", ChildStatus::Running);
        running.writing = 4200;
        let mut done = row("agent-2", ChildStatus::Done);
        done.writing = 4200;
        panel.set_rows(vec![running, done]);
        let text: Vec<String> = panel
            .body(120, &Theme::default())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(
            text.iter()
                .filter(|l| l.contains("writing 4.2k chars"))
                .count(),
            1
        );
    }
}
