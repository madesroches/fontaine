use std::time::Duration;

use data_encoding::BASE32_NOPAD;
use iroh::endpoint::{ConnectionError, presets};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, RelayMode};

use crate::protocol::{
    ALPN, MAX_MESSAGE_BYTES, Reason, Request, Response, Status, VERSION, read_frame, write_frame,
};
use crate::server::CLOSED_CODE;
use crate::ticket::Ticket;

pub struct SendOptions {
    pub timeout: Duration,
    /// Overrides the relay mode derived from the ticket; tests point it at a local relay.
    pub relay: Option<RelayMode>,
    /// Retry once with the same message id when the acknowledgement is lost.
    pub retry: bool,
    /// Overrides the relay trust store, for a relay with a self-signed certificate.
    pub ca_tls: Option<CaTlsConfig>,
}

impl Default for SendOptions {
    fn default() -> SendOptions {
        SendOptions {
            timeout: Duration::from_secs(20),
            relay: None,
            retry: true,
            ca_tls: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Accepted {
    pub message_id: String,
}

#[derive(Debug)]
pub enum SendError {
    Rejected(Reason),
    /// The session closed its inbox or went away mid-request.
    Closed,
    Timeout,
    Connect(Box<dyn std::error::Error + Send + Sync>),
    Protocol(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Rejected(reason) => f.write_str(reason.describe()),
            SendError::Closed => f.write_str(Reason::Closed.describe()),
            SendError::Timeout => f.write_str("timed out waiting for the session"),
            SendError::Connect(_) => f.write_str("could not reach the session"),
            SendError::Protocol(what) => write!(f, "unexpected reply from the session: {what}"),
        }
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SendError::Connect(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

/// Sends one message and returns once the session has queued it.
pub async fn send(
    ticket: &Ticket,
    text: &str,
    options: SendOptions,
) -> Result<Accepted, SendError> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(SendError::Rejected(Reason::TooLarge));
    }
    let relay = options.relay.unwrap_or_else(|| {
        if ticket.node.addrs.iter().any(|addr| addr.is_relay()) {
            RelayMode::Default
        } else {
            RelayMode::Disabled
        }
    });
    let mut builder = Endpoint::builder(presets::Minimal).relay_mode(relay);
    if let Some(ca_tls) = options.ca_tls {
        builder = builder.ca_tls_config(ca_tls);
    }
    let endpoint = builder
        .bind()
        .await
        .map_err(|e| SendError::Connect(Box::new(e)))?;
    let request = Request {
        v: VERSION,
        secret: BASE32_NOPAD.encode(&ticket.secret),
        message_id: format!("msg-{}", uuid::Uuid::new_v4()),
        text: text.to_string(),
    };
    let mut result = attempt(&endpoint, ticket, &request, options.timeout).await;
    // One endpoint, so the retry has the same node id the secret may already be bound to.
    if options.retry
        && matches!(
            result,
            Err(SendError::Timeout | SendError::Connect(_) | SendError::Protocol(_))
        )
    {
        result = attempt(&endpoint, ticket, &request, options.timeout).await;
    }
    endpoint.close().await;
    result
}

async fn attempt(
    endpoint: &Endpoint,
    ticket: &Ticket,
    request: &Request,
    timeout: Duration,
) -> Result<Accepted, SendError> {
    tokio::time::timeout(timeout, exchange(endpoint, ticket, request))
        .await
        .map_err(|_| SendError::Timeout)?
}

fn connect_error(error: impl std::error::Error + Send + Sync + 'static) -> SendError {
    SendError::Connect(Box::new(error))
}

fn closed_or(error: ConnectionError) -> SendError {
    match &error {
        ConnectionError::ApplicationClosed(close) if close.error_code == CLOSED_CODE.into() => {
            SendError::Closed
        }
        _ => connect_error(error),
    }
}

async fn exchange(
    endpoint: &Endpoint,
    ticket: &Ticket,
    request: &Request,
) -> Result<Accepted, SendError> {
    let connection = endpoint
        .connect(ticket.node.clone(), ALPN)
        .await
        .map_err(connect_error)?;
    let result = async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(closed_or)?;
        write_frame(&mut send, request)
            .await
            .map_err(connect_error)?;
        send.finish().map_err(connect_error)?;
        let body = read_frame(&mut recv).await.map_err(|e| match e {
            crate::protocol::FrameError::Io(io) => match connection.close_reason() {
                Some(reason) => closed_or(reason),
                None => connect_error(io),
            },
            other => SendError::Protocol(other.to_string()),
        })?;
        let response: Response =
            serde_json::from_slice(&body).map_err(|e| SendError::Protocol(e.to_string()))?;
        match response.status {
            Status::Accepted { message_id } => Ok(Accepted { message_id }),
            Status::Rejected {
                reason: Reason::Closed,
            } => Err(SendError::Closed),
            Status::Rejected { reason } => Err(SendError::Rejected(reason)),
        }
    }
    .await;
    // The close tells the server its response arrived.
    connection.close(0u32.into(), b"done");
    result
}
