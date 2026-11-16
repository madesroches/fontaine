use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const ALPN: &[u8] = b"fontaine/attach/1";
pub const VERSION: u16 = 1;
/// Longest prompt or slash line a device may send, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
pub const MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_MESSAGE_ID_BYTES: usize = 128;
/// Longest deny feedback a device may attach to an approval answer, in bytes.
pub const MAX_FEEDBACK_BYTES: usize = 1024;
pub const MAX_LABEL_CHARS: usize = 64;
/// The largest frame the host sends. A client frame is far smaller: it carries typed input only.
pub const HOST_FRAME_CAP: usize = 4 * 1024 * 1024;
/// Room for an 8 KiB text in its worst JSON escaping (`\u00XX`, six bytes per byte).
pub const CLIENT_FRAME_CAP: usize = 64 * 1024;
/// Bounds the read of a frame's body, and of the whole handshake frame, so a stalled peer cannot
/// hold a task open.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// The authenticated iroh node id of a remote peer, as text.
pub type Sender = String;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    BadTicket,
    Expired,
    Reused,
    Locked,
    TooLarge,
    InboxFull,
    Closed,
    UnsupportedVersion,
    Malformed,
    NotPaired,
    PairingDeclined,
    PairingTimeout,
    Revoked,
    Superseded,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::BadTicket => "bad_ticket",
            Reason::Expired => "expired",
            Reason::Reused => "reused",
            Reason::Locked => "locked",
            Reason::TooLarge => "too_large",
            Reason::InboxFull => "inbox_full",
            Reason::Closed => "closed",
            Reason::UnsupportedVersion => "unsupported_version",
            Reason::Malformed => "malformed",
            Reason::NotPaired => "not_paired",
            Reason::PairingDeclined => "pairing_declined",
            Reason::PairingTimeout => "pairing_timeout",
            Reason::Revoked => "revoked",
            Reason::Superseded => "superseded",
        }
    }

    /// One line for the person running `fontaine attach`.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::BadTicket => "the ticket was not accepted (wrong or revoked)",
            Reason::Expired => "the ticket has expired",
            Reason::Reused => "the ticket was already used; ask for a new one",
            Reason::Locked => "too many bad attempts; try again in 30 seconds",
            Reason::TooLarge => {
                "too large: the message, or the session's transcript to attach to, is over the size limit"
            }
            Reason::InboxFull => "the session's remote queue is full; try again shortly",
            Reason::Closed => "the session turned remote control off",
            Reason::UnsupportedVersion => "the session speaks a different protocol version",
            Reason::Malformed => "the request was malformed",
            Reason::NotPaired => "this device is not paired with the session; attach with a ticket",
            Reason::PairingDeclined => "the person at the session declined the pairing",
            Reason::PairingTimeout => "nobody at the session confirmed the pairing in time",
            Reason::Revoked => "this device was revoked and must pair again",
            Reason::Superseded => "another connection from this device took over",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warning,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Auth {
    Pair {
        secret: String,
    },
    Resume {
        #[serde(default)]
        epoch: Option<u64>,
        #[serde(default)]
        last_seq: Option<u64>,
    },
}

/// What a device picked at an approval: one of the labels the dialog offered, and for a denial an
/// optional reason. Never a raw core answer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireAnswer {
    pub choice: String,
    #[serde(default)]
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        v: u16,
        label: String,
        auth: Auth,
    },
    Prompt {
        message_id: String,
        text: String,
    },
    ApprovalAnswer {
        id: u64,
        answer: WireAnswer,
    },
    QuestionReply {
        id: u64,
        index: usize,
        options: Vec<String>,
        reply: String,
    },
    Interrupt,
    Slash {
        message_id: String,
        line: String,
    },
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    /// One slice of the serialized snapshot; the client joins every part with the same `seq`.
    Snapshot {
        epoch: u64,
        seq: u64,
        part: u32,
        of: u32,
        state: String,
    },
    Event {
        epoch: u64,
        seq: u64,
        event: serde_json::Value,
    },
    Notice {
        severity: Level,
        text: String,
    },
    Accepted {
        message_id: String,
    },
    PromptRejected {
        message_id: String,
        reason: Reason,
    },
    Paired,
    Rejected {
        reason: Reason,
    },
    Ping,
    Bye,
}

#[derive(Debug)]
pub enum FrameError {
    /// The length prefix exceeded the cap; nothing past it was read.
    TooLarge,
    Timeout,
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge => f.write_str("frame exceeds the size cap"),
            FrameError::Timeout => f.write_str("timed out reading a frame"),
            FrameError::Io(_) => f.write_str("reading a frame failed"),
            FrameError::Json(_) => f.write_str("frame is not valid JSON"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrameError::Io(e) => Some(e),
            FrameError::Json(e) => Some(e),
            _ => None,
        }
    }
}

/// What `read_frame` bounds with `FRAME_TIMEOUT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The prefix and the body together: the handshake, where a silent peer is a stalled one.
    Whole,
    /// The body once the 4-byte prefix has arrived. The wait for the prefix is unbounded: a
    /// session stream is idle between messages, and the keepalive judges silence.
    Body,
}

pub fn encode_frame<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("protocol types serialize")
}

pub async fn write_body<W>(writer: &mut W, body: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = u32::try_from(body.len()).expect("frames are far below 4 GiB");
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(body).await
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    write_body(writer, &encode_frame(value)).await
}

/// Reads one length-prefixed frame as raw JSON bytes. A prefix over `cap` fails before the body
/// is read.
pub async fn read_frame<R>(reader: &mut R, cap: usize, wait: Wait) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    match wait {
        Wait::Whole => {
            let whole = async {
                let len = read_prefix(reader, cap).await?;
                read_body(reader, len).await
            };
            tokio::time::timeout(FRAME_TIMEOUT, whole)
                .await
                .map_err(|_| FrameError::Timeout)?
        }
        Wait::Body => {
            let len = read_prefix(reader, cap).await?;
            tokio::time::timeout(FRAME_TIMEOUT, read_body(reader, len))
                .await
                .map_err(|_| FrameError::Timeout)?
        }
    }
}

async fn read_prefix<R>(reader: &mut R, cap: usize) -> Result<usize, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0u8; 4];
    reader
        .read_exact(&mut prefix)
        .await
        .map_err(FrameError::Io)?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > cap {
        return Err(FrameError::TooLarge);
    }
    Ok(len)
}

async fn read_body<R>(reader: &mut R, len: usize) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.map_err(FrameError::Io)?;
    Ok(body)
}

/// Decodes the first client message, telling a wrong protocol version apart from a malformed body.
pub fn parse_hello(body: &[u8]) -> Result<ClientMsg, Reason> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| Reason::Malformed)?;
    match value.get("v").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => {}
        Some(_) => return Err(Reason::UnsupportedVersion),
        None => return Err(Reason::Malformed),
    }
    match serde_json::from_value(value) {
        Ok(msg @ ClientMsg::Hello { .. }) => Ok(msg),
        _ => Err(Reason::Malformed),
    }
}

pub fn parse_client(body: &[u8]) -> Result<ClientMsg, Reason> {
    serde_json::from_slice(body).map_err(|_| Reason::Malformed)
}

/// The JSON text of a snapshot cut into parts whose frames each fit `cap`. The client joins the
/// parts in order and parses the result once all of them with the same `seq` arrived.
pub fn split_snapshot(epoch: u64, seq: u64, state: &str, cap: usize) -> Vec<HostMsg> {
    // The frame around the slice: tag, numbers and quotes, with generous slack.
    let budget = cap.saturating_sub(512).max(16);
    let escaped = |c: char| match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    };
    let mut slices: Vec<&str> = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (at, c) in state.char_indices() {
        let width = escaped(c);
        if used + width > budget {
            slices.push(&state[start..at]);
            start = at;
            used = 0;
        }
        used += width;
    }
    slices.push(&state[start..]);
    let of = u32::try_from(slices.len()).expect("a snapshot has far fewer than 4 G parts");
    slices
        .into_iter()
        .enumerate()
        .map(|(part, slice)| HostMsg::Snapshot {
            epoch,
            seq,
            part: u32::try_from(part).expect("bounded by `of`"),
            of,
            state: slice.to_string(),
        })
        .collect()
}

/// Collects the parts of one snapshot.
#[derive(Default)]
pub struct SnapshotAssembler {
    key: Option<(u64, u64, u32)>,
    parts: std::collections::BTreeMap<u32, String>,
}

impl SnapshotAssembler {
    /// The whole state, `(epoch, seq, json)`, once every part with this part's `seq` is in.
    pub fn push(
        &mut self,
        epoch: u64,
        seq: u64,
        part: u32,
        of: u32,
        state: String,
    ) -> Option<(u64, u64, String)> {
        if of == 0 || part >= of {
            return None;
        }
        if self.key != Some((epoch, seq, of)) {
            self.key = Some((epoch, seq, of));
            self.parts.clear();
        }
        self.parts.insert(part, state);
        if self.parts.len() != of as usize {
            return None;
        }
        self.key = None;
        let joined = std::mem::take(&mut self.parts).into_values().collect();
        Some((epoch, seq, joined))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_distinguishes_version_from_malformed() {
        let ok = br#"{"type":"hello","v":1,"label":"l","auth":{"method":"resume"}}"#;
        assert!(matches!(
            parse_hello(ok),
            Ok(ClientMsg::Hello {
                auth: Auth::Resume {
                    epoch: None,
                    last_seq: None
                },
                ..
            })
        ));
        assert_eq!(
            parse_hello(br#"{"v":2,"anything":true}"#),
            Err(Reason::UnsupportedVersion)
        );
        assert_eq!(parse_hello(br#"{"v":1}"#), Err(Reason::Malformed));
        assert_eq!(parse_hello(b"not json"), Err(Reason::Malformed));
        assert_eq!(
            parse_hello(br#"{"type":"ping","v":1}"#),
            Err(Reason::Malformed)
        );
    }

    #[test]
    fn messages_round_trip_flat() {
        let accepted = HostMsg::Accepted {
            message_id: "m1".into(),
        };
        let json = serde_json::to_string(&accepted).unwrap();
        assert_eq!(json, r#"{"type":"accepted","message_id":"m1"}"#);
        assert_eq!(serde_json::from_str::<HostMsg>(&json).unwrap(), accepted);
        let reply = ClientMsg::QuestionReply {
            id: 3,
            index: 1,
            options: vec!["a".into()],
            reply: "a".into(),
        };
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), reply);
        let pair = ClientMsg::Hello {
            v: VERSION,
            label: "x".into(),
            auth: Auth::Pair { secret: "s".into() },
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), pair);
    }

    #[tokio::test]
    async fn a_length_prefix_over_the_cap_is_too_large_without_reading_the_body() {
        let mut bytes = ((CLIENT_FRAME_CAP as u32) + 1).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"short");
        let mut reader = &bytes[..];
        assert!(matches!(
            read_frame(&mut reader, CLIENT_FRAME_CAP, Wait::Whole).await,
            Err(FrameError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &HostMsg::Rejected {
                reason: Reason::Expired,
            },
        )
        .await
        .unwrap();
        let mut reader = &buffer[..];
        let body = read_frame(&mut reader, HOST_FRAME_CAP, Wait::Body)
            .await
            .unwrap();
        let back: HostMsg = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            back,
            HostMsg::Rejected {
                reason: Reason::Expired
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_body_wait_does_not_bound_the_wait_for_the_prefix() {
        let (mut near, far) = tokio::io::duplex(64);
        let waiting = tokio::spawn(async move {
            let mut far = far;
            read_frame(&mut far, 1024, Wait::Body).await
        });
        // Far past FRAME_TIMEOUT with nothing sent: still waiting.
        tokio::time::sleep(FRAME_TIMEOUT * 5).await;
        assert!(!waiting.is_finished());
        write_frame(&mut near, &Level::Info).await.unwrap();
        assert!(waiting.await.unwrap().is_ok());

        // Once the prefix is in, a body that never arrives times out.
        let (mut near, far) = tokio::io::duplex(64);
        let stalled = tokio::spawn(async move {
            let mut far = far;
            read_frame(&mut far, 1024, Wait::Body).await
        });
        near.write_all(&10u32.to_be_bytes()).await.unwrap();
        tokio::time::sleep(FRAME_TIMEOUT * 2).await;
        assert!(matches!(stalled.await.unwrap(), Err(FrameError::Timeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn the_handshake_wait_covers_the_prefix_too() {
        let (_near, far) = tokio::io::duplex(64);
        let mut far = far;
        let result = read_frame(&mut far, 1024, Wait::Whole).await;
        assert!(matches!(result, Err(FrameError::Timeout)));
    }

    #[test]
    fn a_snapshot_is_cut_to_parts_that_fit_and_joined_back() {
        // Quotes and control characters double or sextuple on the wire.
        let state: String = (0..5000)
            .map(|i| match i % 3 {
                0 => '"',
                1 => '\u{1}',
                _ => 'é',
            })
            .collect();
        let cap = 2048;
        let parts = split_snapshot(7, 42, &state, cap);
        assert!(parts.len() > 1);
        let mut assembler = SnapshotAssembler::default();
        let mut done = None;
        for part in parts {
            assert!(encode_frame(&part).len() <= cap, "a part exceeds the cap");
            let HostMsg::Snapshot {
                epoch,
                seq,
                part,
                of,
                state,
            } = part
            else {
                panic!("a snapshot part");
            };
            done = assembler.push(epoch, seq, part, of, state).or(done);
        }
        assert_eq!(done, Some((7, 42, state)));
    }

    #[test]
    fn parts_of_another_snapshot_restart_the_assembly() {
        let mut assembler = SnapshotAssembler::default();
        assert_eq!(assembler.push(1, 1, 0, 2, "a".into()), None);
        assert_eq!(assembler.push(1, 2, 0, 2, "x".into()), None);
        assert_eq!(
            assembler.push(1, 2, 1, 2, "y".into()),
            Some((1, 2, "xy".into()))
        );
    }
}
