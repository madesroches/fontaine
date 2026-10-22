//! The session ledger: the append-only record of what the human said and what the harness did,
//! kept apart from the model context that compaction rewrites. Only the harness writes it,
//! never the model, and nothing here removes or edits a record.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::owner_only;

pub const LEDGER_DIR: &str = "ledger";
pub const LEDGER_FILE: &str = "ledger.jsonl";

/// Paths the pre-session file index holds before it records that it was cut short.
pub const FILE_INDEX_CAP: usize = 50_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    SessionStart {
        session_id: String,
        permission_mode: String,
        workspace_roots: Vec<String>,
        workspace_trust: String,
        git_remotes: Vec<String>,
    },
    FileIndex {
        paths: Vec<String>,
        truncated: bool,
    },
    /// A human message, verbatim.
    UserMessage {
        text: String,
    },
    ModeChange {
        from: String,
        to: String,
    },
    Approval {
        tool: String,
        agent: String,
        scope: String,
    },
    Grant {
        rule: String,
    },
    Denial {
        tool: String,
        agent: String,
        reason: String,
        /// The audit rule label the denial carried, such as `Git Destructive` or `deny_rule`.
        #[serde(default)]
        rule: Option<String>,
        /// A compact, credential-redacted summary of the call.
        #[serde(default)]
        summary: String,
    },
    Action {
        tool: String,
        agent: String,
        /// The absolute paths the call touches, as the scope validator resolved them.
        #[serde(default)]
        paths: Vec<String>,
        /// The subset of `paths` that was absent on disk when the call was allowed, so a call
        /// that touches an existing file never marks it as made by this session.
        #[serde(default)]
        created: Vec<String>,
        /// A compact, credential-redacted summary of the arguments.
        #[serde(default)]
        summary: String,
    },
    /// A directory added to the workspace roots after the session started.
    AddDir {
        root: String,
    },
    /// This session continues another one: `resume` and `continue` reopen it, `fork` copies it
    /// under a new id. Session grants are never carried over.
    Resumed {
        source: String,
        how: String,
    },
    /// A subagent started or ended, or two agents exchanged a message. What an agent was told
    /// to do is not recorded here as anything the human said.
    Agent {
        event: String,
        agent: String,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        detail: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub record: Record,
}

struct Tail {
    file: File,
    next_seq: u64,
}

/// A path as the ledger compares it: the same file spelled with or without a Windows verbatim
/// prefix, or in another letter case on Windows, is one path.
pub fn path_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// What the ledger's records add up to, kept beside the file so the guardian's request is built
/// from the ledger without re-reading it for every call.
#[derive(Debug, Default)]
pub struct LedgerView {
    pub workspace_roots: Vec<String>,
    pub workspace_trust: String,
    pub git_remotes: Vec<String>,
    index: HashSet<String>,
    pub index_truncated: bool,
    /// Every human message, verbatim, oldest first.
    pub user_messages: Vec<String>,
    pub actions: Vec<ActionEntry>,
    pub denials: Vec<DenialEntry>,
    touched: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionEntry {
    pub tool: String,
    pub agent: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenialEntry {
    pub tool: String,
    pub rule: String,
    pub summary: String,
}

/// Whether a path was there before the session began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existence {
    /// In the file index taken at session start.
    PreExisting,
    /// Not indexed, and an earlier action of this session created it.
    CreatedThisSession,
    /// Not indexed and untouched so far.
    Unindexed,
}

impl LedgerView {
    pub fn existence(&self, path: &str) -> Existence {
        let key = path_key(path);
        if self.index.contains(&key) {
            Existence::PreExisting
        } else if self.touched.contains(&key) {
            Existence::CreatedThisSession
        } else {
            Existence::Unindexed
        }
    }

    fn apply(&mut self, record: &Record) {
        match record {
            Record::SessionStart {
                workspace_roots,
                workspace_trust,
                git_remotes,
                ..
            } => {
                self.workspace_roots = workspace_roots.clone();
                self.workspace_trust = workspace_trust.clone();
                self.git_remotes = git_remotes.clone();
            }
            Record::FileIndex { paths, truncated } => {
                self.index = paths.iter().map(|p| path_key(p)).collect();
                self.index_truncated = *truncated;
            }
            Record::UserMessage { text } => self.user_messages.push(text.clone()),
            Record::Action {
                tool,
                agent,
                created,
                summary,
                ..
            } => {
                self.touched.extend(created.iter().map(|p| path_key(p)));
                self.actions.push(ActionEntry {
                    tool: tool.clone(),
                    agent: agent.clone(),
                    summary: summary.clone(),
                });
            }
            Record::Denial {
                tool,
                rule,
                summary,
                reason,
                ..
            } => self.denials.push(DenialEntry {
                tool: tool.clone(),
                rule: rule.clone().unwrap_or_else(|| reason.clone()),
                summary: summary.clone(),
            }),
            Record::AddDir { root } => self.workspace_roots.push(root.clone()),
            Record::ModeChange { .. }
            | Record::Approval { .. }
            | Record::Grant { .. }
            | Record::Resumed { .. }
            | Record::Agent { .. } => {}
        }
    }
}

pub struct Ledger {
    path: PathBuf,
    tail: Mutex<Tail>,
    view: Mutex<LedgerView>,
}

impl Ledger {
    /// Starts the ledger of the session whose directory is `session_dir`.
    pub fn create(session_dir: &Path) -> io::Result<Ledger> {
        let dir = session_dir.join(LEDGER_DIR);
        owner_only::create_dir(&dir)?;
        let path = dir.join(LEDGER_FILE);
        let file = owner_only::open(&path, true)?;
        Ok(Ledger {
            path,
            tail: Mutex::new(Tail { file, next_seq: 0 }),
            view: Mutex::new(LedgerView::default()),
        })
    }

    /// Reopens the ledger of an earlier session: its records rebuild the view, and new ones
    /// continue the sequence.
    pub fn open(session_dir: &Path) -> io::Result<Ledger> {
        let path = session_dir.join(LEDGER_DIR).join(LEDGER_FILE);
        let text = std::fs::read_to_string(&path)?;
        let mut view = LedgerView::default();
        let mut next_seq = 0;
        let mut good_end = 0;
        let mut offset = 0;
        let mut lines = text.split_inclusive('\n').peekable();
        while let Some(raw) = lines.next() {
            offset += raw.len();
            if raw.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(raw) {
                Ok(entry) => {
                    view.apply(&entry.record);
                    next_seq = entry.seq + 1;
                    good_end = offset;
                }
                // A write cut short (full disk, power loss) can only damage the end, and leaves no
                // newline; a complete line that fails to parse is something else (say, a newer
                // build's record) and is not ours to cut.
                Err(e) if lines.peek().is_none() && !raw.ends_with('\n') => {
                    tracing::warn!(error = %e, "dropping the torn last line of the session ledger");
                    let file = std::fs::OpenOptions::new().write(true).open(&path)?;
                    file.set_len(good_end as u64)?;
                }
                Err(e) => return Err(io::Error::other(e)),
            }
        }
        let mut file = owner_only::open(&path, true)?;
        // A complete last record can still lack its newline; the next append would join it.
        if good_end == text.len() && !text.is_empty() && !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        Ok(Ledger {
            path,
            tail: Mutex::new(Tail { file, next_seq }),
            view: Mutex::new(view),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: Record) -> io::Result<()> {
        let mut tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        let entry = Entry {
            seq: tail.next_seq,
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            record,
        };
        let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        line.push(b'\n');
        tail.file.write_all(&line)?;
        tail.file.flush()?;
        tail.next_seq += 1;
        self.view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(&entry.record);
        Ok(())
    }

    /// What the records so far add up to.
    pub fn view(&self) -> std::sync::MutexGuard<'_, LedgerView> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every record written so far, in order.
    pub fn read(&self) -> io::Result<Vec<Entry>> {
        std::fs::read_to_string(&self.path)?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(io::Error::other))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileIndex {
    pub paths: Vec<String>,
    pub truncated: bool,
}

impl FileIndex {
    /// The files under `roots` that git would not ignore, up to `cap` of them.
    pub fn build(roots: &[PathBuf], cap: usize) -> FileIndex {
        let mut index = FileIndex::default();
        for root in roots {
            let walk = WalkBuilder::new(root)
                .hidden(false)
                .git_global(false)
                .require_git(false)
                .filter_entry(|entry| entry.file_name() != ".git")
                .build();
            for entry in walk.flatten() {
                if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                if index.paths.len() == cap {
                    index.truncated = true;
                    return index;
                }
                index
                    .paths
                    .push(entry.path().to_string_lossy().into_owned());
            }
        }
        index
    }
}

/// The fetch URLs of the repository around `root`, read from its own config without following
/// `include` or `includeIf`. None when there is no repository or gix cannot read it; Fontaine
/// never spawns `git`.
pub fn git_remotes(root: &Path) -> Vec<String> {
    let options = gix::open::Options::isolated().strict_config(true);
    let Ok(repo) = gix::discover_opts(root, gix::discover::upwards::Options::default(), options)
    else {
        return Vec::new();
    };
    let mut urls: Vec<String> = repo
        .remote_names()
        .iter()
        .filter_map(|name| repo.try_find_remote(name.as_bstr())?.ok())
        .filter_map(|remote| {
            let url = remote.url(gix::remote::Direction::Fetch)?;
            Some(without_credentials(&url.to_bstring().to_string()))
        })
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// `https://user:token@host/x` without its `user:token@`; an scp-style `git@host:x` keeps its
/// user, which names no secret.
fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, None), |(a, p)| (a, Some(p)));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    match path {
        Some(path) => format!("{scheme}://{host}/{path}"),
        None => format!("{scheme}://{host}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_stripped_from_remote_urls() {
        assert_eq!(
            without_credentials("https://me:ghp_secret@github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
        assert_eq!(
            without_credentials("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
        assert_eq!(without_credentials("ssh://git@host"), "ssh://host");
    }

    fn ledger_with_two_messages(dir: &Path) -> PathBuf {
        let ledger = Ledger::create(dir).unwrap();
        for text in ["one", "two"] {
            ledger
                .append(Record::UserMessage { text: text.into() })
                .unwrap();
        }
        ledger.path().to_path_buf()
    }

    #[test]
    fn a_torn_last_line_is_cut_off_and_appends_continue_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"seq\":2,\"ts_ms\":1,\"rec");
        std::fs::write(&path, &text).unwrap();

        let ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append(Record::UserMessage {
                text: "three".into(),
            })
            .unwrap();
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        let lines = std::fs::read_to_string(reopened.path()).unwrap();
        assert_eq!(lines.lines().count(), 3);
        assert!(lines.lines().last().unwrap().contains("three"));
    }

    #[test]
    fn a_complete_last_record_without_its_newline_does_not_join_the_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.trim_end_matches('\n')).unwrap();

        let ledger = Ledger::open(dir.path()).unwrap();
        ledger
            .append(Record::UserMessage {
                text: "three".into(),
            })
            .unwrap();
        drop(ledger);
        let reopened = Ledger::open(dir.path()).unwrap();
        let lines = std::fs::read_to_string(reopened.path()).unwrap();
        assert_eq!(lines.lines().count(), 3);
    }

    #[test]
    fn a_bad_line_before_the_last_still_fails_the_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let text = std::fs::read_to_string(&path).unwrap();
        let (first, rest) = text.split_once('\n').unwrap();
        std::fs::write(&path, format!("{first}\nnot json\n{rest}")).unwrap();
        assert!(Ledger::open(dir.path()).is_err());
    }

    #[test]
    fn a_complete_unparseable_last_line_fails_the_open_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = ledger_with_two_messages(dir.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"seq\":2,\"ts_ms\":1,\"record\":{\"kind\":\"from_the_future\"}}\n");
        std::fs::write(&path, &text).unwrap();
        assert!(Ledger::open(dir.path()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}
