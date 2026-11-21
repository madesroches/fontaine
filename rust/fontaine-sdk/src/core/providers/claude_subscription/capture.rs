//! Opt-in capture of the `claude-subscription` traffic, for finding out why a replayed request
//! missed the prompt cache: the stream-json frames written to the CLI and the result event it
//! returned, one JSON Lines file per connection.
//!
//! Records are written unredacted and only to that file; nothing here reaches telemetry, the
//! ledger or a remote device. The switch is the `FONTAINE_CLAUDE_CAPTURE` environment variable
//! and nothing else, so no config scope can turn it on.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::protocol::Frame;
use crate::core::owner_only;
use crate::core::providers::warn::Warn;
use crate::tools::error_chain::chain;
use crate::tools::lock::lock;

/// The environment variable that turns the capture on.
pub(super) const SWITCH: &str = "FONTAINE_CLAUDE_CAPTURE";
const DIR: &str = "claude-capture";
const ACCEPTED: &str = "unset, empty, `0`, `1`, `full` or `delta`";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// Every request's full frame list as sent.
    Full,
    /// Only the messages new since the previous request of the conversation.
    Delta,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Full => "full",
            Mode::Delta => "delta",
        }
    }
}

/// The mode a switch value selects, `Ok(None)` for off and `Err` for a value nobody defined.
fn parse_switch(value: Option<OsString>) -> Result<Option<Mode>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.to_str() {
        Some("" | "0") => Ok(None),
        Some("1" | "full") => Ok(Some(Mode::Full)),
        Some("delta") => Ok(Some(Mode::Delta)),
        _ => Err(()),
    }
}

enum Files {
    Pending,
    Ready { bodies: PathBuf },
    Failed,
}

struct Baseline {
    prints: Vec<u64>,
    next: usize,
}

struct State {
    files: Files,
    out: Option<File>,
    sessions: HashMap<String, Baseline>,
}

pub(super) struct Capture {
    dir: PathBuf,
    stem: String,
    mode: Mode,
    warn: Warn,
    state: Mutex<State>,
}

/// Names one recorded request so its result lands beside it.
pub(super) struct Handle {
    capture: Arc<Capture>,
    session: String,
    index: usize,
}

impl Capture {
    /// The capture the switch asks for, or `None` when it is off. A value that is not one of the
    /// accepted ones is off with one warning, so a typo neither captures nor silently does nothing.
    pub(super) fn from_env(
        env: &(dyn Fn(&str) -> Option<OsString> + Send + Sync),
        data_dir: &Path,
        session: &str,
        warn: Warn,
    ) -> Option<Arc<Capture>> {
        let mode = match parse_switch(env(SWITCH)) {
            Ok(mode) => mode?,
            Err(()) => {
                warn.emit(&format!(
                    "{SWITCH} is not recognised, so the claude-subscription capture stays off; use {ACCEPTED}"
                ));
                return None;
            }
        };
        let stem: String = session
            .chars()
            .take(64)
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let token = uuid::Uuid::new_v4().simple().to_string();
        Some(Arc::new(Capture {
            dir: data_dir.join(DIR),
            stem: format!("{stem}-{}", &token[..8]),
            mode,
            warn,
            state: Mutex::new(State {
                files: Files::Pending,
                out: None,
                sessions: HashMap::new(),
            }),
        }))
    }

    /// Whether `record_request` needs the frames as they were before the cache layout.
    pub(super) fn wants_pre_frames(&self) -> bool {
        self.mode == Mode::Delta
    }

    /// Creates the capture file and the directory for the CLI's own raw bodies on first use, and
    /// returns the latter. Every caller gets the same answer; a failure warns once, turns the
    /// capture off and returns `None`, and never fails the launch.
    pub(super) fn ensure_created(&self) -> Option<PathBuf> {
        let mut state = lock(&self.state, "claude capture");
        match &state.files {
            Files::Ready { bodies } => return Some(bodies.clone()),
            Files::Failed => return None,
            Files::Pending => {}
        }
        match self.create() {
            Ok((file, path, bodies)) => {
                self.warn.emit(&format!(
                    "claude-subscription capture is on ({SWITCH}): the frames sent to the Claude CLI and its results are written unredacted, prompts and file contents included, to {}; delete {} when you are done",
                    path.display(),
                    self.dir.display()
                ));
                state.out = Some(file);
                state.files = Files::Ready {
                    bodies: bodies.clone(),
                };
                Some(bodies)
            }
            Err(error) => {
                self.warn.emit(&format!(
                    "claude-subscription capture is off: {} could not be set up: {}",
                    self.dir.display(),
                    chain(&error)
                ));
                state.files = Files::Failed;
                None
            }
        }
    }

    fn create(&self) -> io::Result<(File, PathBuf, PathBuf)> {
        match owner_only::create_dir(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let meta = std::fs::symlink_metadata(&self.dir)?;
                if !meta.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "exists and is not a directory",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
        let path = self.dir.join(format!("{}.jsonl", self.stem));
        let file = owner_only::create_new(&path)?;
        let bodies = self.dir.join(format!("{}.bodies", self.stem));
        owner_only::create_dir(&bodies)?;
        Ok((file, path, bodies))
    }

    /// Records one request: `prints` are the pre-layout hashes of its frames, `pre` the frames
    /// before the layout (delta mode only) and `post` the frames as sent, query frame last. The
    /// conversation's baseline advances here, even if the request is later cut or fails.
    pub(super) fn record_request(
        self: &Arc<Self>,
        session: &str,
        prints: &[u64],
        pre: Option<&[Frame]>,
        post: &[Frame],
    ) -> Handle {
        let mut state = lock(&self.state, "claude capture");
        let baseline = state
            .sessions
            .entry(session.to_string())
            .or_insert_with(|| Baseline {
                prints: Vec::new(),
                next: 0,
            });
        let (common, diverged_at) = compare(&baseline.prints, prints);
        let index = baseline.next;
        baseline.next += 1;
        baseline.prints = prints.to_vec();
        let (listed, from) = match (self.mode, pre) {
            (Mode::Delta, Some(pre)) => (pre, common),
            _ => (post, 0),
        };
        let frames: Vec<Value> = listed
            .iter()
            .enumerate()
            .skip(from)
            .map(|(i, frame)| json!({"i": i, "frame": frame.value}))
            .collect();
        let record = json!({
            "kind": "request",
            "session": session,
            "index": index,
            "at": unix_ms(),
            "mode": self.mode.name(),
            "hashes": prints.iter().map(|p| format!("{p:016x}")).collect::<Vec<_>>(),
            "diverged_at": diverged_at,
            "frames": frames,
        });
        self.append(&mut state, &record);
        Handle {
            capture: self.clone(),
            session: session.to_string(),
            index,
        }
    }

    fn append(&self, state: &mut State, record: &Value) {
        let Some(out) = state.out.as_mut() else {
            return;
        };
        let mut line = record.to_string();
        line.push('\n');
        if let Err(error) = out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
            self.warn.emit(&format!(
                "claude-subscription capture stopped: writing to {} failed: {}",
                self.dir.display(),
                chain(&error)
            ));
            state.out = None;
        }
    }
}

impl Handle {
    /// Records the CLI's final result event, verbatim.
    pub(super) fn record_result(&self, line: &[u8]) {
        let result = serde_json::from_slice::<Value>(line)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(line).into_owned()));
        let record = json!({
            "kind": "result",
            "session": self.session,
            "index": self.index,
            "at": unix_ms(),
            "result": result,
        });
        let mut state = lock(&self.capture.state, "claude capture");
        self.capture.append(&mut state, &record);
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The length of the prefix `prev` and `new` share, and where `new` stops extending `prev`: the
/// first differing frame, or `new.len()` when it is a strict prefix of `prev`.
fn compare(prev: &[u64], new: &[u64]) -> (usize, Option<usize>) {
    let common = prev.iter().zip(new).take_while(|(a, b)| a == b).count();
    let diverged = if common == prev.len() {
        None
    } else if common == new.len() {
        Some(new.len())
    } else {
        Some(common)
    };
    (common, diverged)
}

/// A request's frames rebuilt from the capture.
#[derive(Clone, Debug, PartialEq)]
pub struct Reassembled {
    pub frames: Vec<Frame>,
    /// Where the request stopped extending the previous one, as recorded.
    pub diverged_at: Option<usize>,
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("a capture record has no usable {what}"),
    )
}

fn records(path: &Path) -> io::Result<Vec<Value>> {
    std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        })
        .collect()
}

/// The frames of request `index` of `session` in the capture at `path`. A full record holds them
/// all; a delta record is replayed over the earlier ones, so it yields the messages and not the
/// cache marker or reminder placement.
pub fn reassemble(path: &Path, session: &str, index: usize) -> io::Result<Reassembled> {
    let mut values: Vec<Value> = Vec::new();
    for record in records(path)? {
        if record["kind"] != "request" || record["session"] != session {
            continue;
        }
        let at = record["index"].as_u64().ok_or_else(|| invalid("index"))? as usize;
        if at > index {
            continue;
        }
        let hashes = record["hashes"]
            .as_array()
            .ok_or_else(|| invalid("hashes"))?
            .len();
        let entries = record["frames"]
            .as_array()
            .ok_or_else(|| invalid("frames"))?;
        let start = match entries.first() {
            Some(entry) => entry["i"].as_u64().ok_or_else(|| invalid("frame index"))? as usize,
            None => hashes,
        };
        values.truncate(start);
        values.extend(entries.iter().map(|entry| entry["frame"].clone()));
        if at == index {
            let last = values.len().saturating_sub(1);
            return Ok(Reassembled {
                frames: values
                    .into_iter()
                    .enumerate()
                    .map(|(i, value)| Frame {
                        value,
                        query: i == last,
                    })
                    .collect(),
                diverged_at: record["diverged_at"].as_u64().map(|n| n as usize),
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "no request {index} of session {session} in {}",
            path.display()
        ),
    ))
}

/// How two requests of a conversation differ.
#[derive(Clone, Debug, PartialEq)]
pub struct Diff {
    /// The later request's recorded divergence from the one before it.
    pub diverged_at: Option<usize>,
    /// The first frame index at which the two differ.
    pub first_difference: Option<usize>,
    /// The differences inside that frame, one per JSON path.
    pub changes: Vec<String>,
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.diverged_at {
            Some(n) => writeln!(f, "diverged from the previous request at frame {n}")?,
            None => writeln!(f, "extends the previous request")?,
        }
        match self.first_difference {
            Some(n) => writeln!(f, "first differing frame: {n}")?,
            None => writeln!(f, "no differing frame")?,
        }
        for change in &self.changes {
            writeln!(f, "  {change}")?;
        }
        Ok(())
    }
}

/// Compares requests `a_index` and `b_index` of `session`: the later one's divergence first, then
/// the first differing frame and a path-level diff of it.
pub fn diff(path: &Path, session: &str, a_index: usize, b_index: usize) -> io::Result<Diff> {
    let a = reassemble(path, session, a_index)?;
    let b = reassemble(path, session, b_index)?;
    let differing = a
        .frames
        .iter()
        .zip(&b.frames)
        .position(|(x, y)| x.value != y.value);
    let first_difference = differing
        .or_else(|| (a.frames.len() != b.frames.len()).then(|| a.frames.len().min(b.frames.len())));
    let mut changes = Vec::new();
    if let Some(i) = first_difference {
        let path = format!("frames[{i}]");
        match (a.frames.get(i), b.frames.get(i)) {
            (Some(x), Some(y)) => diff_value(&path, &x.value, &y.value, &mut changes),
            (Some(_), None) => changes.push(format!("{path}: only in request {a_index}")),
            (None, Some(_)) => changes.push(format!("{path}: only in request {b_index}")),
            (None, None) => {}
        }
    }
    Ok(Diff {
        diverged_at: b.diverged_at,
        first_difference,
        changes,
    })
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= 200 {
        return text;
    }
    let mut cut: String = text.chars().take(200).collect();
    cut.push('…');
    cut
}

fn diff_value(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    if a == b {
        return;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (key, left) in x {
                let here = format!("{path}.{key}");
                match y.get(key) {
                    Some(right) => diff_value(&here, left, right, out),
                    None => out.push(format!("{here}: removed")),
                }
            }
            for key in y.keys().filter(|key| !x.contains_key(*key)) {
                out.push(format!("{path}.{key}: added"));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            for i in 0..x.len().max(y.len()) {
                let here = format!("{path}[{i}]");
                match (x.get(i), y.get(i)) {
                    (Some(left), Some(right)) => diff_value(&here, left, right, out),
                    (Some(_), None) => out.push(format!("{here}: removed")),
                    _ => out.push(format!("{here}: added")),
                }
            }
        }
        _ => out.push(format!("{path}: {} -> {}", short(a), short(b))),
    }
}

#[cfg(test)]
mod tests;
