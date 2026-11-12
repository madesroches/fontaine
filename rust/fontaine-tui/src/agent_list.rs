//! The list of agents below the status line: `main` and the subagents worth looking at, which
//! Down from the prompt focuses. Enter views the selected agent's transcript, x stops it.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use fontaine_agents::effort::Effort;
use fontaine_agents::runner::ChildStatus;
use fontaine_agents::tree::AgentRow;
use fontaine_tools::jobs::format_age;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agents_panel::{SpendView, row_label, spend_text, writing_text};
use crate::input::{typed_char, wrap};

/// How long an agent that failed, was killed or ran out of budget stays listed.
pub const ENDED_LISTED: Duration = Duration::from_secs(30);

/// One row: `main`, or a subagent.
#[derive(Clone, Debug, PartialEq)]
pub struct ListRow {
    /// `None` is the main conversation.
    pub id: Option<String>,
    /// The agent's name; `main` for the main conversation.
    pub name: String,
    pub depth: usize,
    pub agent_type: String,
    /// Empty when the tree holds no model for the agent.
    pub model: String,
    pub effort: Option<Effort>,
    pub status: Option<ChildStatus>,
    pub elapsed: Duration,
    pub spend: String,
    /// The spend ends in an unpriced-tokens segment.
    pub unpriced: bool,
    /// Characters the model has written in its current step; shown while the agent runs.
    pub writing: u64,
    /// An agent waiting to be woken, listed while it owns a task in progress.
    pub idle: bool,
}

impl ListRow {
    fn main() -> ListRow {
        ListRow {
            id: None,
            name: "main".to_string(),
            depth: 0,
            agent_type: String::new(),
            model: String::new(),
            effort: None,
            status: None,
            elapsed: Duration::ZERO,
            spend: String::new(),
            unpriced: false,
            writing: 0,
            idle: false,
        }
    }
}

/// The rows to list: `main` first, then every running agent, the one being viewed, an idle
/// agent that owns a task in progress (`task_owners`), and those that failed, were killed or ran
/// out of budget less than [`ENDED_LISTED`] ago and were not dismissed. An agent that is done, or
/// idle with no task, leaves at once. Nothing is listed while no subagent is. `main_writing` is
/// the main conversation's step progress, which `main` shows.
pub fn rows(
    agents: &[AgentRow],
    viewing: Option<&str>,
    dismissed: &HashSet<String>,
    task_owners: &HashSet<String>,
    now: Instant,
    main_writing: u64,
) -> Vec<ListRow> {
    let listed = |row: &AgentRow| {
        if row.status.is_running()
            || (row.idle && task_owners.contains(&row.id))
            || viewing == Some(row.id.as_str())
        {
            return true;
        }
        let recent = row
            .ended
            .is_some_and(|ended| now.saturating_duration_since(ended) < ENDED_LISTED);
        match row.status {
            ChildStatus::Failed | ChildStatus::Killed | ChildStatus::BudgetExhausted => {
                recent && !dismissed.contains(&row.id)
            }
            _ => false,
        }
    };
    let mut out = Vec::new();
    let order = depth_first(agents);
    for row in order {
        if listed(row) {
            out.push(ListRow {
                id: Some(row.id.clone()),
                name: row.name.clone(),
                depth: row.depth,
                agent_type: row.agent_type.clone(),
                model: row.model.clone(),
                effort: row.effort,
                status: Some(row.status),
                elapsed: row
                    .ended
                    .unwrap_or(now)
                    .saturating_duration_since(row.started)
                    .saturating_sub(row.paused),
                spend: spend_text(&SpendView {
                    tokens: row.spend.tokens(),
                    token_budget: row.budget.tokens,
                    cost_usd: row.spend.cost_usd,
                    cost_budget: row.budget.cost_usd,
                    unpriced_tokens: row.spend.unpriced_tokens,
                }),
                unpriced: row.spend.unpriced_tokens > 0,
                writing: row.writing,
                idle: row.idle,
            });
        }
    }
    if out.is_empty() {
        return out;
    }
    out.insert(
        0,
        ListRow {
            writing: main_writing,
            idle: false,
            ..ListRow::main()
        },
    );
    out
}

/// `text` as a line with the `(id)` tag dimmed, when it holds one.
pub(crate) fn line_dimming_id(text: String, id: &str, style: Style) -> Line<'static> {
    let tag = format!("({id})");
    let Some(at) = text.find(&tag) else {
        return Line::styled(text, style);
    };
    let after = text[at + tag.len()..].to_string();
    let before = text[..at].to_string();
    Line::from(vec![
        Span::raw(before),
        Span::styled(tag, Style::default().add_modifier(Modifier::DIM)),
        Span::raw(after),
    ])
    .style(style)
}

/// `head` wrapped to `width` as lines, with the `(id)` tag dimmed even where a cut falls inside it.
pub(crate) fn wrapped_dimming_id(
    head: &str,
    id: &str,
    width: usize,
    style: Style,
) -> Vec<Line<'static>> {
    let tag = format!("({id})");
    let range = head.find(&tag).map(|at| {
        let start = head[..at].chars().count();
        start..start + tag.chars().count()
    });
    let width = width.max(1);
    wrap(head, width)
        .into_iter()
        .enumerate()
        .map(|(n, piece)| {
            let Some(range) = &range else {
                return Line::styled(piece, style);
            };
            let from = n * width;
            let chars: Vec<char> = piece.chars().collect();
            let lo = range.start.saturating_sub(from).min(chars.len());
            let hi = range.end.saturating_sub(from).min(chars.len());
            let part = |r: std::ops::Range<usize>| chars[r].iter().collect::<String>();
            Line::from(vec![
                Span::raw(part(0..lo)),
                Span::styled(part(lo..hi), Style::default().add_modifier(Modifier::DIM)),
                Span::raw(part(hi..chars.len())),
            ])
            .style(style)
        })
        .collect()
}

/// The agents with each one's children right after it.
fn depth_first(agents: &[AgentRow]) -> Vec<&AgentRow> {
    fn walk<'a>(
        agents: &'a [AgentRow],
        parent: Option<&str>,
        out: &mut Vec<&'a AgentRow>,
        depth: usize,
    ) {
        // A corrupt parent link must not loop forever.
        if depth > 64 {
            return;
        }
        for row in agents {
            let under = match (&row.parent, parent) {
                (Some(p), Some(parent)) => p == parent,
                (Some(p), None) => !agents.iter().any(|other| &other.id == p),
                (None, _) => false,
            };
            if under {
                out.push(row);
                walk(agents, Some(&row.id), out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    walk(agents, None, &mut out, 0);
    out
}

/// What a key did to the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListKey {
    None,
    /// Focus went back to the prompt; the key itself is for the prompt when `typed`.
    Blur {
        typed: bool,
    },
    /// Enter on a row: `None` is `main`.
    View(Option<String>),
    Kill(String),
}

#[derive(Default)]
pub struct AgentList {
    pub focused: bool,
    selected: usize,
    dismissed: HashSet<String>,
    rows: Vec<ListRow>,
}

impl AgentList {
    pub fn set_rows(&mut self, rows: Vec<ListRow>) {
        let id = self.selected_id();
        self.rows = rows;
        if let Some(at) = id.and_then(|id| self.rows.iter().position(|r| r.id == id)) {
            self.selected = at;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        if self.rows.is_empty() {
            self.focused = false;
        }
    }

    /// An agent that runs again may be listed again when it ends.
    pub fn forget_running(&mut self, agents: &[AgentRow]) {
        self.dismissed.retain(|id| {
            !agents
                .iter()
                .any(|row| row.id == *id && row.status.is_running())
        });
    }

    pub fn dismissed(&self) -> &HashSet<String> {
        &self.dismissed
    }

    pub fn has_rows(&self) -> bool {
        !self.rows.is_empty()
    }

    /// Moves the keyboard here, on `main`.
    pub fn focus(&mut self) {
        if self.has_rows() {
            self.focused = true;
            self.selected = 0;
        }
    }

    fn selected_id(&self) -> Option<Option<String>> {
        self.rows.get(self.selected).map(|row| row.id.clone())
    }

    /// Whether a row is waiting for its 30 seconds to run out, which needs a redraw tick.
    pub fn counts_time(&self) -> bool {
        self.has_rows()
    }

    /// `viewing` is the agent on screen, whose row `x` does not stop.
    pub fn handle_key(&mut self, key: KeyEvent, viewing: Option<&str>) -> ListKey {
        if key.kind == KeyEventKind::Release {
            return ListKey::None;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Up if plain => {
                if self.selected == 0 {
                    self.focused = false;
                    return ListKey::Blur { typed: false };
                }
                self.selected -= 1;
            }
            KeyCode::Down if plain => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
            }
            KeyCode::Esc => {
                self.focused = false;
                return ListKey::Blur { typed: false };
            }
            // Shift/Alt+Enter is the prompt's newline, not a view.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.focused = false;
                return ListKey::Blur { typed: true };
            }
            KeyCode::Enter => {
                if let Some(id) = self.selected_id() {
                    self.focused = false;
                    return ListKey::View(id);
                }
            }
            KeyCode::Char('x') if plain => {
                let Some(row) = self.rows.get(self.selected).cloned() else {
                    return ListKey::None;
                };
                match row.id {
                    // Typing an x, as in Claude Code: the row cannot be stopped.
                    None => {
                        self.focused = false;
                        return ListKey::Blur { typed: true };
                    }
                    Some(id) if viewing == Some(id.as_str()) => {
                        self.focused = false;
                        return ListKey::Blur { typed: true };
                    }
                    Some(id) => {
                        // A failed row is idle too, but only a Done one still has a live
                        // agent to kill.
                        if row.status.is_some_and(ChildStatus::is_running)
                            || (row.idle && row.status == Some(ChildStatus::Done))
                        {
                            return ListKey::Kill(id);
                        }
                        self.dismissed.insert(id);
                    }
                }
            }
            KeyCode::Char(_) if typed_char(&key).is_some() => {
                self.focused = false;
                return ListKey::Blur { typed: true };
            }
            _ => {}
        }
        ListKey::None
    }

    /// The hint and the rows, at most `max` lines, the selection kept in sight.
    pub fn lines(&self, viewing: Option<&str>, width: usize, max: usize) -> Vec<Line<'static>> {
        if self.rows.is_empty() || max == 0 {
            return Vec::new();
        }
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = Vec::new();
        let mut room = max;
        if self.focused && room > 1 {
            out.push(Line::styled(
                "↑/↓ select · Enter view · x stop · Esc back",
                dim,
            ));
            room -= 1;
        }
        let first = (self.selected + 1).saturating_sub(room);
        for (at, row) in self.rows.iter().enumerate().skip(first).take(room) {
            let viewed = row.id.as_deref() == viewing;
            let marker = if viewed { "●" } else { "◯" };
            let indent = "  ".repeat(row.depth.saturating_sub(1));
            let (head, spend) = match (&row.id, row.status) {
                (Some(id), Some(status)) => {
                    let lead = format!("{marker} {indent}{}", row.name);
                    let tag = format!(" ({id})");
                    let mut config = String::new();
                    if !row.model.is_empty() {
                        config.push_str(&format!(" {}", row.model));
                    }
                    if let Some(effort) = row.effort {
                        config.push_str(&format!(" effort {effort}"));
                    }
                    let rest = format!(
                        " {} {}",
                        row_label(status, row.idle),
                        format_age(row.elapsed)
                    );
                    let kind = format!(" {}", row.agent_type);
                    let spend = format!(" · {}", row.spend);
                    let fits = |parts: &[&str]| {
                        parts.iter().map(|p| p.chars().count()).sum::<usize>() <= width
                    };
                    // The id gives way first, then the model and effort, before the status,
                    // the age and the spend.
                    let (tag, config) = if fits(&[&lead, &tag, &kind, &config, &rest, &spend]) {
                        (tag, config)
                    } else if fits(&[&lead, &kind, &config, &rest, &spend]) {
                        (String::new(), config)
                    } else {
                        (String::new(), String::new())
                    };
                    (format!("{lead}{tag}{kind}{config}{rest}"), spend)
                }
                _ => (format!("{marker} main"), String::new()),
            };
            let writing = if (row.status.is_none()
                || row.status.is_some_and(ChildStatus::is_running))
                && row.writing > 0
            {
                format!(" · {}", writing_text(row.writing))
            } else {
                String::new()
            };
            // The suffix is a nicety: it never costs the row its columns or its unpriced figure.
            let (head, spend) = if head.chars().count()
                + spend.chars().count()
                + writing.chars().count()
                <= width
            {
                (head, spend + &writing)
            } else {
                (head, spend)
            };
            let spend_len = spend.chars().count();
            let shown: String = if head.chars().count() + spend_len <= width {
                head + &spend
            } else if row.unpriced && spend_len < width {
                // Cutting the tail would hide the unpriced figure, so the leading columns
                // give way instead.
                let keep = width - spend_len;
                let mut cut: String = head.chars().take(keep.saturating_sub(1)).collect();
                cut.push('…');
                cut + &spend
            } else {
                // An ordinary row, or an unpriced one whose spend alone fills the width: cut the end.
                (head + &spend).chars().take(width).collect()
            };
            let style = if self.focused && at == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            out.push(match &row.id {
                Some(id) => line_dimming_id(shown, id, style),
                None => Line::styled(shown, style),
            });
        }
        out
    }

    /// The number of lines [`lines`](Self::lines) wants before any cap.
    pub fn wanted_rows(&self) -> usize {
        if self.rows.is_empty() {
            0
        } else {
            self.rows.len() + usize::from(self.focused)
        }
    }

    pub fn rows(&self) -> &[ListRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_agents::budget::{Budget, Spend};
    use ratatui::crossterm::event::KeyCode;

    fn agent(id: &str, parent: &str, status: ChildStatus, now: Instant) -> AgentRow {
        AgentRow {
            id: id.into(),
            name: id.into(),
            parent: Some(parent.into()),
            agent_type: "worker".into(),
            model: String::new(),
            effort: None,
            status,
            depth: if parent == "main" { 1 } else { 2 },
            spend: Spend {
                input_tokens: 700,
                output_tokens: 500,
                ..Spend::default()
            },
            budget: Budget::default(),
            exhausted: None,
            started: now,
            ended: (!status.is_running()).then_some(now),
            paused: Duration::ZERO,
            writing: 0,
            idle: false,
        }
    }

    fn ids(rows: &[ListRow]) -> Vec<Option<&str>> {
        rows.iter().map(|row| row.id.as_deref()).collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn focused(rows: Vec<ListRow>) -> AgentList {
        let mut list = AgentList::default();
        list.set_rows(rows);
        list.focus();
        list
    }

    #[test]
    fn an_idle_agent_is_listed_only_while_it_owns_a_task_or_is_viewed() {
        let now = Instant::now();
        let mut idle = agent("agent-1", "main", ChildStatus::Done, now);
        idle.idle = true;
        let mut quiet = agent("agent-3", "main", ChildStatus::Done, now);
        quiet.idle = true;
        let done = agent("agent-2", "main", ChildStatus::Done, now);
        let owners = HashSet::from(["agent-1".to_string()]);
        let listed = rows(
            &[idle.clone(), quiet.clone(), done.clone()],
            None,
            &HashSet::new(),
            &owners,
            now,
            0,
        );
        let ids: Vec<_> = listed.iter().filter_map(|r| r.id.as_deref()).collect();
        assert_eq!(ids, ["agent-1"]);
        assert!(listed[1].idle);
        let viewed = rows(
            &[idle, quiet, done],
            Some("agent-3"),
            &HashSet::new(),
            &owners,
            now,
            0,
        );
        let ids: Vec<_> = viewed.iter().filter_map(|r| r.id.as_deref()).collect();
        assert_eq!(ids, ["agent-1", "agent-3"]);
    }

    #[test]
    fn a_mostly_unpriced_agent_lists_its_priced_tokens_then_the_unpriced_ones() {
        let now = Instant::now();
        let mut worker = agent("agent-1", "main", ChildStatus::Running, now);
        worker.spend = Spend {
            input_tokens: 4_800_000,
            output_tokens: 63_024,
            cost_usd: Some(0.0519),
            unpriced_tokens: 4_850_000,
        };
        let rows = rows(&[worker], None, &HashSet::new(), &HashSet::new(), now, 0);
        assert_eq!(rows[1].spend, "13k tok · $0.0519 · 4.9M unpriced tok");
        assert!(rows[1].unpriced);
        let mut list = AgentList::default();
        list.set_rows(rows);
        assert!(
            line_text(&list, 120, 1).ends_with("0s · 13k tok · $0.0519 · 4.9M unpriced tok"),
            "{}",
            line_text(&list, 120, 1)
        );
    }

    #[test]
    fn a_narrow_row_keeps_the_unpriced_segment() {
        let now = Instant::now();
        let mut worker = agent("agent-1", "main", ChildStatus::Running, now);
        worker.spend = Spend {
            input_tokens: 4_800_000,
            output_tokens: 63_024,
            cost_usd: Some(0.0519),
            unpriced_tokens: 4_850_000,
        };
        let mut rows = rows(&[worker], None, &HashSet::new(), &HashSet::new(), now, 0);
        rows[1].agent_type = "general-purpose".into();
        rows[1].status = Some(ChildStatus::WaitingOnPermission);
        let mut list = AgentList::default();
        list.set_rows(rows);
        let lines = list.lines(None, 80, 5);
        let text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.chars().count() <= 80, "{text}");
        assert!(
            text.ends_with(" · 13k tok · $0.0519 · 4.9M unpriced tok"),
            "{text}"
        );
        assert!(text.contains('…'), "{text}");
    }

    #[test]
    fn a_narrow_ordinary_row_is_cut_at_the_end() {
        let now = Instant::now();
        let mut worker = agent("agent-1", "main", ChildStatus::Running, now);
        worker.agent_type = "general-purpose".into();
        let mut list = AgentList::default();
        list.set_rows(rows(
            &[worker],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        let lines = list.lines(None, 20, 5);
        let text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◯ agent-1 general-pu", "{text}");
    }

    #[test]
    fn nothing_is_listed_while_no_subagent_is() {
        let now = Instant::now();
        assert!(rows(&[], None, &HashSet::new(), &HashSet::new(), now, 0).is_empty());
        let done = [agent("agent-1", "main", ChildStatus::Done, now)];
        assert!(rows(&done, None, &HashSet::new(), &HashSet::new(), now, 0).is_empty());
    }

    #[test]
    fn main_comes_first_and_a_child_sits_under_its_parent() {
        let now = Instant::now();
        let agents = [
            agent("agent-1", "main", ChildStatus::Running, now),
            agent("agent-2", "main", ChildStatus::Running, now),
            agent("agent-3", "agent-1", ChildStatus::Running, now),
        ];
        let listed = rows(&agents, None, &HashSet::new(), &HashSet::new(), now, 0);
        assert_eq!(
            ids(&listed),
            [None, Some("agent-1"), Some("agent-3"), Some("agent-2")]
        );
        assert_eq!(listed[2].depth, 2);
        assert_eq!(listed[1].spend, "1.2k tok");
    }

    #[test]
    fn a_done_agent_leaves_at_once_and_a_failed_one_after_thirty_seconds() {
        let started = Instant::now();
        let agents = [
            agent("agent-1", "main", ChildStatus::Done, started),
            agent("agent-2", "main", ChildStatus::Failed, started),
            agent("agent-3", "main", ChildStatus::Killed, started),
            agent("agent-4", "main", ChildStatus::BudgetExhausted, started),
            agent("agent-5", "main", ChildStatus::Running, started),
        ];
        let soon = started + Duration::from_secs(29);
        assert_eq!(
            ids(&rows(
                &agents,
                None,
                &HashSet::new(),
                &HashSet::new(),
                soon,
                0
            )),
            [
                None,
                Some("agent-2"),
                Some("agent-3"),
                Some("agent-4"),
                Some("agent-5")
            ]
        );
        let later = started + Duration::from_secs(30);
        assert_eq!(
            ids(&rows(
                &agents,
                None,
                &HashSet::new(),
                &HashSet::new(),
                later,
                0
            )),
            [None, Some("agent-5")]
        );
    }

    #[test]
    fn the_viewed_agent_stays_listed_and_a_dismissed_one_goes() {
        let now = Instant::now();
        let agents = [
            agent("agent-1", "main", ChildStatus::Done, now),
            agent("agent-2", "main", ChildStatus::Killed, now),
            agent("agent-3", "main", ChildStatus::Running, now),
        ];
        let dismissed = HashSet::from(["agent-2".to_string()]);
        assert_eq!(
            ids(&rows(
                &agents,
                Some("agent-1"),
                &dismissed,
                &HashSet::new(),
                now,
                0
            )),
            [None, Some("agent-1"), Some("agent-3")]
        );
    }

    #[test]
    fn a_modified_enter_is_handed_to_the_prompt_not_a_view() {
        let now = Instant::now();
        let agents = [agent("agent-1", "main", ChildStatus::Running, now)];
        for modifiers in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
            let mut list = focused(rows(
                &agents,
                None,
                &HashSet::new(),
                &HashSet::new(),
                now,
                0,
            ));
            let enter = KeyEvent::new(KeyCode::Enter, modifiers);
            assert_eq!(list.handle_key(enter, None), ListKey::Blur { typed: true });
            assert!(!list.focused);
        }
    }

    #[test]
    fn focusing_selects_main_and_up_from_the_first_row_gives_the_prompt_back() {
        let now = Instant::now();
        let agents = [agent("agent-1", "main", ChildStatus::Running, now)];
        let mut list = focused(rows(
            &agents,
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        assert!(list.focused);
        assert_eq!(list.handle_key(key(KeyCode::Down), None), ListKey::None);
        assert_eq!(list.handle_key(key(KeyCode::Up), None), ListKey::None);
        assert_eq!(
            list.handle_key(key(KeyCode::Up), None),
            ListKey::Blur { typed: false }
        );
        assert!(!list.focused);
        list.focus();
        assert_eq!(
            list.handle_key(key(KeyCode::Enter), None),
            ListKey::View(None)
        );
        list.focus();
        list.handle_key(key(KeyCode::Down), None);
        assert_eq!(
            list.handle_key(key(KeyCode::Enter), None),
            ListKey::View(Some("agent-1".into()))
        );
    }

    #[test]
    fn x_stops_a_running_agent_and_dismisses_an_ended_one() {
        let now = Instant::now();
        let agents = [
            agent("agent-1", "main", ChildStatus::Running, now),
            agent("agent-2", "main", ChildStatus::Failed, now),
        ];
        let mut list = focused(rows(
            &agents,
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        list.handle_key(key(KeyCode::Down), None);
        assert_eq!(
            list.handle_key(key(KeyCode::Char('x')), None),
            ListKey::Kill("agent-1".into())
        );
        list.handle_key(key(KeyCode::Down), None);
        assert_eq!(
            list.handle_key(key(KeyCode::Char('x')), None),
            ListKey::None
        );
        assert!(list.dismissed().contains("agent-2"));
    }

    #[test]
    fn x_dismisses_an_idle_failed_row_instead_of_killing_it() {
        let now = Instant::now();
        let mut failed = agent("agent-1", "main", ChildStatus::Failed, now);
        failed.idle = true;
        let mut done = agent("agent-2", "main", ChildStatus::Done, now);
        done.idle = true;
        let mut list = focused(rows(
            &[failed, done],
            None,
            &HashSet::new(),
            &HashSet::from(["agent-1".to_string(), "agent-2".to_string()]),
            now,
            0,
        ));
        list.handle_key(key(KeyCode::Down), None);
        assert_eq!(
            list.handle_key(key(KeyCode::Char('x')), None),
            ListKey::None
        );
        assert!(list.dismissed().contains("agent-1"));
        list.handle_key(key(KeyCode::Down), None);
        assert_eq!(
            list.handle_key(key(KeyCode::Char('x')), None),
            ListKey::Kill("agent-2".into())
        );
    }

    #[test]
    fn x_on_main_or_the_viewed_agent_is_typed_into_the_prompt() {
        let now = Instant::now();
        let agents = [agent("agent-1", "main", ChildStatus::Running, now)];
        let listed = rows(
            &agents,
            Some("agent-1"),
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        );
        let mut list = focused(listed.clone());
        assert_eq!(
            list.handle_key(key(KeyCode::Char('x')), Some("agent-1")),
            ListKey::Blur { typed: true }
        );
        let mut list = focused(listed);
        list.handle_key(key(KeyCode::Down), Some("agent-1"));
        assert_eq!(
            list.handle_key(key(KeyCode::Char('x')), Some("agent-1")),
            ListKey::Blur { typed: true }
        );
    }

    #[test]
    fn a_printable_key_and_escape_return_focus_and_only_the_key_is_typed() {
        let now = Instant::now();
        let agents = [agent("agent-1", "main", ChildStatus::Running, now)];
        let listed = rows(&agents, None, &HashSet::new(), &HashSet::new(), now, 0);
        let mut list = focused(listed.clone());
        assert_eq!(
            list.handle_key(key(KeyCode::Char('h')), None),
            ListKey::Blur { typed: true }
        );
        assert!(!list.focused);
        let mut list = focused(listed);
        assert_eq!(
            list.handle_key(key(KeyCode::Esc), None),
            ListKey::Blur { typed: false }
        );
    }

    fn named_list(width: usize) -> Vec<Line<'static>> {
        let now = Instant::now();
        let mut named = agent("agent-1", "main", ChildStatus::Running, now);
        named.name = "survey-tech-debt".into();
        let rows = rows(&[named], None, &HashSet::new(), &HashSet::new(), now, 0);
        let mut list = AgentList::default();
        list.set_rows(rows);
        list.lines(None, width, 10)
    }

    #[test]
    fn a_named_row_shows_the_name_and_a_dimmed_id() {
        let lines = named_list(80);
        assert!(
            lines[1]
                .to_string()
                .starts_with("◯ survey-tech-debt (agent-1) worker running"),
            "{lines:?}"
        );
        let dimmed: Vec<String> = lines[1]
            .spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::DIM))
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(dimmed, ["(agent-1)"]);
    }

    #[test]
    fn a_narrow_row_drops_the_id_before_the_status_and_the_age() {
        // Wide enough for the name, type, status, age and spend, but not for the id too.
        let lines = named_list(50);
        let text = lines[1].to_string();
        assert!(!text.contains("agent-1"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("1.2k tok"), "{text}");
        assert!(
            lines[1]
                .spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::DIM))
        );
    }

    #[test]
    fn the_viewed_row_is_marked_and_the_focused_one_reversed() {
        let now = Instant::now();
        let agents = [agent("agent-1", "main", ChildStatus::Running, now)];
        let mut list = focused(rows(
            &agents,
            Some("agent-1"),
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        list.handle_key(key(KeyCode::Down), Some("agent-1"));
        let lines = list.lines(Some("agent-1"), 80, 10);
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(text[0], "↑/↓ select · Enter view · x stop · Esc back");
        assert_eq!(text[1], "◯ main");
        assert!(
            text[2].starts_with("● agent-1 (agent-1) worker running"),
            "{text:?}"
        );
        assert!(lines[2].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!lines[1].style.add_modifier.contains(Modifier::REVERSED));
    }

    fn line_text(list: &AgentList, width: usize, at: usize) -> String {
        let lines = list.lines(None, width, 5);
        lines[at].spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn main_shows_its_writing_while_a_subagent_is_listed() {
        let now = Instant::now();
        let worker = agent("agent-1", "main", ChildStatus::Running, now);
        let mut list = AgentList::default();
        list.set_rows(rows(
            &[worker],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            4200,
        ));
        assert_eq!(line_text(&list, 80, 0), "● main · writing 4.2k chars");
        let mut list = AgentList::default();
        let worker = agent("agent-1", "main", ChildStatus::Running, now);
        list.set_rows(rows(
            &[worker],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        assert_eq!(line_text(&list, 80, 0), "● main");
        assert!(rows(&[], None, &HashSet::new(), &HashSet::new(), now, 900).is_empty());
    }

    #[test]
    fn a_running_subagent_shows_writing_and_an_ended_one_does_not() {
        let now = Instant::now();
        let mut running = agent("agent-1", "main", ChildStatus::Running, now);
        running.writing = 999;
        let mut failed = agent("agent-2", "main", ChildStatus::Failed, now);
        failed.writing = 999;
        let mut list = AgentList::default();
        list.set_rows(rows(
            &[running, failed],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        assert!(line_text(&list, 120, 1).ends_with(" · writing 999 chars"));
        assert!(!line_text(&list, 120, 2).contains("writing"));
    }

    #[test]
    fn a_narrow_row_drops_writing_before_the_unpriced_segment() {
        let now = Instant::now();
        let mut worker = agent("agent-1", "main", ChildStatus::Running, now);
        worker.spend.cost_usd = Some(0.01);
        worker.spend.unpriced_tokens = 5;
        worker.writing = 5000;
        let mut list = AgentList::default();
        list.set_rows(rows(
            &[worker],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        let text = line_text(&list, 60, 1);
        assert!(text.ends_with(" · 5 unpriced tok"), "{text}");
        assert!(!text.contains("writing"), "{text}");
    }

    fn configured(model: &str, effort: Option<Effort>, name: &str) -> AgentList {
        let now = Instant::now();
        let mut worker = agent("agent-1", "main", ChildStatus::Running, now);
        worker.name = name.into();
        worker.model = model.into();
        worker.effort = effort;
        let mut list = AgentList::default();
        list.set_rows(rows(
            &[worker],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        list
    }

    #[test]
    fn a_wide_row_shows_the_model_and_effort_after_the_type() {
        let list = configured("acct/sonnet", Some(Effort::High), "survey");
        assert_eq!(
            line_text(&list, 120, 1),
            "◯ survey (agent-1) worker acct/sonnet effort high running 0s · 1.2k tok"
        );
    }

    #[test]
    fn an_unset_effort_or_an_empty_model_is_omitted() {
        let list = configured("acct/sonnet", None, "survey");
        assert_eq!(
            line_text(&list, 120, 1),
            "◯ survey (agent-1) worker acct/sonnet running 0s · 1.2k tok"
        );
        let list = configured("", Some(Effort::Low), "survey");
        assert_eq!(
            line_text(&list, 120, 1),
            "◯ survey (agent-1) worker effort low running 0s · 1.2k tok"
        );
    }

    #[test]
    fn the_id_gives_way_before_the_model_and_effort_and_those_before_the_status() {
        let list = configured("acct/sonnet", Some(Effort::High), "survey");
        // Without the id: 61 columns.
        let no_id = "◯ survey worker acct/sonnet effort high running 0s · 1.2k tok";
        let width = no_id.chars().count();
        assert_eq!(line_text(&list, width, 1), no_id);
        let text = line_text(&list, width + 5, 1);
        assert_eq!(text, no_id);
        // One column short of that, both segments go together.
        let text = line_text(&list, width - 1, 1);
        assert_eq!(text, "◯ survey worker running 0s · 1.2k tok");
    }

    #[test]
    fn the_unpriced_front_cut_still_protects_the_spend_with_a_model_and_effort() {
        let now = Instant::now();
        let mut worker = agent("agent-1", "main", ChildStatus::Running, now);
        worker.model = "acct/sonnet".into();
        worker.effort = Some(Effort::Max);
        worker.spend.cost_usd = Some(0.01);
        worker.spend.unpriced_tokens = 5;
        let mut list = AgentList::default();
        list.set_rows(rows(
            &[worker],
            None,
            &HashSet::new(),
            &HashSet::new(),
            now,
            0,
        ));
        let text = line_text(&list, 40, 1);
        assert!(text.chars().count() <= 40, "{text}");
        assert!(text.ends_with(" · 5 unpriced tok"), "{text}");
        assert!(text.contains('…'), "{text}");
    }

    #[test]
    fn the_main_row_ignores_model_and_effort() {
        let list = configured("acct/sonnet", Some(Effort::High), "survey");
        assert_eq!(line_text(&list, 80, 0), "● main");
    }

    #[test]
    fn a_wrap_inside_the_id_tag_dims_both_halves() {
        let lines = wrapped_dimming_id("ab (agent-3) cd", "agent-3", 6, Style::default());
        let dimmed: String = lines
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| s.style.add_modifier.contains(Modifier::DIM))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(dimmed, "(agent-3)");
    }

    #[test]
    fn an_altgr_character_blurs_the_list_as_typed_text() {
        crate::input::with_altgr(true, || {
            let mut list = focused(vec![]);
            let result = list.handle_key(
                KeyEvent::new(
                    KeyCode::Char('@'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                ),
                None,
            );
            assert_eq!(result, ListKey::Blur { typed: true });
            assert!(!list.focused);
        });
    }
}
