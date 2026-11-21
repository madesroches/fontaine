//! The two cache layouts a request can take, and the runtime check that falls back from the
//! native one.
//!
//! The CLI adds a `<system-reminder>` carrying the account's context to the message that asks
//! for a response, so the next request replays that message without it and rewrites it. The
//! native layout replays the reminder exactly where the CLI put it on every replayed user
//! message and puts the adapter's cache marker on the final message, so each replayed message is
//! byte-identical to the request that last wrote it. The legacy layout puts the marker on the
//! last replayed user message instead, before the point where the CLI's addition changes the
//! bytes. The reminder's wording is learned from the CLI, not assumed (see `reminder`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::tools::error_chain::chain;
use crate::tools::lock::{lock, lock_recovering};
use serde_json::{Value, json};

use super::cli::CliContext;
use super::protocol::Frame;
use super::reminder::Reminder;
use crate::core::file_lock;
use crate::core::owner_only;
use crate::core::providers::warn::Warn;

/// The oldest CLI whose reminder placement was measured; the wording is learned, the placement
/// `place` implements is not.
const TEMPLATE_SINCE: (u32, u32, u32) = (2, 1, 319);

/// Identifies where `apply` places the reminder. An edit to that bumps it, so a verdict recorded
/// against the old placement stops applying.
const TEMPLATE_REVISION: u32 = 2;

/// The prompt-cache breakpoint the adapter places itself. The hour matches the CLI's own
/// breakpoint, which an earlier one may not undercut.
const CACHE_MARKER: &str = r#"{"type": "ephemeral", "ttl": "1h"}"#;

/// A read short of the expected one by more than this is a broken replay.
const READ_SLACK: u64 = 64;
/// The 1 h marker's lifetime, less a margin.
const MAX_GAP: Duration = Duration::from_secs(55 * 60);
/// The API looks back 20 blocks for an earlier write; keep a margin inside it.
const MAX_ADDED_BLOCKS: usize = 18;
/// How long a persisted verdict holds before the version is measured again. A version string does
/// not change when the API's cache behaviour does, so age is the one signal always available.
const VERDICT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How many conversations' last requests are remembered.
const RECORDED: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
    /// The reminder is replayed and the marker sits on the final frame.
    Native,
    /// The marker sits on the last replayed user frame.
    Legacy,
}

/// Whether the placement `place` implements was measured on this CLI version.
pub(super) fn replayable(version: (u32, u32, u32)) -> bool {
    version >= TEMPLATE_SINCE
}

/// The layout for a request whose process reported `context` and whose reminder was `learned`.
/// Native is earned by an observed reminder: without one the layout is the earlier one.
pub(super) fn layout(
    context: &CliContext,
    learned: Option<&Reminder>,
    verdicts: &Verdicts,
) -> Layout {
    let native = context.email.is_some()
        && context
            .version
            .zip(learned)
            .is_some_and(|(version, learned)| {
                replayable(version) && !verdicts.contains(version, &learned.wording)
            });
    if native {
        Layout::Native
    } else {
        Layout::Legacy
    }
}

/// Replays the CLI's reminder and places the cache marker for `layout`. Run after the session id
/// is derived so neither changes it.
pub(super) fn apply(
    frames: &mut [Frame],
    context: &CliContext,
    learned: Option<&Reminder>,
    layout: Layout,
) {
    match layout {
        Layout::Legacy => {
            let Some((_, history)) = frames.split_last_mut() else {
                return;
            };
            if let Some(frame) = history.iter_mut().rev().find(|frame| is_user(frame)) {
                mark(frame);
            }
        }
        Layout::Native => {
            let (Some(email), Some(learned)) = (context.email.as_deref(), learned) else {
                return;
            };
            let reminder = learned.with_email(email);
            let Some((_, history)) = frames.split_last_mut() else {
                return;
            };
            let mut index = 0;
            while index < history.len() {
                if !is_user(&history[index]) {
                    index += 1;
                    continue;
                }
                let end = history[index..]
                    .iter()
                    .position(|frame| !is_user(frame))
                    .map_or(history.len(), |offset| index + offset);
                // A run that reaches the final frame is merged into it by the CLI and carries
                // the reminder there, so only a run an assistant frame follows is replayed.
                if end < history.len() {
                    place(&mut history[index..end], &reminder);
                }
                index = end;
            }
            if frames.len() > 1 && frames[..frames.len() - 1].iter().any(is_user) {
                mark(frames.last_mut().expect("frames are not empty"));
            }
        }
    }
}

fn is_user(frame: &Frame) -> bool {
    frame.value.get("type").and_then(Value::as_str) == Some("user")
}

enum Kind {
    Text { empty: bool },
    ToolResult,
    Media,
}

fn kinds(frame: &Frame) -> Vec<Kind> {
    match &frame.value["message"]["content"] {
        Value::String(text) => vec![Kind::Text {
            empty: text.is_empty(),
        }],
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => Kind::Text {
                    empty: block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty),
                },
                Some("tool_result") => Kind::ToolResult,
                _ => Kind::Media,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// Puts the reminder where the CLI would on the merged message a run of user frames makes. A
/// shape the CLI's placement was not measured for is left unchanged: it costs one rewrite on the
/// request after it, and replays the same way from then on.
fn place(run: &mut [Frame], reminder: &str) {
    let merged: Vec<Kind> = run.iter().flat_map(kinds).collect();
    let has_result = merged.iter().any(|kind| matches!(kind, Kind::ToolResult));
    let Some(last) = merged.last() else {
        return;
    };
    if !has_result {
        if matches!(last, Kind::Media) {
            return;
        }
        let content = &mut run[0].value["message"]["content"];
        let lead = text_block(&format!("{reminder}\n"));
        match content {
            Value::String(text) => *content = json!([lead, text_block(text)]),
            Value::Array(blocks) => blocks.insert(0, lead),
            _ => {}
        }
        return;
    }
    let frame = run.last_mut().expect("a run holds a frame");
    let Some(blocks) = frame.value["message"]["content"].as_array_mut() else {
        return;
    };
    match last {
        Kind::ToolResult => {
            if let Some(result) = blocks.last_mut() {
                append_to_result(result, reminder);
            }
        }
        Kind::Text { empty: true } => {
            let kept = blocks
                .iter()
                .rposition(|block| !is_empty_text(block))
                .map_or(0, |at| at + 1);
            let after_result = kept > 0
                && blocks[kept - 1].get("type").and_then(Value::as_str) == Some("tool_result");
            if after_result {
                blocks.truncate(kept);
            }
            blocks.push(text_block(reminder));
        }
        Kind::Text { empty: false } => blocks.push(text_block(reminder)),
        Kind::Media => {}
    }
}

/// Whether `place` would leave this final frame unchanged although the CLI still adds its reminder.
fn unreplayable(frame: &Frame) -> bool {
    match kinds(frame).last() {
        Some(Kind::Media) => true,
        Some(Kind::ToolResult) => frame.value["message"]["content"]
            .as_array()
            .and_then(|blocks| blocks.last())
            .and_then(|result| result["content"].as_array())
            .and_then(|blocks| blocks.last())
            .is_some_and(|block| block.get("type").and_then(Value::as_str) != Some("text")),
        _ => false,
    }
}

fn is_empty_text(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("text")
        && block
            .get("text")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
}

/// The CLI appends the reminder to the last tool result's content: to string content after
/// trimming its trailing whitespace and a blank line; to array content as a new text block, after
/// a single newline appended to a last text block.
fn append_to_result(result: &mut Value, reminder: &str) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    match object.get_mut("content") {
        None | Some(Value::Null) => {
            object.insert("content".to_string(), Value::String(reminder.to_string()));
        }
        Some(Value::String(text)) if text.is_empty() => *text = reminder.to_string(),
        Some(Value::String(text)) => {
            text.truncate(text.trim_end().len());
            text.push_str("\n\n");
            text.push_str(reminder);
        }
        Some(Value::Array(blocks)) if blocks.is_empty() => blocks.push(text_block(reminder)),
        Some(Value::Array(blocks)) => {
            let Some(last) = blocks
                .last_mut()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            else {
                return;
            };
            if let Some(Value::String(text)) = last.get_mut("text") {
                text.push('\n');
            }
            blocks.push(text_block(reminder));
        }
        Some(_) => {}
    }
}

/// Puts the one cache breakpoint on the frame's last cacheable block.
fn mark(frame: &mut Frame) {
    let marker: Value = serde_json::from_str(CACHE_MARKER).expect("the marker is valid JSON");
    let content = &mut frame.value["message"]["content"];
    if let Value::String(text) = content {
        *content = json!([{"type": "text", "text": text.as_str(), "cache_control": marker}]);
        return;
    }
    let cacheable = |block: &&mut Value| match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some("image" | "document" | "tool_result") => true,
        _ => false,
    };
    if let Some(block) = content
        .as_array_mut()
        .and_then(|blocks| blocks.iter_mut().rev().find(cacheable))
        .and_then(Value::as_object_mut)
    {
        block.insert("cache_control".to_string(), marker);
    }
}

/// What a request's result said about the prompt cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CacheUsage {
    pub read: u64,
    pub write_1h: u64,
    // The rest is for the log: a write that landed in the 5-minute bucket looks like a missing
    // read, and the check compares only `read` and `write_1h`.
    pub input: u64,
    pub write_total: u64,
    pub write_5m: u64,
}

impl CacheUsage {
    /// Absent when the result carries no cache read count.
    pub(super) fn from_result(usage: &Value) -> Option<Self> {
        let count = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
        let creation = usage.get("cache_creation");
        Some(CacheUsage {
            read: usage.get("cache_read_input_tokens")?.as_u64()?,
            write_1h: count(creation.and_then(|c| c.get("ephemeral_1h_input_tokens"))),
            input: count(usage.get("input_tokens")),
            write_total: count(usage.get("cache_creation_input_tokens")),
            write_5m: count(creation.and_then(|c| c.get("ephemeral_5m_input_tokens"))),
        })
    }
}

impl std::fmt::Display for CacheUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "read={} input={} write={} write_1h={} write_5m={}",
            self.read, self.input, self.write_total, self.write_1h, self.write_5m
        )
    }
}

/// What the self-check compares between two requests of one conversation.
#[derive(Clone, Debug)]
pub(super) struct Record {
    prints: Vec<u64>,
    blocks: Vec<usize>,
    merged_tail: bool,
    unreplayable_tail: bool,
    usage: CacheUsage,
    settings: u64,
    thinking: bool,
    at: Instant,
}

/// A request as it was sent, kept until its result arrives.
#[derive(Debug)]
pub(super) struct Sent {
    session_id: String,
    version: Option<(u32, u32, u32)>,
    layout: Layout,
    /// The wording of the reminder the native layout replayed.
    wording: Option<String>,
    prints: Vec<u64>,
    blocks: Vec<usize>,
    merged_tail: bool,
    unreplayable_tail: bool,
    settings: u64,
    thinking: bool,
}

/// A hash of what the API's message cache is keyed on besides the messages: the effort level and
/// the extra request body (thinking among it).
pub(super) fn settings_print(effort: Option<&str>, settings: &Value) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    effort.hash(&mut hasher);
    settings.to_string().hash(&mut hasher);
    hasher.finish()
}

/// A hash per frame, taken before `apply` so it names the conversation and not the layout. It
/// covers the message only: the same message is a final frame in one request and a history frame
/// in the next.
pub(super) fn fingerprints(frames: &[Frame]) -> Vec<u64> {
    frames
        .iter()
        .map(|frame| {
            let mut hasher = std::hash::DefaultHasher::new();
            frame.value["message"].to_string().hash(&mut hasher);
            hasher.finish()
        })
        .collect()
}

impl Sent {
    /// `prints` are the frames' fingerprints before `apply`; `frames` are the frames after it.
    pub(super) fn new(
        session_id: &str,
        version: Option<(u32, u32, u32)>,
        layout: Layout,
        wording: Option<&str>,
        prints: Vec<u64>,
        frames: &[Frame],
    ) -> Self {
        let blocks = frames
            .iter()
            .map(|frame| match &frame.value["message"]["content"] {
                Value::Array(blocks) => blocks.len(),
                _ => 1,
            })
            .collect();
        let merged_tail = frames.len() >= 2
            && is_user(&frames[frames.len() - 2])
            && is_user(&frames[frames.len() - 1]);
        let unreplayable_tail = frames.last().is_some_and(unreplayable);
        let thinking = frames.iter().any(|frame| {
            frame.value["message"]["content"]
                .as_array()
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block.get("type").and_then(Value::as_str) == Some("thinking"))
                })
        });
        Sent {
            session_id: session_id.to_string(),
            version,
            layout,
            wording: wording.map(str::to_string),
            prints,
            blocks,
            merged_tail,
            unreplayable_tail,
            settings: 0,
            thinking,
        }
    }

    pub(super) fn with_settings(mut self, settings: u64) -> Self {
        self.settings = settings;
        self
    }
}

/// What a broken replay was judged on, kept so the verdict can be audited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Evidence {
    pub read: u64,
    pub expected: u64,
    pub gap: Duration,
    pub prev_read: u64,
    pub prev_write_1h: u64,
    pub thinking: bool,
}

/// Why a pair of requests gave no verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skip {
    FirstRequest,
    /// Where the two requests' frame fingerprints first differ, and how many each has.
    NotExtended {
        first_diff: usize,
        prev_len: usize,
        now_len: usize,
    },
    MergedTail,
    UnreplayableTail,
    SettingsChanged,
    ColdPrevious,
    GapTooLong,
    TooManyAdded,
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Skip::FirstRequest => "first_request",
            Skip::NotExtended { .. } => "not_extended",
            Skip::MergedTail => "merged_tail",
            Skip::UnreplayableTail => "unreplayable_tail",
            Skip::SettingsChanged => "settings_changed",
            Skip::ColdPrevious => "cold_previous",
            Skip::GapTooLong => "gap_too_long",
            Skip::TooManyAdded => "too_many_added",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Broken(Evidence),
    Healthy,
    Skipped(Skip),
}

/// Whether `now` shows a replay the cache did not read back. Anything that makes the comparison
/// unsound gives no verdict.
fn check(prev: &Record, now: &Record) -> Outcome {
    let extends =
        now.prints.len() > prev.prints.len() && now.prints[..prev.prints.len()] == prev.prints[..];
    if !extends {
        let first_diff = prev
            .prints
            .iter()
            .zip(&now.prints)
            .position(|(a, b)| a != b)
            .unwrap_or(prev.prints.len().min(now.prints.len()));
        return Outcome::Skipped(Skip::NotExtended {
            first_diff,
            prev_len: prev.prints.len(),
            now_len: now.prints.len(),
        });
    }
    if prev.merged_tail {
        return Outcome::Skipped(Skip::MergedTail);
    }
    if prev.unreplayable_tail {
        return Outcome::Skipped(Skip::UnreplayableTail);
    }
    if prev.settings != now.settings {
        return Outcome::Skipped(Skip::SettingsChanged);
    }
    // A request that read nothing was a cold start: its write is not evidence the prefix replays.
    if prev.usage.read == 0 {
        return Outcome::Skipped(Skip::ColdPrevious);
    }
    let gap = now.at.saturating_duration_since(prev.at);
    if gap >= MAX_GAP {
        return Outcome::Skipped(Skip::GapTooLong);
    }
    let added: usize = now.blocks[prev.prints.len()..].iter().sum();
    if added > MAX_ADDED_BLOCKS {
        return Outcome::Skipped(Skip::TooManyAdded);
    }
    let expected = prev.usage.read + prev.usage.write_1h;
    if now.usage.read + READ_SLACK < expected {
        Outcome::Broken(Evidence {
            read: now.usage.read,
            expected,
            gap,
            prev_read: prev.usage.read,
            prev_write_1h: prev.usage.write_1h,
            thinking: now.thinking,
        })
    } else {
        Outcome::Healthy
    }
}

#[derive(Default)]
struct Records {
    by_session: HashMap<String, Record>,
    order: VecDeque<String>,
}

impl Records {
    fn take(&mut self, session: &str) -> Option<Record> {
        let record = self.by_session.remove(session)?;
        self.order.retain(|id| id != session);
        Some(record)
    }

    fn put(&mut self, session: String, record: Record) {
        self.order.push_back(session.clone());
        self.by_session.insert(session, record);
        while self.order.len() > RECORDED {
            if let Some(oldest) = self.order.pop_front() {
                self.by_session.remove(&oldest);
            }
        }
    }
}

static RECORDS: LazyLock<Mutex<Records>> = LazyLock::new(|| Mutex::new(Records::default()));

const VERDICTS_FILE: &str = "claude-cache-verdicts.json";

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A persisted verdict: when it was reached, and the usage it was judged on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    at: u64,
    read: Option<u64>,
    expected: Option<u64>,
    prev_read: Option<u64>,
    prev_write_1h: Option<u64>,
    thinking: Option<bool>,
}

impl Entry {
    fn fresh(&self, now: u64) -> bool {
        now.saturating_sub(self.at) < VERDICT_TTL.as_secs()
    }
}

/// The CLI versions whose replay the self-check saw miss the cache twice in a row, kept in the
/// data dir so a restart keeps the fallback instead of paying a full-history rewrite to
/// rediscover it. A verdict expires, so a transient condition cannot disable the optimization for
/// good.
pub(super) struct Verdicts {
    path: PathBuf,
    warn: Warn,
    // The load happens during model resolution, before the session's warning sink exists, so the
    // warning waits for the first request.
    load_warning: Mutex<Option<String>>,
    // Only chooses a cache layout, so a poisoned lock is recovered; the file is the durable record.
    broken: Mutex<BTreeMap<Key, Entry>>,
    // One shortfall is not yet a verdict; kept in memory only, across conversations.
    struck: Mutex<BTreeSet<Key>>,
}

/// A verdict judges one CLI version replaying one reminder wording, so a new wording starts clean.
type Key = ((u32, u32, u32), String);

static STORES: LazyLock<Mutex<HashMap<PathBuf, Arc<Verdicts>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(|part| part.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The unexpired broken versions a verdicts file holds for the current template revision. An
/// entry without a timestamp predates expiry and counts as expired; one without a wording was
/// judged against a reminder that is not known.
fn parse(text: &str, now: u64) -> Result<BTreeMap<Key, Entry>, serde_json::Error> {
    let value: Value = serde_json::from_str(text)?;
    Ok(value
        .get("broken")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry.get("template").and_then(Value::as_u64) == Some(TEMPLATE_REVISION.into())
        })
        .filter_map(|entry| {
            let version = parse_version(entry.get("cli")?.as_str()?)?;
            let wording = entry.get("wording")?.as_str()?.to_string();
            let parsed = Entry {
                at: entry.get("at")?.as_u64()?,
                read: entry.get("read").and_then(Value::as_u64),
                expected: entry.get("expected").and_then(Value::as_u64),
                prev_read: entry.get("prev_read").and_then(Value::as_u64),
                prev_write_1h: entry.get("prev_write_1h").and_then(Value::as_u64),
                thinking: entry.get("thinking").and_then(Value::as_bool),
            };
            parsed.fresh(now).then_some(((version, wording), parsed))
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    /// The version already had an unexpired verdict.
    Known,
    Saved,
    Unsaved,
}

impl Verdicts {
    /// The one store for `data_dir`'s verdicts file, shared by every `Cli` over that directory
    /// so a verdict found through one account is seen, and warned about, once for all.
    pub(super) fn shared(data_dir: &Path, warn: &Warn) -> Arc<Verdicts> {
        let path = data_dir.join(VERDICTS_FILE);
        let mut stores = lock(&STORES, "claude cache verdict stores");
        stores
            .entry(path.clone())
            .or_insert_with(|| Arc::new(Verdicts::load(path, warn.clone())))
            .clone()
    }

    fn load(path: PathBuf, warn: Warn) -> Verdicts {
        let mut load_warning = None;
        let broken = match file_lock::read_shared(&path) {
            Ok(text) => parse(&text, unix_now()).unwrap_or_else(|e| {
                load_warning = Some(format!(
                    "the Claude CLI cache verdicts in {} could not be parsed and are ignored: {}",
                    path.display(),
                    chain(&e)
                ));
                BTreeMap::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                load_warning = Some(format!(
                    "the Claude CLI cache verdicts in {} could not be read and are ignored: {}",
                    path.display(),
                    chain(&e)
                ));
                BTreeMap::new()
            }
        };
        Verdicts {
            path,
            warn,
            load_warning: Mutex::new(load_warning),
            broken: Mutex::new(broken),
            struck: Mutex::new(BTreeSet::new()),
        }
    }

    /// Every request's layout choice reads this, so it carries the held load warning out.
    pub(super) fn contains(&self, version: (u32, u32, u32), wording: &str) -> bool {
        // Held only to take a string; a poisoned lock costs at most a warning.
        let held = lock_recovering(&self.load_warning).take();
        if let Some(message) = held {
            self.warn.emit(&message);
        }
        lock_recovering(&self.broken)
            .get(&(version, wording.to_string()))
            .is_some_and(|entry| entry.fresh(unix_now()))
    }

    /// Records a shortfall for `version`; true when the previous observation was one too, which
    /// is what makes a verdict.
    pub(super) fn strike(&self, version: (u32, u32, u32), wording: &str) -> bool {
        // Advisory state: a poisoned lock costs at most a missed or early verdict.
        !lock_recovering(&self.struck).insert((version, wording.to_string()))
    }

    /// A healthy pair shows the version replays, so an earlier shortfall no longer counts.
    pub(super) fn clear_strike(&self, version: (u32, u32, u32), wording: &str) {
        lock_recovering(&self.struck).remove(&(version, wording.to_string()));
    }

    /// Adds a version to the broken set and persists it. A failed write is warned about and
    /// leaves the in-memory verdict in place.
    pub(super) fn mark(
        &self,
        version: (u32, u32, u32),
        wording: &str,
        evidence: &Evidence,
    ) -> Mark {
        let key = (version, wording.to_string());
        let now = unix_now();
        let entry = Entry {
            at: now,
            read: Some(evidence.read),
            expected: Some(evidence.expected),
            prev_read: Some(evidence.prev_read),
            prev_write_1h: Some(evidence.prev_write_1h),
            thinking: Some(evidence.thinking),
        };
        {
            let mut broken = lock_recovering(&self.broken);
            if broken.get(&key).is_some_and(|known| known.fresh(now)) {
                return Mark::Known;
            }
            broken.insert(key.clone(), entry);
        }
        match self.persist(key, entry, now) {
            Ok(()) => Mark::Saved,
            Err(e) => {
                self.warn.emit(&format!(
                    "the Claude CLI cache verdict could not be saved to {}: {}",
                    self.path.display(),
                    chain(&e)
                ));
                Mark::Unsaved
            }
        }
    }

    fn persist(&self, key: Key, entry: Entry, now: u64) -> std::io::Result<()> {
        let _guard = file_lock::lock(&self.path)?;
        // Merge what another process wrote since the load. A plain read: `read_shared` would wait
        // on the lock held here. An unreadable file is replaced by the write below, and expired
        // entries are not carried over.
        let mut broken = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| parse(&text, now).ok())
            .unwrap_or_default();
        broken.insert(key, entry);
        let entries: Vec<Value> = broken
            .iter()
            .map(|(((a, b, c), wording), entry)| {
                let mut value = json!({
                    "cli": format!("{a}.{b}.{c}"),
                    "wording": wording,
                    "template": TEMPLATE_REVISION,
                    "at": entry.at,
                });
                if let Some(read) = entry.read {
                    value["read"] = read.into();
                }
                if let Some(expected) = entry.expected {
                    value["expected"] = expected.into();
                }
                if let Some(prev_read) = entry.prev_read {
                    value["prev_read"] = prev_read.into();
                }
                if let Some(prev_write_1h) = entry.prev_write_1h {
                    value["prev_write_1h"] = prev_write_1h.into();
                }
                if let Some(thinking) = entry.thinking {
                    value["thinking"] = thinking.into();
                }
                value
            })
            .collect();
        let text = serde_json::to_string_pretty(&json!({"broken": entries}))
            .expect("a JSON value serializes");
        owner_only::write_atomic(&self.path, text.as_bytes())
    }
}

/// A verdict the self-check reached, with what it was judged on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Finding {
    pub version: (u32, u32, u32),
    pub wording: String,
    pub saved: bool,
    pub evidence: Evidence,
}

impl Finding {
    pub(super) fn warning(&self) -> String {
        let (a, b, c) = self.version;
        let wording: String = self.wording.chars().take(8).collect();
        let remembered = if self.saved {
            " and the verdict is remembered for a week"
        } else {
            ""
        };
        format!(
            "the Claude CLI's replayed context was not read back from the prompt cache \
             (read {} of expected {} after {} s); the cache optimization is off for CLI \
             version {a}.{b}.{c} and reminder wording {wording}{remembered} (`cache-bench` \
             measures it)",
            self.evidence.read,
            self.evidence.expected,
            self.evidence.gap.as_secs()
        )
    }
}

/// Records a finished request and runs the self-check against the conversation's previous one.
/// Returns the verdict when this request is the second consecutive one to show the version's
/// replay broken.
pub(super) fn finished(
    sent: Sent,
    usage: Option<CacheUsage>,
    at: Instant,
    verdicts: &Verdicts,
) -> Option<Finding> {
    let mut records = lock_recovering(&RECORDS);
    let (Layout::Native, Some(version), Some(wording), Some(usage)) =
        (sent.layout, sent.version, sent.wording.as_deref(), usage)
    else {
        records.take(&sent.session_id);
        let missing = if sent.layout != Layout::Native {
            "layout"
        } else if sent.version.is_none() {
            "version"
        } else if sent.wording.is_none() {
            "wording"
        } else {
            "usage"
        };
        tracing::debug!(
            session = %sent.session_id,
            outcome = "no_record",
            missing,
            "claude cache self-check"
        );
        return None;
    };
    let previous = records.take(&sent.session_id);
    let now = Record {
        prints: sent.prints,
        blocks: sent.blocks,
        merged_tail: sent.merged_tail,
        unreplayable_tail: sent.unreplayable_tail,
        settings: sent.settings,
        usage,
        thinking: sent.thinking,
        at,
    };
    let outcome = match &previous {
        Some(prev) => check(prev, &now),
        None => Outcome::Skipped(Skip::FirstRequest),
    };
    let diff = match outcome {
        Outcome::Skipped(Skip::NotExtended {
            first_diff,
            prev_len,
            now_len,
        }) => Some((first_diff, prev_len, now_len)),
        _ => None,
    };
    let report = Report {
        session: &sent.session_id,
        version,
        wording,
        now: &now,
        previous: previous.as_ref(),
        diff,
    };
    records.put(sent.session_id.clone(), now.clone());
    drop(records);
    match outcome {
        Outcome::Healthy => {
            verdicts.clear_strike(version, wording);
            report.emit("healthy", "");
            None
        }
        Outcome::Skipped(skip) => {
            report.emit(&format!("skipped:{skip}"), "");
            None
        }
        Outcome::Broken(evidence) => {
            if !verdicts.strike(version, wording) {
                report.emit("broken", "strike");
                return None;
            }
            let mark = verdicts.mark(version, wording, &evidence);
            verdicts.clear_strike(version, wording);
            report.emit("broken", "verdict");
            match mark {
                Mark::Known => None,
                Mark::Saved | Mark::Unsaved => Some(Finding {
                    version,
                    wording: wording.to_string(),
                    saved: mark == Mark::Saved,
                    evidence,
                }),
            }
        }
    }
}

/// What the per-request debug event is built from.
struct Report<'a> {
    session: &'a str,
    version: (u32, u32, u32),
    wording: &'a str,
    now: &'a Record,
    previous: Option<&'a Record>,
    /// First differing index and both lengths, for a pair whose prefix did not extend.
    diff: Option<(usize, usize, usize)>,
}

impl Report<'_> {
    /// Counts, hashes and tokens only: no message content. A module-target event never reaches an
    /// export backend.
    fn emit(&self, outcome: &str, strike: &str) {
        let (a, b, c) = self.version;
        let wording: String = self.wording.chars().take(8).collect();
        let previous_usage = self
            .previous
            .map_or_else(|| "none".to_string(), |p| p.usage.to_string());
        let gap = self
            .previous
            .map(|p| self.now.at.saturating_duration_since(p.at).as_secs());
        let added = self
            .previous
            .and_then(|p| self.now.blocks.get(p.prints.len()..))
            .map(|blocks| blocks.iter().sum::<usize>());
        tracing::debug!(
            session = %self.session,
            cli = %format!("{a}.{b}.{c}"),
            wording = %wording,
            layout = "native",
            frames = self.now.prints.len(),
            added = ?added,
            usage = %self.now.usage,
            previous_usage = %previous_usage,
            gap_secs = ?gap,
            thinking = self.now.thinking,
            outcome,
            strike = strike,
            first_diff = ?self.diff.map(|d| d.0),
            prev_frames = ?self.diff.map(|d| d.1),
            now_frames = ?self.diff.map(|d| d.2),
            "claude cache self-check"
        );
    }
}

#[cfg(test)]
mod tests;
