//! The two cache layouts a request can take, and the runtime check that falls back from the
//! native one.
//!
//! The CLI adds a `<system-reminder>` carrying the account's context to the message that asks
//! for a response, so the next request replays that message without it and rewrites it. The
//! native layout replays the reminder exactly where the CLI put it on every replayed user
//! message and puts the adapter's cache marker on the final message, so each replayed message is
//! byte-identical to the request that last wrote it. The legacy layout puts the marker on the
//! last replayed user message instead, before the point where the CLI's addition changes the
//! bytes.

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
use crate::core::file_lock;
use crate::core::owner_only;
use crate::core::providers::warn::Warn;

/// The CLI version the reminder template was taken from.
const TEMPLATE_SINCE: (u32, u32, u32) = (2, 1, 319);

/// Identifies the template and where `apply` places it. Any edit to either bumps it, so a
/// verdict recorded against the old one stops applying.
const TEMPLATE_REVISION: u32 = 1;

const REMINDER_TEMPLATE: &str = "<system-reminder>\n\
As you answer the user's questions, you can use the following context:\n\
# userEmail\n\
The user's email address is {email}. Use it only to identify the user, such as for authorship, attribution, or filtering their own work. Never send it to an unrelated service, such as in a request header, URL, or payload, unless the user explicitly asks. A site asking automated clients for contact details, such as an email in the User-Agent, is not a reason to use this address: use a generic client name or ask the user.\n\
\n\
Claude Code attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back.\n\
</system-reminder>";

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

/// The layout for a request whose process reported `context`.
pub(super) fn layout(context: &CliContext, verdicts: &Verdicts) -> Layout {
    let native = context.email.is_some()
        && context
            .version
            .is_some_and(|version| version >= TEMPLATE_SINCE && !verdicts.contains(version));
    if native {
        Layout::Native
    } else {
        Layout::Legacy
    }
}

/// Replays the CLI's reminder and places the cache marker for `layout`. Run after the session id
/// is derived so neither changes it.
pub(super) fn apply(frames: &mut [Frame], context: &CliContext, layout: Layout) {
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
            let Some(email) = context.email.as_deref() else {
                return;
            };
            let reminder = REMINDER_TEMPLATE.replace("{email}", email);
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CacheUsage {
    pub read: u64,
    pub write_1h: u64,
}

impl CacheUsage {
    /// Absent when the result carries no cache read count.
    pub(super) fn from_result(usage: &Value) -> Option<Self> {
        Some(CacheUsage {
            read: usage.get("cache_read_input_tokens")?.as_u64()?,
            write_1h: usage
                .get("cache_creation")
                .and_then(|creation| creation.get("ephemeral_1h_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
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
    at: Instant,
}

/// A request as it was sent, kept until its result arrives.
#[derive(Debug)]
pub(super) struct Sent {
    session_id: String,
    version: Option<(u32, u32, u32)>,
    layout: Layout,
    prints: Vec<u64>,
    blocks: Vec<usize>,
    merged_tail: bool,
    unreplayable_tail: bool,
    settings: u64,
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
        Sent {
            session_id: session_id.to_string(),
            version,
            layout,
            prints,
            blocks,
            merged_tail,
            unreplayable_tail,
            settings: 0,
        }
    }

    pub(super) fn with_settings(mut self, settings: u64) -> Self {
        self.settings = settings;
        self
    }
}

/// What a broken replay was judged on, kept so the verdict can be audited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Evidence {
    pub read: u64,
    pub expected: u64,
    pub gap: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Broken(Evidence),
    Healthy,
    NoVerdict,
}

/// Whether `now` shows a replay the cache did not read back. Anything that makes the comparison
/// unsound gives no verdict.
fn check(prev: &Record, now: &Record) -> Outcome {
    let extends =
        now.prints.len() > prev.prints.len() && now.prints[..prev.prints.len()] == prev.prints[..];
    if !extends || prev.merged_tail || prev.unreplayable_tail || prev.settings != now.settings {
        return Outcome::NoVerdict;
    }
    // A request that read nothing was a cold start: its write is not evidence the prefix replays.
    if prev.usage.read == 0 {
        return Outcome::NoVerdict;
    }
    let gap = now.at.saturating_duration_since(prev.at);
    if gap >= MAX_GAP {
        return Outcome::NoVerdict;
    }
    let added: usize = now.blocks[prev.prints.len()..].iter().sum();
    if added > MAX_ADDED_BLOCKS {
        return Outcome::NoVerdict;
    }
    let expected = prev.usage.read + prev.usage.write_1h;
    if now.usage.read + READ_SLACK < expected {
        Outcome::Broken(Evidence {
            read: now.usage.read,
            expected,
            gap,
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
    broken: Mutex<BTreeMap<(u32, u32, u32), Entry>>,
    // One shortfall is not yet a verdict; kept in memory only, across conversations.
    struck: Mutex<BTreeSet<(u32, u32, u32)>>,
}

static STORES: LazyLock<Mutex<HashMap<PathBuf, Arc<Verdicts>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(|part| part.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The unexpired broken versions a verdicts file holds for the current template revision. An
/// entry without a timestamp predates expiry and counts as expired.
fn parse(text: &str, now: u64) -> Result<BTreeMap<(u32, u32, u32), Entry>, serde_json::Error> {
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
            let parsed = Entry {
                at: entry.get("at")?.as_u64()?,
                read: entry.get("read").and_then(Value::as_u64),
                expected: entry.get("expected").and_then(Value::as_u64),
            };
            parsed.fresh(now).then_some((version, parsed))
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
    pub(super) fn contains(&self, version: (u32, u32, u32)) -> bool {
        // Held only to take a string; a poisoned lock costs at most a warning.
        let held = lock_recovering(&self.load_warning).take();
        if let Some(message) = held {
            self.warn.emit(&message);
        }
        lock_recovering(&self.broken)
            .get(&version)
            .is_some_and(|entry| entry.fresh(unix_now()))
    }

    /// Records a shortfall for `version`; true when the previous observation was one too, which
    /// is what makes a verdict.
    pub(super) fn strike(&self, version: (u32, u32, u32)) -> bool {
        // Advisory state: a poisoned lock costs at most a missed or early verdict.
        !lock_recovering(&self.struck).insert(version)
    }

    /// A healthy pair shows the version replays, so an earlier shortfall no longer counts.
    pub(super) fn clear_strike(&self, version: (u32, u32, u32)) {
        lock_recovering(&self.struck).remove(&version);
    }

    /// Adds a version to the broken set and persists it. A failed write is warned about and
    /// leaves the in-memory verdict in place.
    pub(super) fn mark(&self, version: (u32, u32, u32), evidence: &Evidence) -> Mark {
        let now = unix_now();
        let entry = Entry {
            at: now,
            read: Some(evidence.read),
            expected: Some(evidence.expected),
        };
        {
            let mut broken = lock_recovering(&self.broken);
            if broken.get(&version).is_some_and(|known| known.fresh(now)) {
                return Mark::Known;
            }
            broken.insert(version, entry);
        }
        match self.persist(version, entry, now) {
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

    fn persist(&self, version: (u32, u32, u32), entry: Entry, now: u64) -> std::io::Result<()> {
        let _guard = file_lock::lock(&self.path)?;
        // Merge what another process wrote since the load. A plain read: `read_shared` would wait
        // on the lock held here. An unreadable file is replaced by the write below, and expired
        // entries are not carried over.
        let mut broken = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| parse(&text, now).ok())
            .unwrap_or_default();
        broken.insert(version, entry);
        let entries: Vec<Value> = broken
            .iter()
            .map(|((a, b, c), entry)| {
                let mut value = json!({
                    "cli": format!("{a}.{b}.{c}"),
                    "template": TEMPLATE_REVISION,
                    "at": entry.at,
                });
                if let Some(read) = entry.read {
                    value["read"] = read.into();
                }
                if let Some(expected) = entry.expected {
                    value["expected"] = expected.into();
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Finding {
    pub version: (u32, u32, u32),
    pub saved: bool,
    pub evidence: Evidence,
}

impl Finding {
    pub(super) fn warning(&self) -> String {
        let (a, b, c) = self.version;
        let remembered = if self.saved {
            " and the verdict is remembered for a week"
        } else {
            ""
        };
        format!(
            "the Claude CLI's replayed context was not read back from the prompt cache \
             (read {} of expected {} after {} s); the cache optimization is off for CLI \
             version {a}.{b}.{c}{remembered} (`cache-bench` measures it)",
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
    let (Layout::Native, Some(version), Some(usage)) = (sent.layout, sent.version, usage) else {
        records.take(&sent.session_id);
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
        at,
    };
    let outcome = previous.map_or(Outcome::NoVerdict, |prev| check(&prev, &now));
    records.put(sent.session_id, now);
    drop(records);
    let evidence = match outcome {
        Outcome::Healthy => {
            verdicts.clear_strike(version);
            return None;
        }
        Outcome::NoVerdict => return None,
        Outcome::Broken(evidence) => evidence,
    };
    if !verdicts.strike(version) {
        return None;
    }
    let mark = verdicts.mark(version, &evidence);
    verdicts.clear_strike(version);
    let saved = match mark {
        Mark::Known => return None,
        Mark::Saved => true,
        Mark::Unsaved => false,
    };
    Some(Finding {
        version,
        saved,
        evidence,
    })
}

#[cfg(test)]
mod tests;
