//! What the human has submitted, kept across sessions for Up/Down recall and Ctrl-R search.
//! One owner-only JSONL file shared by every project, each line tagged with the project it was
//! typed in; only this project's lines are recalled.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fontaine_core::owner_only;
use serde_json::{Value, json};

pub const HISTORY_CAP: usize = 5_000;

pub struct History {
    /// `None` once the file could not be read or written: the history then lives in memory only.
    path: Option<PathBuf>,
    project: String,
    entries: Vec<String>,
    /// The entry shown by Up/Down, when navigating.
    cursor: Option<usize>,
    /// What was in the input when navigation began.
    draft: String,
}

fn parse(line: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(line).ok()?;
    Some((
        value.get("project")?.as_str()?.to_string(),
        value.get("text")?.as_str()?.to_string(),
    ))
}

impl History {
    pub fn in_memory(project: &str) -> History {
        History {
            path: None,
            project: project.to_string(),
            entries: Vec::new(),
            cursor: None,
            draft: String::new(),
        }
    }

    /// Reads this project's entries, oldest first. The second value is an error to show once,
    /// when the file could not be used and the history is in memory only.
    pub fn load(path: &Path, project: &str) -> (History, Option<String>) {
        let mut history = History::in_memory(project);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return (history, Some(unusable(path, &e))),
        };
        history.path = Some(path.to_path_buf());
        let lines: Vec<&str> = text.lines().collect();
        let kept = if lines.len() > HISTORY_CAP {
            &lines[lines.len() - HISTORY_CAP..]
        } else {
            &lines[..]
        };
        history.entries = kept
            .iter()
            .filter_map(|line| parse(line))
            .filter(|(entry_project, _)| entry_project == project)
            .map(|(_, text)| text)
            .collect();
        // Slack before rewriting keeps a file at the cap from being rewritten on every start.
        let error = if lines.len() > HISTORY_CAP + HISTORY_CAP / 5 {
            rewrite(path, kept).err().map(|e| unusable(path, &e))
        } else {
            None
        };
        if error.is_some() {
            history.path = None;
        }
        (history, error)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, at: usize) -> Option<&str> {
        self.entries.get(at).map(String::as_str)
    }

    /// Records a submitted input. A repeat of the newest entry is stored once. Returns an error
    /// to show once when the file could not be written.
    pub fn push(&mut self, text: &str) -> Option<String> {
        self.cursor = None;
        if text.trim().is_empty() || self.entries.last().is_some_and(|last| last == text) {
            return None;
        }
        self.entries.push(text.to_string());
        let path = self.path.clone()?;
        match self.append(&path, text) {
            Ok(()) => None,
            Err(e) => {
                self.path = None;
                Some(unusable(&path, &e))
            }
        }
    }

    fn append(&self, path: &Path, text: &str) -> io::Result<()> {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut line = json!({ "ts_ms": ts_ms, "project": self.project, "text": text }).to_string();
        line.push('\n');
        let mut file = owner_only::open(path, true)?;
        file.write_all(line.as_bytes())?;
        file.flush()
    }

    /// Stops navigating, as when the human edits the recalled text.
    pub fn reset(&mut self) {
        self.cursor = None;
    }

    pub fn navigating(&self) -> bool {
        self.cursor.is_some()
    }

    /// The next older entry; `draft` is what the input holds when navigation begins.
    pub fn older(&mut self, draft: &str) -> Option<String> {
        let at = match self.cursor {
            None if self.entries.is_empty() => return None,
            None => {
                self.draft = draft.to_string();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(at) => at - 1,
        };
        self.cursor = Some(at);
        Some(self.entries[at].clone())
    }

    /// The next newer entry, or the draft once past the newest.
    pub fn newer(&mut self) -> Option<String> {
        let at = self.cursor?;
        if at + 1 < self.entries.len() {
            self.cursor = Some(at + 1);
            Some(self.entries[at + 1].clone())
        } else {
            self.cursor = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    /// The newest entry older than `before` that contains `query`, ignoring case.
    pub fn search(&self, query: &str, before: usize) -> Option<(usize, &str)> {
        let query = query.to_lowercase();
        self.entries[..before.min(self.entries.len())]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, entry)| entry.to_lowercase().contains(&query))
            .map(|(at, entry)| (at, entry.as_str()))
    }
}

fn unusable(path: &Path, error: &io::Error) -> String {
    format!(
        "prompt history in {} cannot be used ({}); it is kept for this session only",
        path.display(),
        fontaine_tools::error_chain::chain(error)
    )
}

/// Replaces `path` with `lines` through an owner-only temp file in the same directory, so a
/// crash cannot leave a truncated history.
fn rewrite(path: &Path, lines: &[&str]) -> io::Result<()> {
    let tmp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    let mut body = lines.join("\n");
    body.push('\n');
    owner_only::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        (dir, path)
    }

    #[test]
    fn entries_round_trip_and_other_projects_are_skipped() {
        let (_dir, path) = file();
        let (mut mine, error) = History::load(&path, "/work/a");
        assert!(error.is_none());
        assert!(mine.push("one").is_none());
        assert!(mine.push("two").is_none());
        let (mut other, _) = History::load(&path, "/work/b");
        assert!(other.is_empty());
        other.push("elsewhere");
        let (again, _) = History::load(&path, "/work/a");
        assert_eq!(again.entries, vec!["one", "two"]);
        let line = std::fs::read_to_string(&path).unwrap();
        let first: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(first["project"], "/work/a");
        assert_eq!(first["text"], "one");
        assert!(first["ts_ms"].is_u64());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let (_dir, path) = file();
        std::fs::write(
            &path,
            "not json\n{\"project\":\"p\",\"text\":\"kept\"}\n{\"project\":\"p\"}\n{\"project\":\"p\",\"text\":\"torn",
        )
        .unwrap();
        let (history, error) = History::load(&path, "p");
        assert!(error.is_none());
        assert_eq!(history.entries, vec!["kept"]);
    }

    #[test]
    fn a_long_file_is_rewritten_with_the_newest_entries() {
        let (_dir, path) = file();
        let body: String = (0..HISTORY_CAP + HISTORY_CAP / 5 + 3)
            .map(|i| format!("{{\"ts_ms\":0,\"project\":\"p\",\"text\":\"m{i}\"}}\n"))
            .collect();
        std::fs::write(&path, body).unwrap();
        let (history, error) = History::load(&path, "p");
        assert!(error.is_none());
        assert_eq!(history.len(), HISTORY_CAP);
        let dropped = HISTORY_CAP / 5 + 3;
        assert_eq!(history.entries[0], format!("m{dropped}"));
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert_eq!(rewritten.lines().count(), HISTORY_CAP);
        assert!(rewritten.starts_with(&format!(
            "{{\"ts_ms\":0,\"project\":\"p\",\"text\":\"m{dropped}\"}}"
        )));
        assert!(
            !path
                .with_extension(format!("jsonl.{}.tmp", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn up_and_down_walk_the_entries_and_restore_the_draft() {
        let mut history = History::in_memory("p");
        history.push("one");
        history.push("two");
        assert_eq!(history.older("draft").as_deref(), Some("two"));
        assert_eq!(history.older("ignored").as_deref(), Some("one"));
        assert_eq!(history.older("ignored"), None);
        assert_eq!(history.newer().as_deref(), Some("two"));
        assert_eq!(history.newer().as_deref(), Some("draft"));
        assert_eq!(history.newer(), None);
    }

    #[test]
    fn consecutive_duplicates_are_stored_once() {
        let (_dir, path) = file();
        let (mut history, _) = History::load(&path, "p");
        history.push("same");
        history.push("same");
        history.push("other");
        history.push("same");
        assert_eq!(history.entries, vec!["same", "other", "same"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
        history.push("  ");
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn search_finds_the_newest_older_match_ignoring_case() {
        let mut history = History::in_memory("p");
        for entry in ["Fix the Parser", "run tests", "parser again", "other"] {
            history.push(entry);
        }
        assert_eq!(history.search("PARSER", 4), Some((2, "parser again")));
        assert_eq!(history.search("parser", 2), Some((0, "Fix the Parser")));
        assert_eq!(history.search("parser", 0), None);
        assert_eq!(history.search("missing", 4), None);
    }

    #[test]
    fn an_unwritable_path_keeps_the_history_in_memory_and_reports_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("history.jsonl");
        let (mut history, error) = History::load(&path, "p");
        assert!(error.is_none());
        let first = history.push("one");
        assert!(first.unwrap().contains("kept for this session only"));
        assert!(history.push("two").is_none());
        assert_eq!(history.older("").as_deref(), Some("two"));
        assert!(!path.exists());
    }
}
