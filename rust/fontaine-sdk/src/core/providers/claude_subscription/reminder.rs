//! Learns the `<system-reminder>` the CLI puts on the message that asks for a response.
//!
//! Replaying the reminder byte for byte is what lets the prompt cache read the history back, and
//! the CLI's wording changes between releases. The stream-json output never carries it, so a
//! probe launch points the CLI at a throwaway listener on the loopback interface and reads the
//! reminder out of the request the CLI makes there. The listener answers with a canned stream and
//! never forwards anything, so the probe costs one process start and no model call.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::cli::{ChildFiles, Cli, Scratch, fixed_args};
use crate::core::checkpoint::hex;
use crate::tools::lock::lock;

/// A request body larger than this is not the CLI's probe turn.
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;
/// How long after a probe that could not tell, the next request tries again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

const OPEN: &str = "<system-reminder>\n";
const CLOSE: &str = "</system-reminder>";
const EMAIL_SECTION: &str = "# userEmail";
const PLACEHOLDER: &str = "{email}";

/// The reminder as the CLI sent it, with the account's email replaced by a placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Reminder {
    pub template: String,
    /// The lowercase hex SHA-256 of `template`. Persisted, so it must not depend on the Rust
    /// release the way `DefaultHasher` does.
    pub wording: String,
}

impl Reminder {
    pub(super) fn with_email(&self, email: &str) -> String {
        self.template.replace(PLACEHOLDER, email)
    }
}

/// What one probe found out.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Learned(Reminder),
    /// The CLI's request is understood but cannot be replayed from one capture (a section that
    /// depends on the directory or environment, or a placement the adapter does not reproduce).
    /// The same CLI would say the same again.
    Unusable,
    /// The probe could not tell: the CLI did not start, did not ask in time, or exited first.
    Transient,
}

/// The reminder in the first message of the CLI's request `body`, if the request has the shape
/// the replay assumes: a user message of exactly the reminder block and then the probe's own text.
pub(super) fn parse_reminder(body: &Value, email: &str, nonce: &str) -> Option<Reminder> {
    let first = body.get("messages")?.as_array()?.first()?;
    if first.get("role")?.as_str()? != "user" {
        return None;
    }
    let blocks = first.get("content")?.as_array()?;
    let [lead, prompt] = blocks.as_slice() else {
        return None;
    };
    if text_of(prompt)? != nonce {
        return None;
    }
    let reminder = text_of(lead)?.strip_suffix('\n')?;
    let inner = reminder.strip_prefix(OPEN)?.strip_suffix(CLOSE)?;
    if inner.contains(OPEN.trim_end()) || inner.contains(CLOSE) || inner.contains(PLACEHOLDER) {
        return None;
    }
    if email.is_empty() || reminder.matches(email).count() != 1 {
        return None;
    }
    let mut sections = inner.lines().filter(|line| line.starts_with("# "));
    if sections.next() != Some(EMAIL_SECTION) || sections.next().is_some() {
        return None;
    }
    let template = reminder.replace(email, PLACEHOLDER);
    let wording = hex(&Sha256::digest(template.as_bytes()));
    Some(Reminder { template, wording })
}

fn text_of(block: &Value) -> Option<&str> {
    if block.get("type")?.as_str()? != "text" {
        return None;
    }
    block.get("text")?.as_str()
}

/// The text a request's last user message ends with.
fn last_user_text(body: &Value) -> Option<&str> {
    let last = body
        .get("messages")?
        .as_array()?
        .iter()
        .rfind(|message| message.get("role").and_then(Value::as_str) == Some("user"))?;
    match last.get("content")? {
        Value::String(text) => Some(text),
        Value::Array(blocks) => blocks.iter().rev().find_map(text_of),
        _ => None,
    }
}

/// The canned reply: a complete one-turn stream, so the CLI finishes and exits.
fn canned_stream() -> String {
    let events = [
        json!({"type": "message_start", "message": {
            "id": "msg_probe", "type": "message", "role": "assistant", "model": "probe",
            "content": [], "stop_reason": null,
            "usage": {"input_tokens": 1, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "ok"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ];
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// What the listener answers to and where it hands the probe's request.
struct Route {
    path: String,
    nonce: String,
    found: Mutex<Option<oneshot::Sender<Value>>>,
}

impl Route {
    /// Hands the probe's request to the waiting probe: the first POST to exactly this route's
    /// path whose last user text is the probe's nonce. True for such a request, which gets the
    /// canned stream; anything else gets a 404.
    fn offer(&self, method: &str, target: &str, body: &[u8]) -> bool {
        let path = target.split_once('?').map_or(target, |(path, _)| path);
        if method != "POST" || path != self.path {
            return false;
        }
        let Ok(body) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        if last_user_text(&body) != Some(self.nonce.as_str()) {
            return false;
        }
        if let Some(sender) = lock(&self.found, "reminder probe hand-off").take() {
            // The probe may have ended already; nothing waits then.
            let _ = sender.send(body);
        }
        true
    }
}

struct Head {
    method: String,
    target: String,
    length: usize,
}

/// The request line and the content length. Every other header, the bearer token among them, is
/// read past and dropped.
fn parse_head(head: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next()?.split(' ');
    let method = request.next()?.to_string();
    let target = request.next()?.to_string();
    let mut length = 0;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    Some(Head {
        method,
        target,
        length,
    })
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

async fn connection(mut stream: TcpStream, route: Arc<Route>) {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let head_end = loop {
            if let Some(end) = find_head_end(&buffer) {
                break end;
            }
            if buffer.len() > MAX_HEAD
                || !matches!(stream.read_buf(&mut buffer).await, Ok(n) if n > 0)
            {
                return;
            }
        };
        let Some(head) = parse_head(&buffer[..head_end]) else {
            return;
        };
        if head.length > MAX_BODY {
            let _ = stream.write_all(NOT_FOUND_CLOSING).await;
            return;
        }
        while buffer.len() < head_end + head.length {
            if !matches!(stream.read_buf(&mut buffer).await, Ok(n) if n > 0) {
                return;
            }
        }
        let body: Vec<u8> = buffer[head_end..head_end + head.length].to_vec();
        buffer.drain(..head_end + head.length);
        let reply = if route.offer(&head.method, &head.target, &body) {
            let stream_body = canned_stream();
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{stream_body}",
                stream_body.len()
            )
        } else {
            String::from_utf8_lossy(NOT_FOUND).into_owned()
        };
        if stream.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

const NOT_FOUND: &[u8] = b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n";
const NOT_FOUND_CLOSING: &[u8] =
    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

/// Accepts connections until dropped; dropping it ends every connection it spawned.
async fn serve(listener: TcpListener, route: Arc<Route>) {
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        match listener.accept().await {
            Ok((stream, _)) => {
                connections.spawn(connection(stream, route.clone()));
            }
            // A transient accept failure (descriptor exhaustion) must not spin.
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// What a probe learned, memoized per CLI version and account.
type Key = ((u32, u32, u32), String);

enum Slot {
    Empty,
    Done(Option<Reminder>),
    Failed(Instant),
}

/// The probe results of one data dir, shared by every `Cli` over it, so a burst of requests (a
/// parent and its subagents) starts one probe.
pub(super) struct Reminders {
    slots: Mutex<HashMap<Key, Arc<tokio::sync::Mutex<Slot>>>>,
    warned: AtomicBool,
}

static MEMOS: LazyLock<Mutex<HashMap<PathBuf, Arc<Reminders>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Account emails seen in a listing, per data dir. Process memory only: the plaintext address is
/// never written to disk, so it can be scrubbed from drafts without being stored.
static EMAILS: LazyLock<Mutex<HashMap<PathBuf, BTreeSet<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn record_email(data_dir: &Path, email: &str) {
    lock(&EMAILS, "claude account emails")
        .entry(data_dir.to_path_buf())
        .or_default()
        .insert(email.to_string());
}

/// Every account email a listing has returned in this process for `data_dir`; several accounts
/// can share one data dir.
pub fn known_account_emails(data_dir: &Path) -> Vec<String> {
    // Connect keys the memos by the absolute path, so the lookup must too.
    let data_dir = std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    lock(&EMAILS, "claude account emails")
        .get(&data_dir)
        .map(|emails| emails.iter().cloned().collect())
        .unwrap_or_default()
}

fn key(version: (u32, u32, u32), email: &str) -> Key {
    (version, hex(&Sha256::digest(email.as_bytes())))
}

impl Reminders {
    pub(super) fn shared(data_dir: &Path) -> Arc<Reminders> {
        lock(&MEMOS, "claude reminder memos")
            .entry(data_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Reminders::new()))
            .clone()
    }

    pub(super) fn new() -> Reminders {
        Reminders {
            slots: Mutex::new(HashMap::new()),
            warned: AtomicBool::new(false),
        }
    }

    /// The reminder for `version` and `email`, running `probe` unless one already answered. A
    /// reminder or an unusable shape is kept for the process; a probe that could not tell is
    /// tried again no sooner than five minutes later, and the request in hand gets `None`.
    pub(super) async fn get_or_learn<'a>(
        &self,
        version: (u32, u32, u32),
        email: &str,
        probe: impl FnOnce() -> BoxFuture<'a, Outcome>,
    ) -> Option<Reminder> {
        let slot = lock(&self.slots, "claude reminder slots")
            .entry(key(version, email))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(Slot::Empty)))
            .clone();
        let mut slot = slot.lock().await;
        match &*slot {
            Slot::Done(learned) => return learned.clone(),
            Slot::Failed(at) if at.elapsed() < RETRY_AFTER => return None,
            Slot::Failed(_) | Slot::Empty => {}
        }
        match probe().await {
            Outcome::Learned(reminder) => {
                *slot = Slot::Done(Some(reminder.clone()));
                Some(reminder)
            }
            Outcome::Unusable => {
                *slot = Slot::Done(None);
                None
            }
            Outcome::Transient => {
                *slot = Slot::Failed(Instant::now());
                None
            }
        }
    }

    /// True the first time only: a probe that failed is worth one warning, not one per request.
    fn first_failure(&self) -> bool {
        !self.warned.swap(true, Ordering::Relaxed)
    }
}

impl Cli {
    /// The reminder this CLI version sends for this account, learned from the CLI itself at the
    /// first request and remembered for the process. `None` means the earlier cache layout.
    pub(super) async fn learn_reminder(
        &self,
        email: &str,
        version: (u32, u32, u32),
    ) -> Option<Reminder> {
        self.reminders()
            .get_or_learn(version, email, || {
                Box::pin(async move {
                    let outcome = self.capture(email).await;
                    let failure = match &outcome {
                        Outcome::Learned(_) => None,
                        Outcome::Unusable => Some(
                            "the Claude CLI's context reminder has a shape that cannot be replayed",
                        ),
                        Outcome::Transient => {
                            Some("the Claude CLI's context reminder could not be read")
                        }
                    };
                    if let Some(reason) = failure
                        && self.reminders().first_failure()
                    {
                        self.warn().emit(&format!(
                            "{reason}; the earlier cache layout is used for it"
                        ));
                    }
                    outcome
                })
            })
            .await
    }

    /// One probe launch against a loopback listener.
    async fn capture(&self, email: &str) -> Outcome {
        let Ok(listener) = TcpListener::bind("127.0.0.1:0").await else {
            return Outcome::Transient;
        };
        let Ok(address) = listener.local_addr() else {
            return Outcome::Transient;
        };
        let path = format!("/{}", uuid::Uuid::new_v4().simple());
        let nonce = uuid::Uuid::new_v4().to_string();
        let (sender, mut found) = oneshot::channel();
        let route = Arc::new(Route {
            path: format!("{path}/v1/messages"),
            nonce: nonce.clone(),
            found: Mutex::new(Some(sender)),
        });
        let server = serve(listener, route);
        tokio::pin!(server);

        let Ok(scratch) = Scratch::new(self.warn().clone()) else {
            return Outcome::Transient;
        };
        let Ok(files) = ChildFiles::empty(&scratch) else {
            return Outcome::Transient;
        };
        let args = fixed_args(&files, None, None, None);
        let base_url = format!("http://{address}{path}");
        let Ok(mut child) = self.launch_with(scratch, args, true, Some(&base_url)) else {
            return Outcome::Transient;
        };
        let idle = self.timeouts.probe;
        let frame = json!({"type": "user", "message": {"role": "user", "content": nonce}});
        if child.write_line(&frame, idle).await.is_err() {
            return Outcome::Transient;
        }
        child.close_stdin();

        let drained = async { while matches!(child.next_line(idle).await, Ok(Some(_))) {} };
        let body = tokio::select! {
            body = &mut found => body.ok(),
            () = drained => found.try_recv().ok(),
            () = tokio::time::sleep(idle) => None,
            () = &mut server => None,
        };
        child.stop().await;
        match body {
            None => Outcome::Transient,
            Some(body) => {
                parse_reminder(&body, email, &nonce).map_or(Outcome::Unusable, Outcome::Learned)
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
