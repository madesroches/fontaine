//! The models the human switched to, kept across sessions for the picker's Recent group. One
//! owner-only JSON file in the data directory, shared by every project.

use std::path::{Path, PathBuf};

use fontaine_core::models::ModelRef;
use fontaine_core::owner_only;
use serde_json::{Value, json};

use crate::model_picker::PickerPurpose;

pub const RECENT_CAP: usize = 5;

pub struct RecentModels {
    /// `None` once the file could not be read or written: the list then lives in memory only.
    path: Option<PathBuf>,
    main: Vec<String>,
    guardian: Vec<String>,
}

fn unusable(path: &Path, what: &str) -> String {
    format!(
        "recent models in {} cannot be used ({what}); they are kept for this session only",
        path.display()
    )
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.as_str().map(str::to_string))
        .take(RECENT_CAP)
        .collect()
}

impl RecentModels {
    pub fn in_memory() -> RecentModels {
        RecentModels {
            path: None,
            main: Vec::new(),
            guardian: Vec::new(),
        }
    }

    /// The second value is an error to show once, when the file could not be used.
    pub fn load(path: &Path) -> (RecentModels, Option<String>) {
        let mut recent = RecentModels::in_memory();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                recent.path = Some(path.to_path_buf());
                return (recent, None);
            }
            Err(e) => return (recent, Some(unusable(path, &e.to_string()))),
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(value) if value.is_object() => {
                recent.main = strings(&value, "main");
                recent.guardian = strings(&value, "guardian");
                recent.path = Some(path.to_path_buf());
                (recent, None)
            }
            Ok(_) => (
                recent,
                Some(unusable(path, "it does not hold a JSON object")),
            ),
            Err(e) => (recent, Some(unusable(path, &e.to_string()))),
        }
    }

    fn list_mut(&mut self, purpose: PickerPurpose) -> &mut Vec<String> {
        match purpose {
            PickerPurpose::Main => &mut self.main,
            PickerPurpose::Guardian => &mut self.guardian,
        }
    }

    /// The models for `purpose`, most recent first.
    pub fn list(&self, purpose: PickerPurpose) -> Vec<ModelRef> {
        let list = match purpose {
            PickerPurpose::Main => &self.main,
            PickerPurpose::Guardian => &self.guardian,
        };
        list.iter()
            .filter_map(|text| ModelRef::parse(text))
            .collect()
    }

    /// Moves `model` to the front of the list for `purpose`. Returns an error to show once when
    /// the file could not be written.
    pub fn record(&mut self, purpose: PickerPurpose, model: &ModelRef) -> Option<String> {
        let text = model.to_string();
        let list = self.list_mut(purpose);
        list.retain(|entry| *entry != text);
        list.insert(0, text);
        list.truncate(RECENT_CAP);
        let path = self.path.clone()?;
        let body = json!({ "main": self.main, "guardian": self.guardian }).to_string();
        match owner_only::write_atomic(&path, body.as_bytes()) {
            Ok(()) => None,
            Err(e) => {
                self.path = None;
                Some(unusable(&path, &e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(text: &str) -> ModelRef {
        ModelRef::parse(text).unwrap()
    }

    fn file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent_models.json");
        (dir, path)
    }

    #[test]
    fn a_record_moves_to_the_front_deduplicates_and_caps() {
        let mut recent = RecentModels::in_memory();
        for name in ["a/1", "a/2", "a/3", "a/4", "a/5", "a/6"] {
            recent.record(PickerPurpose::Main, &model(name));
        }
        recent.record(PickerPurpose::Main, &model("a/4"));
        let names: Vec<String> = recent
            .list(PickerPurpose::Main)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(names, ["a/4", "a/6", "a/5", "a/3", "a/2"]);
    }

    #[test]
    fn main_and_guardian_lists_are_separate() {
        let mut recent = RecentModels::in_memory();
        recent.record(PickerPurpose::Main, &model("a/1"));
        recent.record(PickerPurpose::Guardian, &model("b/2"));
        assert_eq!(recent.list(PickerPurpose::Main), [model("a/1")]);
        assert_eq!(recent.list(PickerPurpose::Guardian), [model("b/2")]);
    }

    #[test]
    fn the_file_round_trips() {
        let (_dir, path) = file();
        let (mut recent, error) = RecentModels::load(&path);
        assert!(error.is_none());
        assert!(recent.record(PickerPurpose::Main, &model("a/1")).is_none());
        assert!(recent.record(PickerPurpose::Main, &model("a/2")).is_none());
        assert!(
            recent
                .record(PickerPurpose::Guardian, &model("b/j"))
                .is_none()
        );
        let (again, error) = RecentModels::load(&path);
        assert!(error.is_none());
        assert_eq!(
            again.list(PickerPurpose::Main),
            [model("a/2"), model("a/1")]
        );
        assert_eq!(again.list(PickerPurpose::Guardian), [model("b/j")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
    }

    #[test]
    fn a_missing_file_is_empty() {
        let (_dir, path) = file();
        let (recent, error) = RecentModels::load(&path);
        assert!(error.is_none());
        assert!(recent.list(PickerPurpose::Main).is_empty());
    }

    #[test]
    fn garbage_degrades_to_memory_with_one_error() {
        let (_dir, path) = file();
        std::fs::write(&path, "not json").unwrap();
        let (mut recent, error) = RecentModels::load(&path);
        assert!(error.unwrap().contains("kept for this session only"));
        assert!(recent.record(PickerPurpose::Main, &model("a/1")).is_none());
        assert_eq!(recent.list(PickerPurpose::Main), [model("a/1")]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    #[test]
    fn a_failed_write_reports_once_and_keeps_the_state() {
        let (dir, _) = file();
        let path = dir.path().join("missing").join("recent_models.json");
        let (mut recent, error) = RecentModels::load(&path);
        assert!(error.is_none());
        let first = recent.record(PickerPurpose::Main, &model("a/1"));
        assert!(first.unwrap().contains("recent models"));
        assert!(recent.record(PickerPurpose::Main, &model("a/2")).is_none());
        assert_eq!(
            recent.list(PickerPurpose::Main),
            [model("a/2"), model("a/1")]
        );
    }
}
