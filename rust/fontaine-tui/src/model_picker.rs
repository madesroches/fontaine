//! The `/model` and `/guardian-model` overlay: models grouped under their account, the
//! recently used ones first, narrowed by a fuzzy filter typed into it.

use fontaine_core::config::AccountKind;
use fontaine_core::models::{AccountListing, ModelRef};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::theme::Theme;

/// The most rows (headings included) the body shows at once.
const MAX_ROWS: usize = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerPurpose {
    Main,
    Guardian,
}

/// What the human did with the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerKey {
    None,
    Close,
    /// Switch to `<account>/<model>`.
    Pick(PickerPurpose, String),
}

/// One account's models, or why they could not be listed.
pub struct Group {
    account: String,
    kind: AccountKind,
    models: Result<Vec<String>, String>,
}

impl Group {
    pub fn new(account: String, kind: AccountKind, models: Result<Vec<String>, String>) -> Group {
        Group {
            account,
            kind,
            models,
        }
    }
}

impl From<&AccountListing> for Group {
    fn from(listing: &AccountListing) -> Group {
        Group::new(
            listing.account.clone(),
            listing.kind,
            listing.models.clone(),
        )
    }
}

enum Row {
    Heading(String),
    Model {
        label: String,
        reference: String,
        current: bool,
    },
    Failed(String),
}

pub struct ModelPicker {
    purpose: PickerPurpose,
    header: String,
    current: Option<ModelRef>,
    recent: Vec<ModelRef>,
    groups: Vec<Group>,
    query: String,
    /// Index into the visible model rows.
    selected: usize,
}

/// A case-insensitive subsequence match of `query` in `text`, scored higher for contiguous runs
/// and for matches that start the model id or follow a separator. `None` when it does not match.
fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let mut wanted = query.chars().flat_map(char::to_lowercase).peekable();
    let mut score = 0;
    let mut previous: Option<char> = None;
    let mut run = false;
    for c in text.chars() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        if wanted.peek() == Some(&lower) {
            wanted.next();
            score += 1;
            if run {
                score += 3;
            }
            if previous.is_none_or(|p| matches!(p, '/' | '-' | '_' | '.')) {
                score += 5;
            }
            run = true;
        } else {
            run = false;
        }
        previous = Some(c);
    }
    wanted.peek().is_none().then_some(score)
}

/// Whether the listed id `row` is the model `current` names; providers accept aliases such as
/// Ollama's bare name for its `:latest` tag.
fn is_current(kind: AccountKind, account: &str, row: &str, current: Option<&ModelRef>) -> bool {
    current.is_some_and(|c| {
        c.account == account && kind.provider().lists(&[row.to_string()], &c.model)
    })
}

impl ModelPicker {
    /// `header` is the line above the rows; `current` is marked wherever it appears.
    pub fn new(
        purpose: PickerPurpose,
        header: String,
        current: Option<ModelRef>,
        recent: Vec<ModelRef>,
        mut groups: Vec<Group>,
    ) -> ModelPicker {
        groups.sort_by(|a, b| a.account.cmp(&b.account));
        let mut picker = ModelPicker {
            purpose,
            header,
            current,
            recent,
            groups,
            query: String::new(),
            selected: 0,
        };
        picker.selected = picker.current_index().unwrap_or(0);
        picker
    }

    fn current_index(&self) -> Option<usize> {
        self.visible()
            .iter()
            .filter(|row| matches!(row, Row::Model { .. }))
            .position(|row| matches!(row, Row::Model { current: true, .. }))
    }

    pub fn purpose(&self) -> PickerPurpose {
        self.purpose
    }

    fn recent_rows(&self) -> Vec<(i32, Row)> {
        let mut out = Vec::new();
        for entry in &self.recent {
            let Some(group) = self.groups.iter().find(|g| g.account == entry.account) else {
                continue;
            };
            // A failed listing cannot vouch against the entry; the switch validates it anyway.
            if let Ok(models) = &group.models
                && !group.kind.provider().lists(models, &entry.model)
            {
                continue;
            }
            let reference = entry.to_string();
            let Some(score) = fuzzy(&self.query, &reference) else {
                continue;
            };
            let current = is_current(
                group.kind,
                &entry.account,
                &entry.model,
                self.current.as_ref(),
            );
            out.push((
                score,
                Row::Model {
                    label: reference.clone(),
                    reference,
                    current,
                },
            ));
        }
        out
    }

    fn visible(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut recent = self.recent_rows();
        recent.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        if !recent.is_empty() {
            rows.push(Row::Heading("Recent".to_string()));
            rows.extend(recent.into_iter().map(|(_, row)| row));
        }
        for group in &self.groups {
            match &group.models {
                Ok(models) => {
                    let mut hits: Vec<(i32, &String)> = models
                        .iter()
                        .filter_map(|model| {
                            fuzzy(&self.query, &format!("{}/{model}", group.account))
                                .map(|score| (score, model))
                        })
                        .collect();
                    if hits.is_empty() {
                        continue;
                    }
                    hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
                    rows.push(Row::Heading(group.account.clone()));
                    for (_, model) in hits {
                        rows.push(Row::Model {
                            label: model.clone(),
                            reference: format!("{}/{model}", group.account),
                            current: is_current(
                                group.kind,
                                &group.account,
                                model,
                                self.current.as_ref(),
                            ),
                        });
                    }
                }
                Err(reason) => {
                    if fuzzy(&self.query, &group.account).is_some() {
                        rows.push(Row::Heading(group.account.clone()));
                        rows.push(Row::Failed(format!("could not list: {reason}")));
                    }
                }
            }
        }
        rows
    }

    fn model_count(rows: &[Row]) -> usize {
        rows.iter()
            .filter(|row| matches!(row, Row::Model { .. }))
            .count()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerKey {
        if key.kind == KeyEventKind::Release {
            return PickerKey::None;
        }
        match key.code {
            KeyCode::Esc => return PickerKey::Close,
            KeyCode::Enter => {
                // The current model is picked too: that is how a model given by `--model`, or
                // one the settings file no longer names, becomes the saved default.
                let mut picked = self.visible().into_iter().filter_map(|row| match row {
                    Row::Model { reference, .. } => Some(reference),
                    _ => None,
                });
                return match picked.nth(self.selected) {
                    Some(reference) => PickerKey::Pick(self.purpose, reference),
                    None => PickerKey::None,
                };
            }
            KeyCode::Up | KeyCode::Down => {
                let n = Self::model_count(&self.visible());
                if n > 0 {
                    self.selected = if key.code == KeyCode::Up {
                        (self.selected + n - 1) % n
                    } else {
                        (self.selected + 1) % n
                    };
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        PickerKey::None
    }

    /// `max_rows` bounds the rows under the header (headings included), so a short terminal
    /// still shows the header and the selection.
    pub fn body(&self, width: usize, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let clip = |text: String| -> String { text.chars().take(width).collect() };
        let mut out = vec![Line::raw(clip(self.header.clone()))];
        let rows = self.visible();
        if rows.is_empty() {
            out.push(Line::styled(
                if self.query.is_empty() {
                    "no models to choose from".to_string()
                } else {
                    clip(format!("no model matches \"{}\"", self.query))
                },
                dim,
            ));
            return out;
        }
        let mut seen = 0;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected_row = 0;
        let mut heading = None;
        let mut selected_heading = None;
        for (i, row) in rows.iter().enumerate() {
            match row {
                Row::Heading(name) => {
                    heading = Some(i);
                    lines.push(Line::styled(clip(name.clone()), bold.fg(theme.heading)));
                }
                Row::Failed(reason) => lines.push(Line::styled(clip(format!("  {reason}")), dim)),
                Row::Model { label, current, .. } => {
                    let chosen = seen == self.selected;
                    let mark = if *current { " (current)" } else { "" };
                    let style = if chosen {
                        selected_row = i;
                        selected_heading = heading;
                        bold.add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::styled(clip(format!("  {label}{mark}")), style));
                    seen += 1;
                }
            }
        }
        let window = max_rows.clamp(1, MAX_ROWS);
        let mut start = 0;
        if lines.len() > window {
            start = selected_row
                .saturating_sub(window / 2)
                .min(lines.len() - window);
            if let Some(h) = selected_heading
                && selected_row - h < window
            {
                start = start.min(h);
            }
        }
        out.extend(lines.into_iter().skip(start).take(window));
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![
            Line::raw(format!("filter: {}▏", self.query)),
            Line::styled(
                "type to filter · ↑/↓ select · Enter switch · Esc cancel",
                dim,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn model(text: &str) -> ModelRef {
        ModelRef::parse(text).unwrap()
    }

    fn kind(name: &str) -> AccountKind {
        AccountKind::named(name)
    }

    fn ok(models: &[&str]) -> Result<Vec<String>, String> {
        Ok(models.iter().map(ToString::to_string).collect())
    }

    fn picker(recent: &[&str], groups: Vec<Group>) -> ModelPicker {
        ModelPicker::new(
            PickerPurpose::Main,
            "header".to_string(),
            Some(model("cloud/sonnet")),
            recent.iter().map(|r| model(r)).collect(),
            groups,
        )
    }

    fn two_accounts() -> Vec<Group> {
        vec![
            Group::new(
                "local".into(),
                kind("ollama"),
                ok(&["llama3:latest", "qwen3:4b"]),
            ),
            Group::new(
                "cloud".into(),
                kind("openai-compatible"),
                ok(&["haiku", "sonnet", "opus"]),
            ),
        ]
    }

    fn text(picker: &ModelPicker) -> Vec<String> {
        picker
            .body(80, MAX_ROWS, &Theme::default())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn type_text(picker: &mut ModelPicker, query: &str) {
        for c in query.chars() {
            picker.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn fuzzy_matches_subsequences_case_insensitively() {
        assert!(fuzzy("", "anything").is_some());
        assert!(fuzzy("clso", "cloud/sonnet").is_some());
        assert!(fuzzy("CLOUD", "cloud/sonnet").is_some());
        assert!(fuzzy("sx", "cloud/sonnet").is_none());
        assert!(fuzzy("tt", "cloud/sonne").is_none());
    }

    #[test]
    fn fuzzy_ranks_runs_and_boundaries_higher() {
        let run = fuzzy("son", "cloud/sonnet").unwrap();
        let scattered = fuzzy("son", "sxoxn/zzzzzz").unwrap();
        assert!(run > scattered);
        let boundary = fuzzy("o", "a/opus").unwrap();
        let inside = fuzzy("o", "a/zoom").unwrap();
        assert!(boundary > inside);
    }

    #[test]
    fn filtering_keeps_the_headings_of_groups_with_matches() {
        let mut p = picker(&[], two_accounts());
        let all = text(&p);
        assert!(all.contains(&"cloud".to_string()));
        assert!(all.contains(&"local".to_string()));
        type_text(&mut p, "qwen");
        let rows = text(&p);
        assert!(rows.contains(&"local".to_string()), "{rows:?}");
        assert!(!rows.contains(&"cloud".to_string()), "{rows:?}");
        assert!(rows.contains(&"  qwen3:4b".to_string()), "{rows:?}");
    }

    #[test]
    fn matches_are_ordered_by_score() {
        let mut p = picker(&[], two_accounts());
        type_text(&mut p, "cloud/o");
        let rows = text(&p);
        let opus = rows.iter().position(|r| r == "  opus").unwrap();
        let sonnet = rows.iter().position(|r| r.starts_with("  sonnet"));
        assert!(sonnet.is_none_or(|s| opus < s), "{rows:?}");
    }

    #[test]
    fn a_failed_account_shows_its_reason_unless_the_query_excludes_it() {
        let mut groups = two_accounts();
        groups.push(Group::new(
            "broken".into(),
            kind("openai-compatible"),
            Err("500 boom".into()),
        ));
        let mut p = picker(&[], groups);
        let rows = text(&p);
        assert!(rows.contains(&"broken".to_string()), "{rows:?}");
        assert!(
            rows.contains(&"  could not list: 500 boom".to_string()),
            "{rows:?}"
        );
        type_text(&mut p, "brok");
        assert!(text(&p).contains(&"broken".to_string()));
        type_text(&mut p, "zzz");
        assert!(!text(&p).contains(&"broken".to_string()));
    }

    #[test]
    fn up_and_down_skip_headings_and_wrap() {
        let mut p = ModelPicker::new(
            PickerPurpose::Main,
            "header".to_string(),
            None,
            vec![],
            two_accounts(),
        );
        // cloud: haiku, sonnet, opus; local: llama3:latest, qwen3:4b
        let pick = |p: &mut ModelPicker| match p.handle_key(key(KeyCode::Enter)) {
            PickerKey::Pick(_, r) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(pick(&mut p), "cloud/haiku");
        p.handle_key(key(KeyCode::Up));
        assert_eq!(pick(&mut p), "local/qwen3:4b");
        p.handle_key(key(KeyCode::Down));
        assert_eq!(pick(&mut p), "cloud/haiku");
        for _ in 0..3 {
            p.handle_key(key(KeyCode::Down));
        }
        assert_eq!(pick(&mut p), "local/llama3:latest");
    }

    #[test]
    fn editing_the_query_selects_the_first_match() {
        let mut p = picker(&[], two_accounts());
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Down));
        type_text(&mut p, "o");
        let PickerKey::Pick(_, first) = p.handle_key(key(KeyCode::Enter)) else {
            panic!("a match is selected");
        };
        let rows = text(&p);
        let first_model = rows.iter().find(|r| r.starts_with("  ")).unwrap();
        assert!(first.ends_with(first_model.trim().trim_end_matches(" (current)")));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Backspace));
        let PickerKey::Pick(_, again) = p.handle_key(key(KeyCode::Enter)) else {
            panic!("a match is selected");
        };
        assert_eq!(again, "cloud/haiku");
    }

    #[test]
    fn enter_picks_the_full_reference_and_nothing_when_nothing_matches() {
        let mut p = picker(&[], two_accounts());
        type_text(&mut p, "qwen");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            PickerKey::Pick(PickerPurpose::Main, "local/qwen3:4b".into())
        );
        type_text(&mut p, "zzz");
        assert_eq!(p.handle_key(key(KeyCode::Enter)), PickerKey::None);
        assert!(
            text(&p).iter().any(|l| l == "no model matches \"qwenzzz\""),
            "{:?}",
            text(&p)
        );
    }

    #[test]
    fn escape_closes_and_jkq_are_typed() {
        let mut p = picker(&[], two_accounts());
        assert_eq!(p.handle_key(key(KeyCode::Char('j'))), PickerKey::None);
        assert_eq!(p.handle_key(key(KeyCode::Char('q'))), PickerKey::None);
        assert_eq!(p.query, "jq");
        assert_eq!(p.handle_key(key(KeyCode::Esc)), PickerKey::Close);
    }

    #[test]
    fn control_and_alt_characters_are_not_typed_but_shift_is() {
        let mut p = picker(&[], two_accounts());
        p.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        p.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        p.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        assert_eq!(p.query, "X");
    }

    #[test]
    fn the_current_model_is_marked_in_recent_and_in_its_group() {
        let p = picker(&["cloud/sonnet", "cloud/haiku"], two_accounts());
        let rows = text(&p);
        assert_eq!(rows[1], "Recent");
        assert_eq!(rows[2], "  cloud/sonnet (current)");
        assert_eq!(rows[3], "  cloud/haiku");
        assert!(rows.contains(&"  sonnet (current)".to_string()), "{rows:?}");
    }

    #[test]
    fn a_bare_ollama_name_matches_its_latest_tag() {
        let p = ModelPicker::new(
            PickerPurpose::Main,
            "h".into(),
            Some(model("local/llama3")),
            vec![model("local/llama3")],
            two_accounts(),
        );
        let rows = text(&p);
        assert!(
            rows.contains(&"  local/llama3 (current)".to_string()),
            "{rows:?}"
        );
        assert!(
            rows.contains(&"  llama3:latest (current)".to_string()),
            "{rows:?}"
        );
    }

    #[test]
    fn recent_drops_models_no_longer_offered_but_keeps_those_of_failed_accounts() {
        let mut groups = two_accounts();
        groups.push(Group::new(
            "broken".into(),
            kind("openai-compatible"),
            Err("down".into()),
        ));
        let p = picker(
            &["cloud/gone", "broken/x", "removed/y", "cloud/opus"],
            groups,
        );
        let rows = text(&p);
        assert!(rows.contains(&"  broken/x".to_string()), "{rows:?}");
        assert!(rows.contains(&"  cloud/opus".to_string()), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("gone")), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("removed")), "{rows:?}");
    }

    #[test]
    fn guardian_recent_keeps_only_candidates() {
        let candidates = vec![Group::new(
            "cloud".into(),
            kind("openai-compatible"),
            ok(&["judge-b"]),
        )];
        let p = ModelPicker::new(
            PickerPurpose::Guardian,
            "h".into(),
            None,
            vec![model("cloud/judge-a"), model("cloud/judge-b")],
            candidates,
        );
        let rows = text(&p);
        assert!(rows.contains(&"  cloud/judge-b".to_string()), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("judge-a")), "{rows:?}");
    }

    #[test]
    fn the_window_follows_the_selection_in_a_long_list() {
        let models: Vec<String> = (0..60).map(|i| format!("m{i:02}")).collect();
        let group = Group::new("big".into(), kind("openai-compatible"), Ok(models));
        let mut p = picker(&[], vec![group]);
        let rows = p.body(80, MAX_ROWS, &Theme::default());
        assert_eq!(rows.len(), 1 + MAX_ROWS);
        assert_eq!(text(&p)[1], "big");
        for _ in 0..40 {
            p.handle_key(key(KeyCode::Down));
        }
        let rows = text(&p);
        assert_eq!(rows.len(), 1 + MAX_ROWS);
        assert!(rows.contains(&"  m40".to_string()), "{rows:?}");
        p.handle_key(key(KeyCode::Up));
        p.handle_key(key(KeyCode::Up));
        assert!(text(&p).contains(&"  m38".to_string()));
        // Wrapping to the last model scrolls to the end.
        let mut p = picker(
            &[],
            vec![Group::new(
                "big".into(),
                kind("openai-compatible"),
                Ok((0..60).map(|i| format!("m{i:02}")).collect()),
            )],
        );
        p.handle_key(key(KeyCode::Up));
        assert!(text(&p).contains(&"  m59".to_string()));
    }

    #[test]
    fn a_short_terminal_keeps_the_header_and_selection() {
        let group = Group::new(
            "big".into(),
            kind("openai-compatible"),
            Ok((0..60).map(|i| format!("m{i:02}")).collect()),
        );
        let mut p = picker(&[], vec![group]);
        for _ in 0..30 {
            p.handle_key(key(KeyCode::Down));
        }
        let rows: Vec<String> = p
            .body(80, 3, &Theme::default())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], "header");
        assert!(rows.contains(&"  m30".to_string()), "{rows:?}");
    }

    #[test]
    fn open_preselects_the_current_model_and_enter_picks_it() {
        let mut p = picker(&[], two_accounts());
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            PickerKey::Pick(PickerPurpose::Main, "cloud/sonnet".into())
        );
    }

    #[test]
    fn the_footer_shows_the_filter() {
        let mut p = picker(&[], two_accounts());
        type_text(&mut p, "ab");
        let footer: String = p.footer()[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(footer, "filter: ab▏");
    }
}
