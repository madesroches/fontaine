//! The `/config` overlay: every settings key with its effective value and the scope that
//! supplied it. A few preference keys can be changed from here; the change always goes to the
//! user settings file, never to a project file.

use std::collections::BTreeMap;
use std::path::Path;

use fontaine_core::config::{Config, Slot};
use fontaine_core::config_catalog::{
    Applies, Edit, KEYS, KeyRow, ModelSlot, Running, winning_slot,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use toml_edit::Value;

use crate::theme::Theme;

/// The widest a value column grows before it is cut short.
const MAX_VALUE: usize = 32;

/// What the human did with the panel.
#[derive(Clone, Debug)]
pub enum ConfigKey {
    None,
    Close,
    Save { path: &'static str, value: Value },
    Reset(&'static str),
    OpenPicker(ModelSlot),
}

// `toml_edit::Value` has no equality of its own; the text it renders to is what gets written.
impl PartialEq for ConfigKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ConfigKey::None, ConfigKey::None) | (ConfigKey::Close, ConfigKey::Close) => true,
            (
                ConfigKey::Save { path, value },
                ConfigKey::Save {
                    path: other_path,
                    value: other_value,
                },
            ) => path == other_path && value.to_string() == other_value.to_string(),
            (ConfigKey::Reset(a), ConfigKey::Reset(b)) => a == b,
            (ConfigKey::OpenPicker(a), ConfigKey::OpenPicker(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for ConfigKey {}

struct PanelRow {
    key: &'static KeyRow,
    effective: String,
    source: String,
    /// The label of a saved value that applies from the next launch; `unset` after a reset.
    pending: Option<String>,
    /// The label of a Save or Reset emitted but not yet reported back, so a repeated Enter moves
    /// on from it instead of asking for the same change again.
    cycle: Option<String>,
    /// A scope above the user file sets this key, so `pending` is not what the next launch runs.
    overridden: bool,
}

pub struct ConfigPanel {
    rows: Vec<PanelRow>,
    selected: usize,
    filter: String,
    /// The digits typed for a `PositiveInt` row while its field is open.
    field: Option<String>,
    status: Option<String>,
    /// Where saved changes go, for the footer.
    user_file: Option<String>,
}

/// The label a saved value carries in the `pending` column.
pub fn value_label(value: &Value) -> String {
    match value {
        Value::String(s) => s.value().clone(),
        Value::Integer(n) => n.value().to_string(),
        Value::Boolean(b) => b.value().to_string(),
        other => other.to_string().trim().to_string(),
    }
}

/// What a change written to the user file means for the running session. `effective` is the
/// value a next-launch row keeps running on, when known.
pub fn saved_status(
    applies: Applies,
    overridden_by: Option<Slot>,
    file: &str,
    effective: Option<&str>,
) -> String {
    match (applies, overridden_by) {
        (Applies::Live, None) => format!("saved to {file}"),
        (Applies::Live, Some(scope)) => format!(
            "applied now; {} sets this key and wins from the next launch",
            scope.label()
        ),
        (Applies::NextLaunch, None) => format!("applies from the next launch; saved to {file}"),
        (Applies::NextLaunch, Some(scope)) => {
            let stays = effective.map_or_else(String::new, |v| format!(", so the value stays {v}"));
            format!("saved, but {} sets this key and wins{stays}", scope.label())
        }
    }
}

impl ConfigPanel {
    /// `pending` maps a key path to the label of a value saved earlier in this session that
    /// applies from the next launch.
    pub fn new(
        config: &Config,
        running: &Running,
        user_file: Option<&Path>,
        pending: &BTreeMap<String, String>,
    ) -> ConfigPanel {
        let rows = KEYS
            .iter()
            .map(|key| PanelRow {
                key,
                effective: (key.effective)(config, running),
                source: key.source_label(config),
                pending: pending.get(key.path).cloned(),
                cycle: None,
                overridden: winning_slot(config, key.path).is_some_and(|slot| slot != Slot::User),
            })
            .collect();
        ConfigPanel {
            rows,
            selected: 0,
            filter: String::new(),
            field: None,
            status: None,
            user_file: user_file.map(|p| p.display().to_string()),
        }
    }

    fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.key.path.contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_row(&self) -> Option<&PanelRow> {
        self.visible().get(self.selected).map(|&i| &self.rows[i])
    }

    /// The path of the row the cursor is on.
    pub fn selected_path(&self) -> Option<&'static str> {
        self.selected_row().map(|r| r.key.path)
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ConfigKey {
        if key.kind == KeyEventKind::Release {
            return ConfigKey::None;
        }
        if self.field.is_some() {
            return self.field_key(key);
        }
        match key.code {
            KeyCode::Esc => return ConfigKey::Close,
            KeyCode::Up | KeyCode::Down => {
                let n = self.visible().len();
                if n > 0 {
                    self.selected = if key.code == KeyCode::Up {
                        (self.selected + n - 1) % n
                    } else {
                        (self.selected + 1) % n
                    };
                }
            }
            KeyCode::Enter => return self.enter(),
            KeyCode::Delete => {
                if let Some(row) = self.selected_row()
                    && matches!(row.key.edit, Some(Edit::Choice(_) | Edit::PositiveInt))
                {
                    return ConfigKey::Reset(row.key.path);
                }
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_some() {
                    self.selected = 0;
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        ConfigKey::None
    }

    fn enter(&mut self) -> ConfigKey {
        let Some(&index) = self.visible().get(self.selected) else {
            return ConfigKey::None;
        };
        let row = &mut self.rows[index];
        let key = row.key;
        match &key.edit {
            None => {
                self.status = Some(match &self.user_file {
                    Some(file) => {
                        format!("read-only here; edit the settings files, such as {file}")
                    }
                    None => "read-only here; edit the settings files".to_string(),
                });
                ConfigKey::None
            }
            Some(Edit::Picker(slot)) => ConfigKey::OpenPicker(*slot),
            Some(Edit::PositiveInt) => {
                self.field = Some(String::new());
                self.status = None;
                ConfigKey::None
            }
            Some(Edit::Choice(choices)) => {
                let choices = choices();
                let current = row
                    .cycle
                    .as_deref()
                    .or(row.pending.as_deref())
                    .unwrap_or(&row.effective);
                let next = choices
                    .iter()
                    .position(|(label, _)| *label == current)
                    .map_or(0, |i| (i + 1) % choices.len());
                match choices.into_iter().nth(next) {
                    Some((label, Some(value))) => {
                        row.cycle = Some(label.to_string());
                        ConfigKey::Save {
                            path: key.path,
                            value,
                        }
                    }
                    Some((label, None)) => {
                        row.cycle = Some(label.to_string());
                        ConfigKey::Reset(key.path)
                    }
                    None => ConfigKey::None,
                }
            }
        }
    }

    fn field_key(&mut self, key: KeyEvent) -> ConfigKey {
        let Some(path) = self.selected_path() else {
            self.field = None;
            return ConfigKey::None;
        };
        let Some(typed) = self.field.as_mut() else {
            return ConfigKey::None;
        };
        match key.code {
            KeyCode::Esc => self.field = None,
            KeyCode::Backspace => {
                typed.pop();
            }
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                typed.push(c);
            }
            KeyCode::Enter => {
                if typed.is_empty() {
                    self.field = None;
                    return ConfigKey::Reset(path);
                }
                return match typed.parse::<i64>() {
                    Ok(n) if n > 0 => {
                        self.field = None;
                        ConfigKey::Save {
                            path,
                            value: Value::from(n),
                        }
                    }
                    Ok(_) => {
                        self.status = Some("enter a whole number above zero".to_string());
                        ConfigKey::None
                    }
                    Err(_) => {
                        self.status = Some("number too large".to_string());
                        ConfigKey::None
                    }
                };
            }
            _ => {}
        }
        ConfigKey::None
    }

    fn row_mut(&mut self, path: &str) -> Option<&mut PanelRow> {
        self.rows.iter_mut().find(|r| r.key.path == path)
    }

    /// Records a change that was written to the user file. `label` is the saved value as the
    /// row shows it (`unset` after a reset), `live_value` the value a live row now runs on, and
    /// `overridden_by` the scope that outranks the user file for this key. Returns the label to
    /// remember as pending, when the change waits for the next launch.
    pub fn saved(
        &mut self,
        path: &str,
        label: &str,
        live_value: Option<&str>,
        overridden_by: Option<Slot>,
    ) -> Option<String> {
        let file = self.user_file.clone().unwrap_or_default();
        let row = self.row_mut(path)?;
        if row.cycle.as_deref() == Some(label) {
            row.cycle = None;
        }
        let live = row.key.applies == Applies::Live;
        if live && let Some(value) = live_value {
            row.effective = value.to_string();
        }
        let unchanged = !live && overridden_by.is_none() && row.effective == label;
        let status = if unchanged {
            format!("saved to {file}; unchanged from the running value")
        } else {
            saved_status(
                row.key.applies,
                overridden_by,
                &file,
                (!live).then_some(row.effective.as_str()),
            )
        };
        let pending = (!live && !unchanged).then(|| label.to_string());
        row.pending = pending.clone();
        self.status = Some(status);
        pending
    }

    fn forget_cycles(&mut self) {
        for row in &mut self.rows {
            row.cycle = None;
        }
    }

    /// Records that a reset found the key already absent from the user file.
    pub fn not_set(&mut self) {
        self.forget_cycles();
        let file = self.user_file.clone().unwrap_or_default();
        self.status = Some(format!("not set in {file}"));
    }

    /// Records that a change could not be written.
    pub fn failed(&mut self, why: &str) {
        self.forget_cycles();
        self.status = Some(format!("not saved: {why}"));
    }

    pub fn field(&self) -> (&'static str, String) {
        match &self.field {
            Some(typed) => ("value: ", typed.clone()),
            None => ("filter: ", self.filter.clone()),
        }
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let (prompt, typed) = self.field();
        let mut out = vec![Line::raw(format!("{prompt}{typed}▏"))];
        out.push(Line::styled(
            if self.field.is_some() {
                "digits only · Enter saves (empty resets) · Esc cancels"
            } else {
                "type to filter · ↑/↓ select · Enter change · Del reset · Esc close"
            },
            dim,
        ));
        if let Some(status) = &self.status {
            out.push(Line::styled(status.clone(), dim));
        } else if let Some(row) = self.selected_row() {
            let note = match (&row.key.edit, &self.user_file) {
                (Some(_), Some(file)) => format!("changes are saved to {file}"),
                (Some(_), None) => "there is no user settings file to save to".to_string(),
                (None, Some(file)) => {
                    format!("read-only here; edit {file} or the project settings")
                }
                (None, None) => "read-only here; edit the settings files".to_string(),
            };
            out.push(Line::styled(note, dim));
        }
        out
    }

    fn value_text(row: &PanelRow) -> String {
        let shown = match &row.pending {
            Some(label) if row.overridden => format!("{} (saved: {label})", row.effective),
            Some(label) => format!("{} (next launch: {label})", row.effective),
            None => row.effective.clone(),
        };
        clip(&shown, MAX_VALUE)
    }

    pub fn body(&self, width: usize, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        self.body_as(width, max_rows, theme, false)
    }

    /// `linear` renders one plain line per row, without columns, for a screen reader.
    pub fn body_as(
        &self,
        width: usize,
        max_rows: usize,
        theme: &Theme,
        linear: bool,
    ) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let visible = self.visible();
        if visible.is_empty() {
            return vec![Line::styled(
                clip(&format!("no setting matches \"{}\"", self.filter), width),
                dim,
            )];
        }
        let path_width = visible
            .iter()
            .map(|&i| self.rows[i].key.path.len())
            .max()
            .unwrap_or(0);
        let value_width = visible
            .iter()
            .map(|&i| Self::value_text(&self.rows[i]).chars().count())
            .max()
            .unwrap_or(0);
        let mut out = vec![Line::styled(
            if linear {
                "Settings".to_string()
            } else {
                "Settings · key, effective value, source".to_string()
            },
            bold.fg(theme.heading),
        )];
        let window = max_rows.saturating_sub(1).max(1);
        let start = if visible.len() > window {
            self.selected
                .saturating_sub(window / 2)
                .min(visible.len() - window)
        } else {
            0
        };
        for (n, &i) in visible.iter().enumerate().skip(start).take(window) {
            let row = &self.rows[i];
            let value = Self::value_text(row);
            let editable = if row.key.edit.is_some() { " ✎" } else { "" };
            let text = if linear {
                let edit = if row.key.edit.is_some() {
                    ", editable"
                } else {
                    ""
                };
                format!("  {}: {value}, from {}{edit}", row.key.path, row.source)
            } else {
                format!(
                    "  {:<path_width$}  {value:<value_width$}  {}{editable}",
                    row.key.path, row.source
                )
            };
            let style = if n == self.selected {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            out.push(Line::styled(clip(&text, width), style));
        }
        out
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fontaine_core::config::{LoadParams, load};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn config(user: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.toml"), user).unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        load(&LoadParams {
            launch_dir: &work,
            config_dir: &config_dir,
            data_dir: dir.path(),
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap()
    }

    fn panel_with(user: &str, pending: &BTreeMap<String, String>) -> ConfigPanel {
        let config = config(user);
        let running = Running {
            theme: config.tui.theme.as_str(),
            main_model: "work/opus",
            guardian_model: None,
        };
        ConfigPanel::new(
            &config,
            &running,
            Some(&PathBuf::from("/home/u/config.toml")),
            pending,
        )
    }

    fn panel() -> ConfigPanel {
        panel_with("", &BTreeMap::new())
    }

    fn type_text(panel: &mut ConfigPanel, text: &str) {
        for c in text.chars() {
            panel.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn select(panel: &mut ConfigPanel, path: &str) {
        panel.handle_key(key(KeyCode::Esc));
        panel.filter = path.to_string();
        panel.selected = 0;
        assert_eq!(
            panel.selected_path(),
            Some(KEYS.iter().find(|k| k.path == path).unwrap().path)
        );
    }

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn typing_filters_by_path_including_spaces() {
        let mut p = panel();
        type_text(&mut p, "guardian.composition");
        assert!(p.visible().len() >= 5);
        assert!(
            p.visible()
                .iter()
                .all(|&i| p.rows[i].key.path.contains("guardian.composition"))
        );
        type_text(&mut p, " x");
        assert!(p.visible().is_empty());
        assert_eq!(p.selected_path(), None);
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), ConfigKey::None));
    }

    #[test]
    fn up_and_down_stay_inside_the_visible_rows() {
        let mut p = panel();
        type_text(&mut p, "telemetry.log");
        let n = p.visible().len();
        assert!(n > 1);
        for _ in 0..n * 2 + 1 {
            p.handle_key(key(KeyCode::Down));
            assert!(p.selected < n);
        }
        for _ in 0..n * 2 + 1 {
            p.handle_key(key(KeyCode::Up));
            assert!(p.selected < n);
        }
    }

    #[test]
    fn backspace_on_an_empty_filter_does_nothing() {
        let mut p = panel();
        p.handle_key(key(KeyCode::Down));
        let before = p.selected;
        assert!(matches!(
            p.handle_key(key(KeyCode::Backspace)),
            ConfigKey::None
        ));
        assert_eq!(p.selected, before);
        assert!(p.filter.is_empty());
    }

    #[test]
    fn enter_cycles_a_choice_and_saves_its_typed_value() {
        let mut p = panel();
        select(&mut p, "schedule.enabled");
        let ConfigKey::Save { path, value } = p.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a save");
        };
        assert_eq!(path, "schedule.enabled");
        assert_eq!(value.as_bool(), Some(false));
    }

    #[test]
    fn cycling_onto_unset_resets_the_key() {
        let mut p = panel();
        select(&mut p, "tui.screen_reader");
        for expected in [true, false] {
            let ConfigKey::Save { value, .. } = p.handle_key(key(KeyCode::Enter)) else {
                panic!("expected a save");
            };
            assert_eq!(value.as_bool(), Some(expected));
            let pending = value_label(&value);
            p.saved("tui.screen_reader", &pending, None, None);
        }
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            ConfigKey::Reset("tui.screen_reader")
        ));
    }

    #[test]
    fn enter_cycles_from_the_pending_value_when_there_is_one() {
        let pending = BTreeMap::from([("schedule.enabled".to_string(), "false".to_string())]);
        let mut p = panel_with("", &pending);
        select(&mut p, "schedule.enabled");
        assert!(plain(&p.body(120, 20, &Theme::default()))[1].contains("next launch: false"));
        let ConfigKey::Save { value, .. } = p.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a save");
        };
        assert_eq!(value.as_bool(), Some(true));
    }

    #[test]
    fn repeated_enter_cycles_before_the_save_is_reported() {
        let mut p = panel();
        select(&mut p, "schedule.enabled");
        let mut seen = Vec::new();
        for _ in 0..2 {
            let ConfigKey::Save { value, .. } = p.handle_key(key(KeyCode::Enter)) else {
                panic!("expected a save");
            };
            seen.push(value.as_bool());
        }
        assert_eq!(seen, [Some(false), Some(true)]);
    }

    #[test]
    fn enter_moves_on_from_a_saved_value_a_higher_scope_overrides() {
        let mut p = panel();
        select(&mut p, "schedule.enabled");
        p.saved("schedule.enabled", "false", None, Some(Slot::Cli));
        let ConfigKey::Save { value, .. } = p.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a save");
        };
        assert_eq!(value.as_bool(), Some(true));
    }

    #[test]
    fn the_integer_field_takes_digits_only() {
        let mut p = panel();
        select(&mut p, "context.autocompact_window");
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), ConfigKey::None));
        assert_eq!(p.field(), ("value: ", String::new()));
        type_text(&mut p, "1a5x0");
        assert_eq!(p.field().1, "150");
        let ConfigKey::Save { path, value } = p.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a save");
        };
        assert_eq!(path, "context.autocompact_window");
        assert_eq!(value.as_integer(), Some(150));
        assert_eq!(p.field().0, "filter: ");
    }

    #[test]
    fn an_empty_integer_field_resets_and_zero_is_refused() {
        let mut p = panel();
        select(&mut p, "context.autocompact_window");
        p.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            ConfigKey::Reset("context.autocompact_window")
        ));
        p.handle_key(key(KeyCode::Enter));
        type_text(&mut p, "0");
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), ConfigKey::None));
        assert!(p.status().unwrap().contains("above zero"));
        assert!(p.field.is_some());
        p.handle_key(key(KeyCode::Esc));
        assert!(p.field.is_none());
        assert!(matches!(p.handle_key(key(KeyCode::Esc)), ConfigKey::Close));
    }

    #[test]
    fn delete_resets_choice_and_int_rows_only() {
        let mut p = panel();
        for path in [
            "tui.theme",
            "context.autocompact_window",
            "schedule.enabled",
        ] {
            select(&mut p, path);
            assert!(
                matches!(p.handle_key(key(KeyCode::Delete)), ConfigKey::Reset(got) if got == path),
                "{path}"
            );
        }
        for path in ["model.model", "guardian.model", "agents.max_parallel"] {
            select(&mut p, path);
            assert!(
                matches!(p.handle_key(key(KeyCode::Delete)), ConfigKey::None),
                "{path}"
            );
        }
    }

    #[test]
    fn a_picker_row_opens_the_picker() {
        let mut p = panel();
        select(&mut p, "model.model");
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            ConfigKey::OpenPicker(ModelSlot::Main)
        ));
        select(&mut p, "guardian.model");
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            ConfigKey::OpenPicker(ModelSlot::Guardian)
        ));
    }

    #[test]
    fn a_read_only_row_emits_nothing_and_says_where_to_edit() {
        let mut p = panel();
        select(&mut p, "agents.max_parallel");
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), ConfigKey::None));
        assert!(p.status().unwrap().contains("read-only"));
        assert!(plain(&p.footer()).iter().any(|l| l.contains("read-only")));
    }

    #[test]
    fn a_save_that_a_higher_scope_overrides_names_that_scope() {
        let mut p = panel_with("[schedule]\nenabled = true\n", &BTreeMap::new());
        select(&mut p, "schedule.enabled");
        let pending = p.saved("schedule.enabled", "false", None, Some(Slot::Cli));
        assert_eq!(pending.as_deref(), Some("false"));
        let status = p.status().unwrap();
        assert!(status.contains("cli sets this key and wins"), "{status}");
        assert!(status.contains("stays true"), "{status}");

        select(&mut p, "tui.theme");
        p.saved("tui.theme", "light", Some("light"), Some(Slot::Project));
        let status = p.status().unwrap();
        assert!(
            status.starts_with("applied now; project sets this key"),
            "{status}"
        );
    }

    #[test]
    fn a_next_launch_save_shows_pending_and_a_live_save_updates_the_value() {
        let mut p = panel();
        select(&mut p, "schedule.enabled");
        let pending = p.saved("schedule.enabled", "false", None, None);
        assert_eq!(pending.as_deref(), Some("false"));
        assert!(p.status().unwrap().contains("applies from the next launch"));
        assert!(plain(&p.body(120, 20, &Theme::default()))[1].contains("next launch: false"));

        select(&mut p, "tui.theme");
        p.saved("tui.theme", "light", Some("light"), None);
        assert!(plain(&p.body(120, 20, &Theme::default()))[1].contains("light"));
        assert!(p.status().unwrap().starts_with("saved to "));
    }

    #[test]
    fn a_failure_is_reported_as_not_saved() {
        let mut p = panel();
        p.failed("there is no user settings file");
        assert_eq!(
            p.status(),
            Some("not saved: there is no user settings file")
        );
    }

    #[test]
    fn the_linear_body_has_one_plain_line_per_row() {
        let p = panel();
        let lines = plain(&p.body_as(200, 500, &Theme::default(), true));
        assert_eq!(lines.len(), KEYS.len() + 1);
        assert!(
            lines
                .iter()
                .all(|l| !l.contains(['│', '┌', '└', '─', '┐', '┘']))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tui.theme: dark-ansi, from default, editable"))
        );
    }

    #[test]
    fn the_body_shows_value_and_source_columns_and_an_edit_mark() {
        let p = panel_with("[tui]\ntheme = \"light\"\n", &BTreeMap::new());
        let lines = plain(&p.body(120, 500, &Theme::default()));
        let theme = lines.iter().find(|l| l.contains("tui.theme")).unwrap();
        assert!(theme.contains("light") && theme.contains("user") && theme.contains('✎'));
        let max = lines
            .iter()
            .find(|l| l.contains("agents.max_parallel"))
            .unwrap();
        assert!(max.contains("default") && !max.contains('✎'));
    }

    #[test]
    fn the_window_keeps_the_selection_in_view() {
        let mut p = panel();
        for _ in 0..30 {
            p.handle_key(key(KeyCode::Down));
        }
        let lines = plain(&p.body(120, 6, &Theme::default()));
        assert_eq!(lines.len(), 6);
        let shown = &p.rows[p.visible()[p.selected]].key.path;
        assert!(lines.iter().any(|l| l.contains(shown)));
    }
}
