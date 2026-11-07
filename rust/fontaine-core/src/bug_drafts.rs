//! Bug-report drafts the model queued with `draft_bug_report`: redacted JSON files in
//! `<data_dir>/bug-drafts`, which `/bug` lists and loads as a description.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use fontaine_tools::bug_draft::{DraftArgs, DraftSink, MAX_DRAFTS};
use serde::{Deserialize, Serialize};

use crate::owner_only;
use crate::redact::Redactor;

const DIR: &str = "bug-drafts";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub area: String,
    pub details: String,
}

impl Draft {
    /// The text `/bug` starts from.
    pub fn description(&self) -> String {
        format!("{}\n\nArea: {}\n\n{}", self.title, self.area, self.details)
    }
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR)
}

/// Every readable draft, oldest id first. A file that cannot be read or parsed is skipped.
pub fn list(data_dir: &Path) -> Vec<Draft> {
    let Ok(entries) = std::fs::read_dir(dir(data_dir)) else {
        return Vec::new();
    };
    let mut drafts: Vec<Draft> = entries
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect();
    drafts.sort_by(|a, b| a.id.cmp(&b.id));
    drafts
}

/// Deletes the draft with `id`; a draft already gone is fine. The id must be one [`list`] returned.
pub fn remove(data_dir: &Path, id: &str) -> io::Result<()> {
    match std::fs::remove_file(dir(data_dir).join(format!("{id}.json"))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The number of readable drafts, re-read only when the directory's contents change.
#[derive(Default)]
pub struct Pending {
    stamp: Option<(Option<std::time::SystemTime>, usize)>,
    count: usize,
}

impl Pending {
    pub fn count(&mut self, data_dir: &Path) -> usize {
        let directory = dir(data_dir);
        let entries = std::fs::read_dir(&directory).map_or(0, Iterator::count);
        let modified = std::fs::metadata(&directory)
            .and_then(|m| m.modified())
            .ok();
        if self.stamp != Some((modified, entries)) {
            self.count = list(data_dir).len();
            self.stamp = Some((modified, entries));
        }
        self.count
    }
}

/// Saves drafts for one session, at most [`MAX_DRAFTS`].
pub struct FileDrafts {
    data_dir: PathBuf,
    session: String,
    redactor: Redactor,
}

impl FileDrafts {
    pub fn new(data_dir: PathBuf, session: &str, redactor: Redactor) -> FileDrafts {
        FileDrafts {
            data_dir,
            session: session.get(..8).unwrap_or(session).to_string(),
            redactor,
        }
    }

    /// Drafts this session has queued.
    pub fn count(&self) -> usize {
        list(&self.data_dir)
            .iter()
            .filter(|d| d.id.starts_with(&format!("{}-", self.session)))
            .count()
    }
}

impl DraftSink for FileDrafts {
    fn save(&self, draft: &DraftArgs) -> Result<String, String> {
        if self.count() >= MAX_DRAFTS {
            return Err(format!(
                "this session already queued {MAX_DRAFTS} drafts; the user can file them with /bug"
            ));
        }
        let directory = dir(&self.data_dir);
        std::fs::create_dir_all(&directory)
            .map_err(|e| format!("cannot create the drafts directory: {e}"))?;
        for n in 1u32.. {
            let id = format!("{}-{n}", self.session);
            let path = directory.join(format!("{id}.json"));
            let saved = Draft {
                id: id.clone(),
                title: self.redactor.redact(&draft.title),
                area: self.redactor.redact(&draft.area),
                details: self.redactor.redact(&draft.details),
            };
            let text = serde_json::to_string_pretty(&saved).expect("a draft serializes");
            match owner_only::create_new(&path) {
                Ok(mut file) => {
                    let written = file.write_all(text.as_bytes()).and_then(|()| file.flush());
                    if let Err(e) = written {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(format!("cannot write the draft: {e}"));
                    }
                    return Ok(id);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("cannot write the draft: {e}")),
            }
        }
        unreachable!("the loop only returns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Secret;

    fn args(details: &str) -> DraftArgs {
        DraftArgs {
            title: "crash".into(),
            area: "tui".into(),
            details: details.into(),
        }
    }

    #[test]
    fn drafts_are_redacted_listed_and_capped() {
        let data = tempfile::tempdir().unwrap();
        let sink = FileDrafts::new(
            data.path().into(),
            "session-abcdef",
            Redactor::new([Secret::new("super-secret-value")]),
        );
        let id = sink.save(&args("leaked super-secret-value")).unwrap();
        assert_eq!(id, "session--1");
        sink.save(&args("two")).unwrap();
        sink.save(&args("three")).unwrap();
        assert!(sink.save(&args("four")).is_err());
        let drafts = list(data.path());
        assert_eq!(drafts.len(), 3);
        assert_eq!(drafts[0].details, "leaked [redacted]");
        assert!(drafts[0].description().starts_with("crash\n\nArea: tui"));
        let raw = std::fs::read_to_string(dir(data.path()).join("session--1.json")).unwrap();
        assert!(!raw.contains("super-secret-value"));
    }

    #[test]
    fn another_session_has_its_own_cap() {
        let data = tempfile::tempdir().unwrap();
        for session in ["aaaaaaaa", "bbbbbbbb"] {
            let sink = FileDrafts::new(data.path().into(), session, Redactor::default());
            for _ in 0..MAX_DRAFTS {
                sink.save(&args("d")).unwrap();
            }
        }
        assert_eq!(list(data.path()).len(), 2 * MAX_DRAFTS);
    }

    #[test]
    fn removed_drafts_leave_the_pending_count() {
        let data = tempfile::tempdir().unwrap();
        let mut pending = Pending::default();
        assert_eq!(pending.count(data.path()), 0);
        let sink = FileDrafts::new(data.path().into(), "aaaaaaaa", Redactor::default());
        let first = sink.save(&args("one")).unwrap();
        sink.save(&args("two")).unwrap();
        assert_eq!(pending.count(data.path()), 2);
        remove(data.path(), &first).unwrap();
        remove(data.path(), &first).unwrap();
        assert_eq!(pending.count(data.path()), 1);
        assert_eq!(list(data.path()).len(), 1);
    }

    #[test]
    fn an_absent_directory_lists_nothing() {
        assert!(list(Path::new("/nonexistent/fontaine-drafts")).is_empty());
    }
}
