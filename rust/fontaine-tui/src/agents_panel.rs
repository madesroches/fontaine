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

use crate::agent_list::wrapped_dimming_id;
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
    /// Tokens of `tokens` that carried no price; `cost_usd` leaves them out.
    pub unpriced_tokens: u64,
    pub depth: usize,
    /// Characters the model has written in its current step.
    pub writing: u64,
    /// Why it stopped early, when its budget ran out.
    pub stopped_because: Option<String>,
    /// A teammate waiting for work, which is not the same as done.
    pub idle: bool,
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

/// One task of an agent's board, as the driver reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardRow {
    /// The name of the agent that owns the board the task is on.
    pub board: String,
    pub id: String,
    /// Written by an agent: shown with its control characters taken out.
    pub title: String,
    pub status: String,
    /// The owner's name, when it has one.
    pub owner: Option<String>,
    pub blocked_by: Vec<String>,
}

#[derive(Default)]
pub struct AgentsPanel {
    rows: Vec<PanelRow>,
    /// The task boards, each under its owner's name, shown below the agents and the jobs; they
    /// are not selectable.
    board: Vec<BoardRow>,
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

/// What a row says about its agent: one that finished its turn and can be woken is `idle`, not
/// `done`.
pub fn row_label(status: ChildStatus, idle: bool) -> &'static str {
    if idle && status == ChildStatus::Done {
        "idle"
    } else {
        status_label(status)
    }
}

/// `compact_count` without a trailing `.0` (`110k`, not `110.0k`).
pub(crate) fn trimmed_count(n: u64) -> String {
    let text = compact_count(n);
    match text.strip_suffix('k').or_else(|| text.strip_suffix('M')) {
        Some(digits) if digits.ends_with(".0") => {
            format!("{}{}", &digits[..digits.len() - 2], &text[text.len() - 1..])
        }
        _ => text,
    }
}

/// The figures `spend_text` renders.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpendView {
    pub tokens: u64,
    pub token_budget: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_budget: Option<f64>,
    /// Tokens of `tokens` that carried no price, so `cost_usd` leaves them out.
    pub unpriced_tokens: u64,
}

/// What an agent has spent, against its budget when it has one: the priced tokens next to their
/// price, then the unpriced tokens on their own.
pub(crate) fn spend_text(view: &SpendView) -> String {
    let SpendView {
        tokens,
        token_budget,
        cost_usd,
        cost_budget,
        unpriced_tokens,
    } = *view;
    let mut parts = Vec::new();
    match token_budget {
        // The cap binds on the total, so the budgeted figure is the total.
        Some(cap) => parts.push(format!(
            "{} / {} tok",
            trimmed_count(tokens),
            trimmed_count(cap)
        )),
        None => {
            let priced = tokens.saturating_sub(unpriced_tokens);
            if priced > 0 || unpriced_tokens == 0 {
                parts.push(format!("{} tok", trimmed_count(priced)));
            }
        }
    }
    match (cost_usd, cost_budget) {
        (Some(spent), Some(cap)) => parts.push(format!("${spent:.4} / ${cap:.2}")),
        (Some(spent), None) => parts.push(format!("${spent:.4}")),
        (None, Some(_)) => parts.push("cost n/a".to_string()),
        (None, None) => {}
    }
    if unpriced_tokens > 0 {
        parts.push(format!("{} unpriced tok", trimmed_count(unpriced_tokens)));
    }
    parts.join(" · ")
}

/// `writing 4.2k chars`: the model's progress through a step that has not reported usage yet.
pub(crate) fn writing_text(chars: u64) -> String {
    format!("writing {} chars", trimmed_count(chars))
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

    /// Replaces the board's rows.
    pub fn set_board(&mut self, board: Vec<BoardRow>) {
        self.board = board;
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
                        if row.status.is_running() || row.idle {
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
        if self.len() == 0 && self.board.is_empty() {
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
                row_label(row.status, row.idle)
            );
            let style = if i == self.selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            out.extend(wrapped_dimming_id(&head, &row.id, width, style));
            let spend = spend_text(&SpendView {
                tokens: row.tokens,
                token_budget: row.token_budget,
                cost_usd: row.cost_usd,
                cost_budget: row.cost_budget,
                unpriced_tokens: row.unpriced_tokens,
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
        let mut shown: Option<&str> = None;
        for task in &self.board {
            if shown != Some(task.board.as_str()) {
                out.push(Line::styled(
                    format!("Board of {}", task.board),
                    bold.fg(theme.heading),
                ));
                shown = Some(task.board.as_str());
            }
            let title: String = task
                .title
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            for piece in wrap(&format!("  {} {} {title}", task.id, task.status), width) {
                out.push(Line::raw(piece));
            }
            let owner = task.owner.as_deref().unwrap_or("nobody");
            let blocked = if task.blocked_by.is_empty() {
                String::new()
            } else {
                format!(" · blocked by {}", task.blocked_by.join(", "))
            };
            for piece in wrap(&format!("    owner: {owner}{blocked}"), width) {
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
            unpriced_tokens: 0,
            depth: 1,
            writing: 0,
            idle: false,
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
        assert!(text.contains("1.2k / 200k tok"), "{text}");
        assert!(text.contains("$0.0042 / $2.00"), "{text}");
    }

    fn view(cost_budget: Option<f64>, unpriced_tokens: u64) -> SpendView {
        SpendView {
            tokens: 238_000,
            token_budget: None,
            cost_usd: Some(0.0066),
            cost_budget,
            unpriced_tokens,
        }
    }

    #[test]
    fn priced_tokens_sit_next_to_their_price_and_unpriced_ones_follow() {
        assert_eq!(
            spend_text(&view(None, 128_000)),
            "110k tok · $0.0066 · 128k unpriced tok"
        );
        assert_eq!(
            spend_text(&view(Some(2.0), 128_000)),
            "110k tok · $0.0066 / $2.00 · 128k unpriced tok"
        );
    }

    #[test]
    fn a_token_budget_keeps_the_total() {
        let capped = SpendView {
            token_budget: Some(500_000),
            ..view(None, 128_000)
        };
        assert_eq!(
            spend_text(&capped),
            "238k / 500k tok · $0.0066 · 128k unpriced tok"
        );
    }

    #[test]
    fn a_fully_unpriced_agent_shows_only_unpriced_tokens() {
        let none = SpendView {
            tokens: 4_863_024,
            cost_usd: None,
            ..view(None, 4_863_024)
        };
        assert_eq!(spend_text(&none), "4.9M unpriced tok");
        let budgeted = SpendView {
            cost_budget: Some(2.0),
            ..none
        };
        assert_eq!(spend_text(&budgeted), "cost n/a · 4.9M unpriced tok");
        let both = SpendView {
            token_budget: Some(5_000_000),
            ..budgeted
        };
        assert_eq!(
            spend_text(&both),
            "4.9M / 5M tok · cost n/a · 4.9M unpriced tok"
        );
    }

    #[test]
    fn a_fully_priced_agent_has_no_unpriced_segment() {
        assert_eq!(spend_text(&view(None, 0)), "238k tok · $0.0066");
        let no_price = SpendView {
            cost_usd: None,
            ..view(Some(2.0), 0)
        };
        assert_eq!(spend_text(&no_price), "238k tok · cost n/a");
    }

    #[test]
    fn counts_drop_a_trailing_zero_decimal() {
        assert_eq!(trimmed_count(999), "999");
        assert_eq!(trimmed_count(1_000), "1k");
        assert_eq!(trimmed_count(1_050), "1.1k");
        assert_eq!(trimmed_count(200_000), "200k");
        assert_eq!(trimmed_count(4_900_000), "4.9M");
        assert_eq!(trimmed_count(5_000_000), "5M");
    }

    #[test]
    fn a_stopped_row_formats_its_token_budget_with_trimmed_counts() {
        let why = fontaine_agents::budget::Exhausted::Tokens {
            spent: 200_000,
            cap: 500_000,
        };
        assert_eq!(
            why.describe_with(trimmed_count),
            "its token budget is spent (200k of 500k)"
        );
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

    #[test]
    fn an_idle_teammate_can_be_killed() {
        let mut panel = AgentsPanel::new();
        let mut idle = row("agent-1", ChildStatus::Done);
        idle.idle = true;
        panel.set_rows(vec![idle]);
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('x'))),
            PanelKey::Kill("agent-1".into())
        );
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
        assert_eq!(writing_text(1000), "writing 1k chars");
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
