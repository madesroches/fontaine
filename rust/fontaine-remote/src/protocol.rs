use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const ALPN: &[u8] = b"fontaine/send/1";
pub const VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Room for the 8 KiB text in its worst JSON escaping (`\u00XX`, six bytes per byte).
pub const MAX_FRAME: usize = 64 * 1024;
pub const MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_MESSAGE_ID_BYTES: usize = 128;
/// Bounds every read of a peer's frame so a stalled peer cannot hold a task open.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// The authenticated iroh node id of a remote peer, as text.
pub type Sender = String;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub v: u16,
    pub secret: String,
    pub message_id: String,
    pub text: String,
}

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
        }
    }

    /// One line for the person running `fontaine send`.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::BadTicket => "the ticket was not accepted (wrong or revoked)",
            Reason::Expired => "the ticket has expired",
            Reason::Reused => "the ticket was already used; ask for a new one",
            Reason::Locked => "too many bad attempts; try again in a minute",
            Reason::TooLarge => "the message is too large",
            Reason::InboxFull => "the session's remote inbox is full; try again shortly",
            Reason::Closed => "the session closed its remote inbox",
            Reason::UnsupportedVersion => "the session speaks a different protocol version",
            Reason::Malformed => "the request was malformed",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    Accepted { message_id: String },
    Rejected { reason: Reason },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub v: u16,
    #[serde(flatten)]
    pub status: Status,
}

impl Response {
    pub fn accepted(message_id: String) -> Response {
        Response {
            v: VERSION,
            status: Status::Accepted { message_id },
        }
    }

    pub fn rejected(reason: Reason) -> Response {
        Response {
            v: VERSION,
            status: Status::Rejected { reason },
        }
    }
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

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(value).expect("protocol types serialize");
    let len = u32::try_from(body.len()).expect("frames are far below 4 GiB");
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await
}

/// Reads one length-prefixed frame as raw JSON bytes, under `FRAME_TIMEOUT`.
pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let read = async {
        let mut prefix = [0u8; 4];
        reader
            .read_exact(&mut prefix)
            .await
            .map_err(FrameError::Io)?;
        let len = u32::from_be_bytes(prefix) as usize;
        if len > MAX_FRAME {
            return Err(FrameError::TooLarge);
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).await.map_err(FrameError::Io)?;
        Ok(body)
    };
    tokio::time::timeout(FRAME_TIMEOUT, read)
        .await
        .map_err(|_| FrameError::Timeout)?
}

/// Decodes a request, telling a wrong protocol version apart from a malformed body.
pub fn parse_request(body: &[u8]) -> Result<Request, Reason> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| Reason::Malformed)?;
    match value.get("v").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => {}
        Some(_) => return Err(Reason::UnsupportedVersion),
        None => return Err(Reason::Malformed),
    }
    serde_json::from_value(value).map_err(|_| Reason::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_round_trips_flat() {
        let accepted = Response::accepted("m1".into());
        let json = serde_json::to_string(&accepted).unwrap();
        assert_eq!(json, r#"{"v":1,"status":"accepted","message_id":"m1"}"#);
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), accepted);
        let rejected = Response::rejected(Reason::InboxFull);
        let json = serde_json::to_string(&rejected).unwrap();
        assert_eq!(json, r#"{"v":1,"status":"rejected","reason":"inbox_full"}"#);
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), rejected);
    }

    #[test]
    fn a_request_distinguishes_version_from_malformed() {
        let ok = br#"{"v":1,"secret":"s","message_id":"m","text":"t"}"#;
        assert!(parse_request(ok).is_ok());
        assert_eq!(
            parse_request(br#"{"v":2,"anything":true}"#),
            Err(Reason::UnsupportedVersion)
        );
        assert_eq!(parse_request(br#"{"v":1}"#), Err(Reason::Malformed));
        assert_eq!(parse_request(b"not json"), Err(Reason::Malformed));
        assert_eq!(parse_request(br#"{"secret":"x"}"#), Err(Reason::Malformed));
    }

    #[tokio::test]
    async fn a_length_prefix_over_the_cap_is_too_large_without_reading_the_body() {
        let mut bytes = ((MAX_FRAME as u32) + 1).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"short");
        let mut reader = &bytes[..];
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(FrameError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &Response::rejected(Reason::Expired))
            .await
            .unwrap();
        let mut reader = &buffer[..];
        let body = read_frame(&mut reader).await.unwrap();
        let back: Response = serde_json::from_slice(&body).unwrap();
        assert_eq!(back, Response::rejected(Reason::Expired));
    }
}
