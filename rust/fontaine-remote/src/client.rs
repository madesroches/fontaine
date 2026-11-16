//! The attaching side: connects to a host with a ticket or by a stored node id, pairs when it has
//! a ticket, and keeps the connection up with `Resume` while the caller reads events and sends
//! input. It runs no session and loads no credentials.

use std::sync::Arc;
use std::time::Duration;

use data_encoding::BASE32_NOPAD;
use iroh::address_lookup::{DnsAddressLookup, PkarrResolver};
use iroh::endpoint::{ConnectionError, RecvStream, SendStream, presets};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::paired::{ClientStore, StoredHost};
use crate::protocol::{
    Auth, ClientMsg, HOST_FRAME_CAP, HostMsg, Level, MAX_MESSAGE_BYTES, Reason, SnapshotAssembler,
    VERSION, Wait, read_frame, write_frame,
};
use crate::server::{IDLE_LIMIT, KEEPALIVE};
use crate::ticket::Ticket;

const FIRST_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
/// How long a connection attempt may take to reach the host and have its stream accepted.
const CONNECT_LIMIT: Duration = Duration::from_secs(20);
/// The host gives its person this long to confirm a pairing, plus the time to get there.
const PAIRING_WINDOW: Duration = Duration::from_secs(75);
const MESSAGE_WAIT: Duration = Duration::from_secs(20);

pub enum Target {
    /// A first connection: the ticket carries the address and the pairing secret.
    Ticket(Box<Ticket>),
    /// A paired host, found by its stored addresses and, with lookup on, by its node id.
    Host(Box<StoredHost>),
}

pub struct AttachOptions {
    /// What this device calls itself; the person at the host reads it when confirming.
    pub label: String,
    /// What the host is called in `attach_hosts.json`; its short node id when `None`.
    pub host_label: Option<String>,
    /// `None` takes the ticket's own relay or lack of one, and disables relays for a stored host.
    pub relay: Option<RelayMode>,
    /// Overrides the relay trust store, for a relay with a self-signed certificate.
    pub ca_tls: Option<CaTlsConfig>,
    /// Reconnect with backoff when the connection is lost.
    pub reconnect: bool,
}

impl AttachOptions {
    pub fn new(label: impl Into<String>) -> AttachOptions {
        AttachOptions {
            label: label.into(),
            host_label: None,
            relay: None,
            ca_tls: None,
            reconnect: true,
        }
    }
}

#[derive(Debug)]
pub enum AttachError {
    Rejected(Reason),
    /// The connection ended and was not to be retried.
    Closed,
    Timeout,
    Connect(Box<dyn std::error::Error + Send + Sync>),
    Protocol(String),
    /// A key or host file could not be read or written.
    Store(std::io::Error),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachError::Rejected(reason) => f.write_str(reason.describe()),
            AttachError::Closed => f.write_str(Reason::Closed.describe()),
            AttachError::Timeout => f.write_str("timed out waiting for the session"),
            AttachError::Connect(_) => f.write_str("could not reach the session"),
            AttachError::Protocol(what) => write!(f, "unexpected reply from the session: {what}"),
            AttachError::Store(_) => f.write_str("could not read or write this device's files"),
        }
    }
}

impl std::error::Error for AttachError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AttachError::Connect(e) => Some(e.as_ref()),
            AttachError::Store(e) => Some(e),
            _ => None,
        }
    }
}

fn connect_error(error: impl std::error::Error + Send + Sync + 'static) -> AttachError {
    AttachError::Connect(Box::new(error))
}

/// What the caller's loop sees.
#[derive(Debug)]
pub enum ClientEvent {
    /// The host confirmed the pairing and this device is stored.
    Paired,
    /// The connection is up, whether or not anything was replayed.
    Connected,
    /// The whole session state as JSON; it replaces what the caller has.
    Snapshot {
        epoch: u64,
        seq: u64,
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
    /// The connection dropped; a retry follows after this many milliseconds.
    Reconnecting {
        after_ms: u64,
    },
    /// The host said goodbye; the session closed remote control or ended.
    Bye,
    /// The attach is over for good.
    Failed(AttachError),
}

pub struct Client {
    pub events: mpsc::UnboundedReceiver<ClientEvent>,
    send: mpsc::UnboundedSender<ClientMsg>,
    task: JoinHandle<()>,
}

impl Client {
    /// Starts connecting. Input sent before the connection is up waits for it.
    pub fn start(target: Target, options: AttachOptions, store: Arc<dyn ClientStore>) -> Client {
        let (event_tx, events) = mpsc::unbounded_channel();
        let (send, input) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let failure = attach(target, options, store, &event_tx, input).await;
            if let Err(error) = failure {
                let _ = event_tx.send(ClientEvent::Failed(error));
            }
        });
        Client { events, send, task }
    }

    /// Queues a message for the host. `Hello` and `Ping` are the client's own.
    pub fn send(&self, message: ClientMsg) {
        if !matches!(message, ClientMsg::Hello { .. } | ClientMsg::Ping) {
            let _ = self.send.send(message);
        }
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<ClientMsg> {
        self.send.clone()
    }

    /// Closes the connection and stops reconnecting.
    pub fn stop(self) {
        self.task.abort();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn build_endpoint(
    store: &dyn ClientStore,
    relay: RelayMode,
    ca_tls: Option<CaTlsConfig>,
) -> Result<Endpoint, AttachError> {
    let key = SecretKey::from_bytes(&store.device_key().map_err(AttachError::Store)?);
    // Resolvers only: the client never publishes its own node id or home relay.
    let lookup = matches!(relay, RelayMode::Default);
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(relay)
        .secret_key(key);
    if lookup {
        builder = builder
            .address_lookup(PkarrResolver::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns());
    }
    if let Some(ca_tls) = ca_tls {
        builder = builder.ca_tls_config(ca_tls);
    }
    builder.bind().await.map_err(connect_error)
}

struct Link {
    send: SendStream,
    recv: RecvStream,
    connection: iroh::endpoint::Connection,
}

async fn open_link(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    hello: &ClientMsg,
) -> Result<Link, AttachError> {
    let opened = async {
        let connection = endpoint
            .connect(addr, crate::protocol::ALPN)
            .await
            .map_err(connect_error)?;
        let (mut send, recv) = connection.open_bi().await.map_err(connect_error)?;
        write_frame(&mut send, hello).await.map_err(connect_error)?;
        Ok(Link {
            send,
            recv,
            connection,
        })
    };
    tokio::time::timeout(CONNECT_LIMIT, opened)
        .await
        .map_err(|_| AttachError::Timeout)?
}

enum Frame {
    Host(HostMsg),
    Lost(AttachError),
}

async fn next_frame(link: &mut Link) -> Frame {
    match read_frame(&mut link.recv, HOST_FRAME_CAP, Wait::Body).await {
        Ok(body) => match serde_json::from_slice::<HostMsg>(&body) {
            Ok(message) => Frame::Host(message),
            Err(e) => Frame::Lost(AttachError::Protocol(e.to_string())),
        },
        Err(crate::protocol::FrameError::Io(io)) => {
            Frame::Lost(match link.connection.close_reason() {
                Some(ConnectionError::ApplicationClosed(_)) | None => connect_error(io),
                Some(other) => connect_error(other),
            })
        }
        Err(other) => Frame::Lost(AttachError::Protocol(other.to_string())),
    }
}

/// How one connection ended.
enum Ended {
    /// Worth retrying.
    Lost(AttachError),
    /// The host said it is done, or refused for good.
    Over(Result<(), AttachError>),
}

struct Progress {
    epoch: Option<u64>,
    last_seq: Option<u64>,
    assembler: SnapshotAssembler,
}

async fn pump(
    mut link: Link,
    events: &mpsc::UnboundedSender<ClientEvent>,
    input: &mut mpsc::UnboundedReceiver<ClientMsg>,
    progress: &mut Progress,
) -> Ended {
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.reset();
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            frame = next_frame(&mut link) => {
                last_heard = Instant::now();
                match frame {
                    Frame::Lost(error) => return Ended::Lost(error),
                    Frame::Host(message) => {
                        if let Some(ended) = apply(message, events, progress) {
                            return ended;
                        }
                    }
                }
            }
            message = input.recv() => {
                let Some(message) = message else {
                    return Ended::Over(Ok(()));
                };
                if write_frame(&mut link.send, &message).await.is_err() {
                    // The message is lost with the connection; the caller sees the reconnect.
                    return Ended::Lost(AttachError::Closed);
                }
            }
            _ = keepalive.tick() => {
                if last_heard.elapsed() >= IDLE_LIMIT {
                    return Ended::Lost(AttachError::Timeout);
                }
                if write_frame(&mut link.send, &ClientMsg::Ping).await.is_err() {
                    return Ended::Lost(AttachError::Closed);
                }
            }
        }
    }
}

fn apply(
    message: HostMsg,
    events: &mpsc::UnboundedSender<ClientEvent>,
    progress: &mut Progress,
) -> Option<Ended> {
    match message {
        HostMsg::Ping | HostMsg::Paired => {}
        HostMsg::Snapshot {
            epoch,
            seq,
            part,
            of,
            state,
        } => {
            if let Some((epoch, seq, state)) = progress.assembler.push(epoch, seq, part, of, state)
            {
                progress.epoch = Some(epoch);
                progress.last_seq = Some(seq);
                let _ = events.send(ClientEvent::Snapshot { epoch, seq, state });
            }
        }
        HostMsg::Event { epoch, seq, event } => {
            // An event after a gap would corrupt the mirror; reconnecting resumes from the last
            // one applied.
            if progress.epoch == Some(epoch)
                && progress.last_seq.is_some_and(|last| seq != last + 1)
            {
                return Some(Ended::Lost(AttachError::Protocol(
                    "an event was missing".to_string(),
                )));
            }
            progress.epoch = Some(epoch);
            progress.last_seq = Some(seq);
            let _ = events.send(ClientEvent::Event { epoch, seq, event });
        }
        HostMsg::Notice { severity, text } => {
            let _ = events.send(ClientEvent::Notice { severity, text });
        }
        HostMsg::Accepted { message_id } => {
            let _ = events.send(ClientEvent::Accepted { message_id });
        }
        HostMsg::PromptRejected { message_id, reason } => {
            let _ = events.send(ClientEvent::PromptRejected { message_id, reason });
        }
        HostMsg::Rejected { reason } => {
            return Some(match reason {
                Reason::Superseded | Reason::Locked => {
                    Ended::Over(Err(AttachError::Rejected(reason)))
                }
                other => Ended::Over(Err(AttachError::Rejected(other))),
            });
        }
        HostMsg::Bye => {
            let _ = events.send(ClientEvent::Bye);
            return Some(Ended::Over(Ok(())));
        }
    }
    None
}

fn relay_for(target: &Target, options: &AttachOptions) -> RelayMode {
    if let Some(relay) = &options.relay {
        return relay.clone();
    }
    match target {
        Target::Ticket(ticket) if ticket.node.addrs.iter().any(|a| a.is_relay()) => {
            RelayMode::Default
        }
        _ => RelayMode::Disabled,
    }
}

fn short(node: &str) -> String {
    node.chars().take(10).collect()
}

/// Remembers the host and where it was reached, so a later attach needs no ticket.
async fn remember(
    store: &dyn ClientStore,
    endpoint: &Endpoint,
    addr: &EndpointAddr,
    label: Option<&str>,
) -> Result<(), AttachError> {
    let mut known = addr.clone();
    if let Some(info) = endpoint.remote_info(addr.id).await {
        known = EndpointAddr::from_parts(info.id(), info.into_addrs().map(|a| a.into_addr()));
    }
    let node = addr.id.to_string();
    let host = StoredHost {
        label: label.map_or_else(|| short(&node), str::to_string),
        node,
        addr: known,
    };
    store
        .update_hosts(&mut |hosts| {
            let keep = hosts
                .hosts
                .iter()
                .find(|h| h.node == host.node && label.is_none())
                .map(|h| h.label.clone());
            let mut entry = host.clone();
            if let Some(keep) = keep {
                entry.label = keep;
            }
            hosts.upsert(entry);
        })
        .map_err(AttachError::Store)
}

async fn attach(
    target: Target,
    options: AttachOptions,
    store: Arc<dyn ClientStore>,
    events: &mpsc::UnboundedSender<ClientEvent>,
    mut input: mpsc::UnboundedReceiver<ClientMsg>,
) -> Result<(), AttachError> {
    let relay = relay_for(&target, &options);
    let endpoint = build_endpoint(store.as_ref(), relay, options.ca_tls.clone()).await?;
    let result = attach_with(
        &endpoint,
        target,
        &options,
        store.as_ref(),
        events,
        &mut input,
    )
    .await;
    endpoint.close().await;
    result
}

async fn attach_with(
    endpoint: &Endpoint,
    target: Target,
    options: &AttachOptions,
    store: &dyn ClientStore,
    events: &mpsc::UnboundedSender<ClientEvent>,
    input: &mut mpsc::UnboundedReceiver<ClientMsg>,
) -> Result<(), AttachError> {
    let mut progress = Progress {
        epoch: None,
        last_seq: None,
        assembler: SnapshotAssembler::default(),
    };
    let (addr, mut pair_secret) = match target {
        Target::Ticket(ticket) => (
            ticket.node.clone(),
            Some(BASE32_NOPAD.encode(&ticket.secret)),
        ),
        Target::Host(host) => (host.addr.clone(), None),
    };
    let mut backoff = FIRST_BACKOFF;
    loop {
        let auth = match &pair_secret {
            Some(secret) => Auth::Pair {
                secret: secret.clone(),
            },
            None => Auth::Resume {
                epoch: progress.epoch,
                last_seq: progress.last_seq,
            },
        };
        let hello = ClientMsg::Hello {
            v: VERSION,
            label: options.label.clone(),
            auth,
        };
        let pairing = pair_secret.is_some();
        let outcome = match open_link(endpoint, addr.clone(), &hello).await {
            Err(error) => Ended::Lost(error),
            Ok(mut link) => {
                match first_reply(&mut link, pairing).await {
                    Err(error) => Ended::Over(Err(error)),
                    Ok(()) => {
                        if pairing {
                            // The secret was spent by this pairing; later connections resume.
                            pair_secret = None;
                            remember(store, endpoint, &addr, options.host_label.as_deref()).await?;
                            let _ = events.send(ClientEvent::Paired);
                        }
                        backoff = FIRST_BACKOFF;
                        let _ = events.send(ClientEvent::Connected);
                        pump(link, events, input, &mut progress).await
                    }
                }
            }
        };
        match outcome {
            Ended::Over(result) => return result,
            Ended::Lost(error) => {
                // A pairing that failed before the host answered cannot be retried: its secret
                // may be spent.
                if !options.reconnect || pair_secret.is_some() {
                    return Err(error);
                }
                let _ = events.send(ClientEvent::Reconnecting {
                    after_ms: u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                });
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// With a pairing in flight the host answers `Paired` or `Rejected` only after its person
/// confirmed; a resume is answered by the first frame of the replay or by `Rejected`.
async fn first_reply(link: &mut Link, pairing: bool) -> Result<(), AttachError> {
    if !pairing {
        return Ok(());
    }
    let answer = async {
        match next_frame(link).await {
            Frame::Host(HostMsg::Paired) => Ok(()),
            Frame::Host(HostMsg::Rejected { reason }) => Err(AttachError::Rejected(reason)),
            Frame::Host(other) => Err(AttachError::Protocol(format!("{other:?}"))),
            Frame::Lost(error) => Err(error),
        }
    };
    tokio::time::timeout(PAIRING_WINDOW, answer)
        .await
        .map_err(|_| AttachError::Timeout)?
}

#[derive(Debug, PartialEq, Eq)]
pub struct Accepted {
    pub message_id: String,
}

/// `attach --message`: attaches, sends one prompt and returns once the host queued or refused it.
/// The wait covers the pairing window before it waits for the acknowledgement.
pub async fn send_message(
    target: Target,
    text: &str,
    options: AttachOptions,
    store: Arc<dyn ClientStore>,
) -> Result<Accepted, AttachError> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(AttachError::Rejected(Reason::TooLarge));
    }
    let pairing = matches!(target, Target::Ticket(_));
    let message_id = format!("msg-{}", uuid::Uuid::new_v4());
    let mut client = Client::start(
        target,
        AttachOptions {
            reconnect: false,
            ..options
        },
        store,
    );
    client.send(ClientMsg::Prompt {
        message_id: message_id.clone(),
        text: text.to_string(),
    });
    let deadline = Instant::now()
        + MESSAGE_WAIT
        + if pairing {
            PAIRING_WINDOW
        } else {
            Duration::ZERO
        }
        + CONNECT_LIMIT;
    loop {
        let event = tokio::time::timeout_at(deadline, client.events.recv())
            .await
            .map_err(|_| AttachError::Timeout)?;
        match event {
            Some(ClientEvent::Accepted { message_id: id }) if id == message_id => {
                return Ok(Accepted { message_id: id });
            }
            Some(ClientEvent::PromptRejected {
                message_id: id,
                reason,
            }) if id == message_id => return Err(AttachError::Rejected(reason)),
            Some(ClientEvent::Failed(error)) => return Err(error),
            Some(ClientEvent::Bye) | None => return Err(AttachError::Closed),
            Some(_) => {}
        }
    }
}
