//! Checkpoints: the pre-image of every file `write` and `edit` change, keyed by the turn the human
//! started, so `/rewind` can put the files back as they were when a prompt was submitted. The
//! manifest is append-only; a rewind adds a marker instead of removing entries. Only the two
//! editing tools are tracked: a change made through the shell, by the user or by an MCP tool is
//! not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fontaine_tools::checkpoint::{Capture, PreImages};
use fontaine_tools::lock::lock;
use fontaine_tools::scope::Scope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Checkpoints;
use crate::ledger::path_key;
use crate::owner_only;
use crate::permissions::Approver;

pub const CHECKPOINT_DIR: &str = "checkpoints";
const MANIFEST: &str = "manifest.jsonl";
const BLOBS: &str = "blobs";

/// SHA-256 of a file's bytes, in lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    fn of(bytes: &[u8]) -> ContentHash {
        ContentHash(hex(&Sha256::digest(bytes)))
    }

    /// The manifest is read back from disk, so a hash is checked before it names a file.
    fn is_well_formed(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    TooLarge,
    NotRegular,
    Unreadable,
    /// The blob could not be written to the session's store.
    StoreWrite,
    Budget,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SkipReason::TooLarge => "larger than [checkpoints] max_file_bytes",
            SkipReason::NotRegular => "not a regular file",
            SkipReason::Unreadable => "could not be read",
            SkipReason::StoreWrite => "could not be copied into the checkpoint store",
            SkipReason::Budget => "the session's [checkpoints] max_session_bytes is used up",
        })
    }
}

/// What a file was like before the first edit of a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Before {
    Absent,
    Blob {
        hash: ContentHash,
        unix_mode: Option<u32>,
    },
    Skipped {
        why: SkipReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// `/clear` happened: turn ordinals restart and earlier entries are out of reach.
    Epoch {},
    Turn {
        turn: u64,
        /// One past the last stamp of the conversation log when the prompt was submitted.
        conv_n: u64,
        prompt: String,
        /// How many human messages the ledger's intent view held before this prompt.
        user_messages_before: usize,
    },
    /// The conversation (and, with `code`, the files) went back to the start of `to_turn`: the
    /// turns from there on, and with `code` their pre-images, are no longer offered.
    Rewind { to_turn: u64, code: bool },
    PreImage {
        turn: u64,
        agent: String,
        path: String,
        before: Before,
    },
    /// What a tool left at `path` once its call ended; `None` when the file is absent.
    After {
        turn: u64,
        path: String,
        hash: Option<ContentHash>,
    },
}

/// What a rewind puts back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindWhat {
    Both,
    Conversation,
    Code,
}

impl RewindWhat {
    pub fn includes_conversation(self) -> bool {
        self != RewindWhat::Code
    }

    pub fn includes_code(self) -> bool {
        self != RewindWhat::Conversation
    }
}

/// One prompt the picker offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnSummary {
    pub turn: u64,
    pub prompt: String,
    /// Files this turn and the later ones changed.
    pub files: usize,
}

/// What a rewind to a turn would do to one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub step: Step,
    /// The file is not what the last tool call left, so restoring it discards a later edit.
    pub changed_since: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Restore {
        hash: ContentHash,
        unix_mode: Option<u32>,
    },
    /// The turn created the file.
    Delete,
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Skipped(SkipReason),
    OutOfScope,
    Symlink,
    Memory,
    /// The saved bytes are missing or do not match their hash.
    BlobLost,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Skipped(why) => write!(f, "it was not saved: {why}"),
            Refusal::OutOfScope => f.write_str("it is outside the workspace"),
            Refusal::Symlink => f.write_str("it is, or sits under, a symbolic link"),
            Refusal::Memory => f.write_str("it is in the memory directory"),
            Refusal::BlobLost => f.write_str("the saved copy is missing or damaged"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewindPlan {
    pub turn: u64,
    pub prompt: String,
    pub conv_n: u64,
    pub user_messages_before: usize,
    pub files: Vec<PlannedFile>,
}

impl RewindPlan {
    pub fn to_restore(&self) -> impl Iterator<Item = &PlannedFile> {
        self.files
            .iter()
            .filter(|f| matches!(f.step, Step::Restore { .. }))
    }

    pub fn to_delete(&self) -> impl Iterator<Item = &PlannedFile> {
        self.files.iter().filter(|f| f.step == Step::Delete)
    }

    pub fn changed_since(&self) -> impl Iterator<Item = &PlannedFile> {
        self.files
            .iter()
            .filter(|f| f.changed_since && !matches!(f.step, Step::Refused(_)))
    }

    pub fn not_restorable(&self) -> impl Iterator<Item = (&Path, Refusal)> {
        self.files.iter().filter_map(|f| match f.step {
            Step::Refused(why) => Some((f.path.as_path(), why)),
            _ => None,
        })
    }
}

/// What a code restore did.
#[derive(Debug, Default)]
pub struct CodeReport {
    pub restored: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    /// Left alone because the user's later edit was not confirmed for overwriting.
    pub kept_changed: Vec<PathBuf>,
    pub not_restorable: Vec<(PathBuf, Refusal)>,
    pub failed: Vec<(PathBuf, io::Error)>,
}

/// Fixed once the session has its config, approver and workspace.
#[derive(Default)]
struct Settings {
    limits: Option<Checkpoints>,
    memory_root: Option<PathBuf>,
    approver: Option<Arc<dyn Approver>>,
}

struct State {
    manifest: File,
    entries: Vec<Entry>,
    settings: Settings,
    /// Bytes of blobs on disk, which the session budget counts.
    blob_bytes: u64,
    warned: HashSet<SkipReason>,
    io_warned: bool,
    /// `(turn, path)` pairs already holding a pre-image, in every epoch turn ever written.
    captured: HashSet<(u64, String)>,
    next_turn: u64,
}

pub struct CheckpointStore {
    dir: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct Folded {
    turns: Vec<(u64, String)>,
    pre: Vec<(u64, String, Before)>,
    after: Vec<(u64, String, Option<ContentHash>)>,
}

/// The entries a picker, a count or a restore may use: those since the last `/clear`, less what
/// a rewind marker hid.
fn fold(entries: &[Entry]) -> Folded {
    let mut out = Folded::default();
    for entry in entries {
        match entry {
            Entry::Epoch {} => out = Folded::default(),
            Entry::Turn { turn, prompt, .. } => out.turns.push((*turn, prompt.clone())),
            Entry::Rewind { to_turn, code } => {
                out.turns.retain(|(turn, _)| turn < to_turn);
                if *code {
                    out.pre.retain(|(turn, ..)| turn < to_turn);
                    out.after.retain(|(turn, ..)| turn < to_turn);
                }
            }
            Entry::PreImage {
                turn, path, before, ..
            } => out.pre.push((*turn, path.clone(), before.clone())),
            Entry::After { turn, path, hash } => {
                out.after.push((*turn, path.clone(), hash.clone()))
            }
        }
    }
    out
}

fn blob_in(dir: &Path, hash: &ContentHash) -> Option<PathBuf> {
    hash.is_well_formed().then(|| dir.join(BLOBS).join(&hash.0))
}

impl CheckpointStore {
    /// Opens the store under `session_dir`, creating it when the session has none. Entries from
    /// an earlier run are read back; a torn last line is skipped.
    pub fn open(session_dir: &Path) -> io::Result<CheckpointStore> {
        let dir = session_dir.join(CHECKPOINT_DIR);
        for sub in [dir.clone(), dir.join(BLOBS)] {
            match owner_only::create_dir(&sub) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        let path = dir.join(MANIFEST);
        let mut entries = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut lines = text.split_inclusive('\n').peekable();
            while let Some(raw) = lines.next() {
                if raw.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Entry>(raw) {
                    Ok(entry) => entries.push(entry),
                    Err(e) if lines.peek().is_none() && !raw.ends_with('\n') => {
                        tracing::warn!(error = %e, "skipping the torn last line of the checkpoint manifest");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "skipping an unreadable checkpoint manifest line");
                    }
                }
            }
        }
        let mut manifest = owner_only::open(&path, true)?;
        // A complete last record can still lack its newline; the next append would join it.
        if std::fs::read(&path).is_ok_and(|b| !b.is_empty() && !b.ends_with(b"\n")) {
            manifest.write_all(b"\n")?;
        }
        let blob_bytes = std::fs::read_dir(dir.join(BLOBS))?
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        let mut captured = HashSet::new();
        let mut next_turn = 1;
        for entry in &entries {
            match entry {
                Entry::Epoch {} => {
                    captured.clear();
                    next_turn = 1;
                }
                Entry::Turn { turn, .. } => next_turn = next_turn.max(turn + 1),
                Entry::PreImage { turn, path, .. } => {
                    captured.insert((*turn, path.clone()));
                }
                _ => {}
            }
        }
        Ok(CheckpointStore {
            dir,
            state: Mutex::new(State {
                manifest,
                entries,
                settings: Settings::default(),
                blob_bytes,
                warned: HashSet::new(),
                io_warned: false,
                captured,
                next_turn,
            }),
        })
    }

    /// Applies the config, the workspace's memory directory (never captured or restored) and the
    /// channel warnings go to.
    pub fn configure(
        &self,
        limits: Checkpoints,
        memory_root: Option<PathBuf>,
        approver: Arc<dyn Approver>,
    ) {
        lock(&self.state, "checkpoint store").settings = Settings {
            limits: Some(limits),
            memory_root,
            approver: Some(approver),
        };
    }

    fn warn(&self, messages: Vec<String>) {
        let approver = lock(&self.state, "checkpoint store")
            .settings
            .approver
            .clone();
        for message in messages {
            match &approver {
                Some(approver) => approver.warn(&message),
                None => tracing::warn!("{message}"),
            }
        }
    }

    /// Appends `entry`; the in-memory list is the truth for this run even when the disk fails.
    fn append(state: &mut State, entry: Entry, warnings: &mut Vec<String>) {
        let written = serde_json::to_vec(&entry)
            .map_err(io::Error::other)
            .and_then(|mut line| {
                line.push(b'\n');
                state.manifest.write_all(&line)?;
                state.manifest.flush()
            });
        if let Err(e) = written
            && !std::mem::replace(&mut state.io_warned, true)
        {
            warnings.push(format!(
                "the checkpoint manifest could not be written, so /rewind may not restore recent edits: {}",
                fontaine_tools::error_chain::chain(&e)
            ));
        }
        state.entries.push(entry);
    }

    /// A turn started by the human. `conv_n` is the conversation log position the prompt will be
    /// appended after; `user_messages_before` sizes the intent trim of a later rewind.
    pub fn begin_turn(&self, conv_n: u64, prompt: &str, user_messages_before: usize) -> u64 {
        let mut warnings = Vec::new();
        let turn = {
            let mut state = lock(&self.state, "checkpoint store");
            let turn = state.next_turn;
            state.next_turn += 1;
            let entry = Entry::Turn {
                turn,
                conv_n,
                prompt: prompt.to_string(),
                user_messages_before,
            };
            Self::append(&mut state, entry, &mut warnings);
            turn
        };
        self.warn(warnings);
        turn
    }

    /// `/clear`: ordinals restart and no earlier turn can be rewound to.
    pub fn begin_epoch(&self) {
        let mut warnings = Vec::new();
        {
            let mut state = lock(&self.state, "checkpoint store");
            state.next_turn = 1;
            state.captured.clear();
            Self::append(&mut state, Entry::Epoch {}, &mut warnings);
        }
        self.warn(warnings);
    }

    /// Records that the conversation (and with `code` the files) went back to `to_turn`.
    pub fn mark_rewound(&self, to_turn: u64, code: bool) {
        let mut warnings = Vec::new();
        {
            let mut state = lock(&self.state, "checkpoint store");
            Self::append(&mut state, Entry::Rewind { to_turn, code }, &mut warnings);
        }
        self.warn(warnings);
    }

    /// The prompts of the current epoch, oldest first, that a rewind may still go back to.
    pub fn turns(&self) -> Vec<TurnSummary> {
        let state = lock(&self.state, "checkpoint store");
        let folded = fold(&state.entries);
        let memory = state.settings.memory_root.clone();
        folded
            .turns
            .iter()
            .map(|(turn, prompt)| TurnSummary {
                turn: *turn,
                prompt: prompt.clone(),
                files: folded
                    .pre
                    .iter()
                    .filter(|(t, path, _)| {
                        t >= turn && !in_memory(memory.as_deref(), Path::new(path))
                    })
                    .map(|(_, path, _)| path_key(path))
                    .collect::<HashSet<_>>()
                    .len(),
            })
            .collect()
    }

    /// What going back to the start of `turn` would do to the files. `None` when the turn is not
    /// one the picker offers.
    pub fn plan(&self, turn: u64, scope: &Scope) -> Option<RewindPlan> {
        let (conv_n, user_messages_before, prompt, folded, memory) = {
            let state = lock(&self.state, "checkpoint store");
            let folded = fold(&state.entries);
            if !folded.turns.iter().any(|(t, _)| *t == turn) {
                return None;
            }
            // The turn entry carries the numbers; the fold keeps only what the picker shows.
            let (conv_n, before, prompt) = state.entries.iter().rev().find_map(|e| match e {
                Entry::Turn {
                    turn: t,
                    conv_n,
                    user_messages_before,
                    prompt,
                } if *t == turn => Some((*conv_n, *user_messages_before, prompt.clone())),
                _ => None,
            })?;
            (
                conv_n,
                before,
                prompt,
                folded,
                state.settings.memory_root.clone(),
            )
        };
        // The earliest pre-image of a path gives its state at the start of `turn`.
        let mut earliest: BTreeMap<String, (u64, usize, Before)> = BTreeMap::new();
        for (order, (t, path, before)) in folded.pre.iter().enumerate() {
            if *t < turn {
                continue;
            }
            let slot = earliest
                .entry(path_key(path))
                .or_insert_with(|| (*t, order, before.clone()));
            if (*t, order) < (slot.0, slot.1) {
                *slot = (*t, order, before.clone());
            }
        }
        let originals: HashMap<String, &String> = folded
            .pre
            .iter()
            .map(|(_, path, _)| (path_key(path), path))
            .collect();
        let mut files = Vec::new();
        for (key, (_, _, before)) in earliest {
            let path = PathBuf::from(originals[&key]);
            let refusal = refusal_for(&path, scope, memory.as_deref());
            let last_after = folded
                .after
                .iter()
                .rfind(|(t, p, _)| *t >= turn && path_key(p) == key)
                .map(|(_, _, hash)| hash.clone());
            let changed_since = match last_after {
                Some(expected) => hash_file(&path) != expected,
                None => false,
            };
            let step = match (refusal, before) {
                (Some(why), _) => Step::Refused(why),
                (None, Before::Skipped { why }) => Step::Refused(Refusal::Skipped(why)),
                (None, Before::Absent) => Step::Delete,
                (None, Before::Blob { hash, unix_mode }) => {
                    if blob_in(&self.dir, &hash).is_some_and(|p| p.is_file()) {
                        Step::Restore { hash, unix_mode }
                    } else {
                        Step::Refused(Refusal::BlobLost)
                    }
                }
            };
            files.push(PlannedFile {
                path,
                step,
                changed_since,
            });
        }
        Some(RewindPlan {
            turn,
            prompt,
            conv_n,
            user_messages_before,
            files,
        })
    }

    /// Puts the files of `plan` back. Files changed since the last tool call are left alone
    /// unless `overwrite_changed`. Every path is checked again, because the disk may have changed
    /// since the plan was made.
    pub fn restore_code(
        &self,
        plan: &RewindPlan,
        scope: &Scope,
        overwrite_changed: bool,
    ) -> CodeReport {
        // The plan may be older than the user's last edit, so the files are compared again here.
        let (memory, folded) = {
            let state = lock(&self.state, "checkpoint store");
            (state.settings.memory_root.clone(), fold(&state.entries))
        };
        let mut report = CodeReport::default();
        for file in &plan.files {
            let path = &file.path;
            if let Step::Refused(why) = file.step {
                report.not_restorable.push((path.clone(), why));
                continue;
            }
            if let Some(why) = refusal_for(path, scope, memory.as_deref()) {
                report.not_restorable.push((path.clone(), why));
                continue;
            }
            let changed_now = folded
                .after
                .iter()
                .rfind(|(t, p, _)| {
                    *t >= plan.turn && path_key(p) == path_key(&path.to_string_lossy())
                })
                .is_some_and(|(_, _, expected)| hash_file(path) != *expected);
            if (file.changed_since || changed_now) && !overwrite_changed {
                report.kept_changed.push(path.clone());
                continue;
            }
            match &file.step {
                Step::Delete => match delete_regular(path) {
                    Ok(true) => {
                        report.deleted.push(path.clone());
                        self.note_restored(path);
                    }
                    Ok(false) => {}
                    Err(e) => report.failed.push((path.clone(), e)),
                },
                Step::Restore { hash, unix_mode } => {
                    match self.restore_blob(path, hash, *unix_mode) {
                        Ok(()) => {
                            report.restored.push(path.clone());
                            self.note_restored(path);
                        }
                        Err(RestoreError::Lost) => report
                            .not_restorable
                            .push((path.clone(), Refusal::BlobLost)),
                        Err(RestoreError::Io(e)) => report.failed.push((path.clone(), e)),
                    }
                }
                Step::Refused(_) => {}
            }
        }
        report
    }

    /// Records what the rewind left at `path`, so a later plan compares against it and not
    /// against the edit the rewind undid.
    fn note_restored(&self, path: &Path) {
        let turn = Self::current_turn(&lock(&self.state, "checkpoint store"));
        if let Some(turn) = turn {
            self.record_after(turn, path);
        }
    }

    fn restore_blob(
        &self,
        path: &Path,
        hash: &ContentHash,
        unix_mode: Option<u32>,
    ) -> Result<(), RestoreError> {
        let blob = blob_in(&self.dir, hash).ok_or(RestoreError::Lost)?;
        let bytes = std::fs::read(blob).map_err(|_| RestoreError::Lost)?;
        // The store is read back from disk, so the bytes are checked against their name.
        if ContentHash::of(&bytes) != *hash {
            return Err(RestoreError::Lost);
        }
        rig_tools::fs::write_atomic(path, &bytes)?;
        #[cfg(unix)]
        if let Some(mode) = unix_mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = unix_mode;
        Ok(())
    }

    /// The turn edits are attributed to: the latest the human started.
    fn current_turn(state: &State) -> Option<u64> {
        (state.next_turn > 1).then(|| state.next_turn - 1)
    }

    fn capture_before(&self, agent: &str, path: &Path) -> Option<u64> {
        let mut warnings = Vec::new();
        let (turn, limits, blob_bytes) = {
            let state = lock(&self.state, "checkpoint store");
            let limits = state.settings.limits.clone()?;
            if !limits.enabled || in_memory(state.settings.memory_root.as_deref(), path) {
                return None;
            }
            let turn = Self::current_turn(&state)?;
            if state
                .captured
                .contains(&(turn, path.to_string_lossy().into_owned()))
            {
                return Some(turn);
            }
            (turn, limits, state.blob_bytes)
        };
        // Read and hashed without the lock: the file can be large.
        let mut cause = None;
        let (before, written) = self.read_before(&limits, blob_bytes, path, &mut cause);
        {
            let mut state = lock(&self.state, "checkpoint store");
            let key = path.to_string_lossy().into_owned();
            if state.captured.insert((turn, key.clone())) {
                state.blob_bytes += written;
                if let Before::Skipped { why } = &before
                    && state.warned.insert(*why)
                {
                    let detail = cause.map(|c| format!(" ({c})")).unwrap_or_default();
                    warnings.push(format!(
                        "{} is not saved for /rewind: it is {why}{detail}; the edit goes ahead",
                        path.display()
                    ));
                }
                let entry = Entry::PreImage {
                    turn,
                    agent: agent.to_string(),
                    path: key,
                    before,
                };
                Self::append(&mut state, entry, &mut warnings);
            }
        }
        self.warn(warnings);
        Some(turn)
    }

    /// The state of `path` for the manifest, and how many new blob bytes were stored for it.
    fn read_before(
        &self,
        limits: &Checkpoints,
        blob_bytes: u64,
        path: &Path,
        cause: &mut Option<String>,
    ) -> (Before, u64) {
        let skipped = |why| (Before::Skipped { why }, 0);
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Before::Absent, 0),
            Err(_) => return skipped(SkipReason::Unreadable),
        };
        if !meta.is_file() {
            return skipped(SkipReason::NotRegular);
        }
        if meta.len() > limits.max_file_bytes {
            return skipped(SkipReason::TooLarge);
        }
        let Ok(bytes) = std::fs::read(path) else {
            return skipped(SkipReason::Unreadable);
        };
        let hash = ContentHash::of(&bytes);
        let blob = self.dir.join(BLOBS).join(&hash.0);
        let mut written = 0;
        if !blob.exists() {
            if blob_bytes.saturating_add(bytes.len() as u64) > limits.max_session_bytes {
                return skipped(SkipReason::Budget);
            }
            if let Err(e) = owner_only::write_atomic(&blob, &bytes) {
                *cause = Some(fontaine_tools::error_chain::chain(&e));
                return skipped(SkipReason::StoreWrite);
            }
            written = bytes.len() as u64;
        }
        #[cfg(unix)]
        let unix_mode = {
            use std::os::unix::fs::PermissionsExt;
            Some(meta.permissions().mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let unix_mode = None;
        (Before::Blob { hash, unix_mode }, written)
    }

    fn record_after(&self, turn: u64, path: &Path) {
        let mut warnings = Vec::new();
        let hash = hash_file(path);
        {
            let mut state = lock(&self.state, "checkpoint store");
            let entry = Entry::After {
                turn,
                path: path.to_string_lossy().into_owned(),
                hash,
            };
            Self::append(&mut state, entry, &mut warnings);
        }
        self.warn(warnings);
    }
}

enum RestoreError {
    Lost,
    Io(io::Error),
}

impl From<io::Error> for RestoreError {
    fn from(e: io::Error) -> Self {
        RestoreError::Io(e)
    }
}

fn in_memory(memory_root: Option<&Path>, path: &Path) -> bool {
    memory_root.is_some_and(|root| path.starts_with(root))
}

/// The hash of the regular file at `path`; `None` when it is absent or cannot be read.
fn hash_file(path: &Path) -> Option<ContentHash> {
    let mut file = File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(ContentHash(hex(&hasher.finalize())))
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Why `path` must not be touched by a restore, if it must not. The manifest is trusted data
/// from disk, not a permission: the path has to be inside the workspace now, and no link may lead
/// the write elsewhere.
fn refusal_for(path: &Path, scope: &Scope, memory_root: Option<&Path>) -> Option<Refusal> {
    if in_memory(memory_root, path) {
        return Some(Refusal::Memory);
    }
    let text = path.to_string_lossy();
    let Ok((root, resolved)) = scope.find(&text) else {
        return Some(Refusal::OutOfScope);
    };
    if path_key(&resolved.to_string_lossy()) != path_key(&text) {
        return Some(Refusal::Symlink);
    }
    if is_symlink(path) {
        return Some(Refusal::Symlink);
    }
    let root = &scope.roots()[root];
    let linked = path
        .ancestors()
        .skip(1)
        .take_while(|ancestor| *ancestor != root.as_path())
        .any(is_symlink);
    linked.then_some(Refusal::Symlink)
}

fn delete_regular(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => std::fs::remove_file(path).map(|()| true),
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The checkpoint store as the file tools' capture seam.
pub struct StoreCapture(pub Arc<CheckpointStore>);

struct Pending {
    store: Arc<CheckpointStore>,
    turn: u64,
    path: PathBuf,
}

impl PreImages for StoreCapture {
    fn before(&self, agent: &str, _tool: &str, path: &Path) -> Option<Box<dyn Capture>> {
        let turn = self.0.capture_before(agent, path)?;
        Some(Box::new(Pending {
            store: self.0.clone(),
            turn,
            path: path.to_path_buf(),
        }))
    }
}

impl Capture for Pending {
    // A failed call may still have changed the file, so what is on disk is recorded either way.
    fn finish(self: Box<Self>, _succeeded: bool) {
        self.store.record_after(self.turn, &self.path);
    }
}

#[cfg(test)]
mod tests;
