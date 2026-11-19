use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, presets};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, RelayMode, SecretKey};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::hub::{Close, Hub, Next, Registration};
use crate::inbox::{Clock, MintError, Pairing};
use crate::paired::{DeviceStore, PairedDevice};
use crate::protocol::{
    ALPN, Auth, CLIENT_FRAME_CAP, ClientMsg, FrameError, HostMsg, Level, MAX_FEEDBACK_BYTES,
    MAX_LABEL_CHARS, MAX_MESSAGE_BYTES, MAX_MESSAGE_ID_BYTES, Reason, Sender, Wait, encode_frame,
    parse_client, parse_hello, read_frame, write_body, write_frame,
};
use crate::session::{Handled, HostSession};
use crate::ticket::Ticket;

const MAX_CONNECTIONS: usize = 32;
const ONLINE_WAIT: Duration = Duration::from_secs(5);
/// Both sides ping this often; a side that has heard nothing for `IDLE_LIMIT` calls the
/// connection lost.
pub const KEEPALIVE: Duration = Duration::from_secs(15);
pub const IDLE_LIMIT: Duration = Duration::from_secs(45);
const WRITE_LIMIT: Duration = Duration::from_secs(30);
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
const LINGER: Duration = Duration::from_secs(2);
const DRAIN_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub enum ServerError {
    Bind(iroh::endpoint::BindError),
    Key(std::io::Error),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Bind(_) => f.write_str("could not open the network endpoint"),
            ServerError::Key(_) => f.write_str("could not read or create the host key"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServerError::Bind(e) => Some(e),
            ServerError::Key(e) => Some(e),
        }
    }
}

pub struct ServerConfig {
    pub store: Arc<dyn DeviceStore>,
    pub session: Arc<dyn HostSession>,
    pub hub: Arc<Hub>,
    pub clock: Arc<dyn Clock>,
    pub relay: RelayMode,
    /// Overrides the relay trust store, for a relay with a self-signed certificate.
    pub ca_tls: Option<CaTlsConfig>,
    /// How long the person at the host has to confirm a pairing.
    pub pairing_timeout: Duration,
}

impl ServerConfig {
    pub fn new(
        store: Arc<dyn DeviceStore>,
        session: Arc<dyn HostSession>,
        hub: Arc<Hub>,
        clock: Arc<dyn Clock>,
        relay: RelayMode,
    ) -> ServerConfig {
        ServerConfig {
            store,
            session,
            hub,
            clock,
            relay,
            ca_tls: None,
            pairing_timeout: Duration::from_secs(60),
        }
    }
}

struct Shared {
    store: Arc<dyn DeviceStore>,
    session: Arc<dyn HostSession>,
    hub: Arc<Hub>,
    pairing: Pairing,
    pairing_timeout: Duration,
    clock: Arc<dyn Clock>,
}

/// One live endpoint for one session. Its key is the host's persistent one, so a paired device
/// finds it again by node id.
pub struct RemoteServer {
    endpoint: Endpoint,
    shared: Arc<Shared>,
    relay: RelayMode,
    closing: watch::Sender<bool>,
    accept_loop: JoinHandle<()>,
}

impl RemoteServer {
    pub async fn start(config: ServerConfig) -> Result<RemoteServer, ServerError> {
        let key = SecretKey::from_bytes(&config.store.host_key().map_err(ServerError::Key)?);
        let relayed = !matches!(config.relay, RelayMode::Disabled);
        let relay = config.relay.clone();
        // Only the default relay mode publishes the host's node id and home relay to iroh's
        // address-lookup service, which is what lets a paired device find it again.
        let builder = match config.relay {
            RelayMode::Default => Endpoint::builder(presets::N0).relay_mode(RelayMode::Default),
            other => Endpoint::builder(presets::Minimal).relay_mode(other),
        };
        let mut builder = builder.secret_key(key).alpns(vec![ALPN.to_vec()]);
        if let Some(ca_tls) = config.ca_tls {
            builder = builder.ca_tls_config(ca_tls);
        }
        let endpoint = builder.bind().await.map_err(ServerError::Bind)?;
        if relayed {
            // A relay that cannot be reached leaves a direct-only ticket rather than a hang.
            let _ = tokio::time::timeout(ONLINE_WAIT, endpoint.online()).await;
        }
        let shared = Arc::new(Shared {
            store: config.store,
            session: config.session,
            hub: config.hub,
            pairing: Pairing::new(config.clock.clone()),
            pairing_timeout: config.pairing_timeout,
            clock: config.clock,
        });
        let (closing, closed) = watch::channel(false);
        let accept_loop = tokio::spawn(accept_loop(endpoint.clone(), shared.clone(), closed));
        Ok(RemoteServer {
            endpoint,
            relay,
            shared,
            closing,
            accept_loop,
        })
    }

    pub fn node_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Mints a ticket for the current addresses, revoking the previous one.
    pub fn mint_ticket(&self, ttl: Duration) -> Result<Ticket, MintError> {
        let minted = self.shared.pairing.mint(ttl)?;
        let mut addr = self.endpoint.addr();
        // Every address in the ticket costs QR rows. The default relay publishes the host to
        // address lookup, which the device queries, so the id alone finds it; another relay is
        // not published, so its URL stays and the direct addresses go.
        match self.relay {
            RelayMode::Default => addr.addrs.clear(),
            RelayMode::Disabled => {}
            _ => addr.addrs.retain(|a| a.is_relay()),
        }
        Ok(Ticket::new(addr, minted.secret, minted.expires_at))
    }

    pub fn has_live_ticket(&self) -> bool {
        self.shared.pairing.has_live_ticket()
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.shared.hub
    }

    /// Closes a device's live connection, as `/remote revoke` does after removing its entry.
    pub fn revoke(&self, node: &str) -> bool {
        self.shared
            .hub
            .close_device(node, Close::Reject(Reason::Revoked))
    }

    /// Says goodbye to every device, stops accepting and closes the endpoint.
    pub async fn shutdown(self) {
        self.shared.pairing.close();
        self.shared.hub.close_all(Close::Bye);
        let drained = async {
            while self.shared.hub.device_count() > 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = tokio::time::timeout(DRAIN_WAIT, drained).await;
        let _ = self.closing.send(true);
        self.accept_loop.abort();
        self.endpoint.close().await;
    }
}

async fn accept_loop(endpoint: Endpoint, shared: Arc<Shared>, closed: watch::Receiver<bool>) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            // Over the connection cap: refuse rather than queue stranger traffic.
            incoming.refuse();
            continue;
        };
        let shared = shared.clone();
        let closed = closed.clone();
        tokio::spawn(async move {
            serve(incoming, shared, closed).await;
            drop(permit);
        });
    }
}

async fn serve(incoming: Incoming, shared: Arc<Shared>, mut closed: watch::Receiver<bool>) {
    let Ok(connection) = incoming.await else {
        return;
    };
    let from: Sender = connection.remote_id().to_string();
    tokio::select! {
        () = connect(&connection, &shared, &from) => {}
        _ = closed.wait_for(|closed| *closed) => {}
    }
    connection.close(0u32.into(), b"done");
}

/// The text a device calls itself: shown to the person at the host, so control characters and
/// excess length go.
fn clean_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "unnamed device".to_string()
    } else {
        cleaned.to_string()
    }
}

async fn reject(
    send: &mut SendStream,
    connection: &Connection,
    shared: &Shared,
    from: &Sender,
    reason: Reason,
) {
    shared.session.refused(reason, from);
    if write_frame(send, &HostMsg::Rejected { reason })
        .await
        .is_ok()
    {
        let _ = send.finish();
        // The rejection is acknowledged once the client closes; a client that never does is cut off.
        let _ = tokio::time::timeout(LINGER, connection.closed()).await;
    }
}

async fn connect(connection: &Connection, shared: &Shared, from: &Sender) {
    // iroh keep-alives hold an idle connection open, so a peer that never opens a stream would
    // otherwise pin its connection slot for good.
    let Ok(Ok((mut send, mut recv))) =
        tokio::time::timeout(STREAM_TIMEOUT, connection.accept_bi()).await
    else {
        return;
    };
    let hello = match read_frame(&mut recv, CLIENT_FRAME_CAP, Wait::Whole).await {
        Ok(body) => parse_hello(&body),
        Err(FrameError::TooLarge) => Err(Reason::TooLarge),
        Err(_) => return,
    };
    let (label, auth) = match hello {
        Ok(ClientMsg::Hello { label, auth, .. }) => (clean_label(&label), auth),
        Ok(_) => return reject(&mut send, connection, shared, from, Reason::Malformed).await,
        Err(reason) => return reject(&mut send, connection, shared, from, reason).await,
    };
    let resume = match auth {
        Auth::Pair { secret } => {
            if let Err(reason) = shared.pairing.verify(from, &secret) {
                return reject(&mut send, connection, shared, from, reason).await;
            }
            // The secret is spent whatever the person says; it authorizes nothing by itself.
            let confirm = shared.session.confirm_pairing(from, &label);
            let verdict = tokio::select! {
                answered = tokio::time::timeout(shared.pairing_timeout, confirm) => match answered {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(Reason::PairingDeclined),
                    Err(_) => Err(Reason::PairingTimeout),
                },
                // A client that went away closes the host's dialog with no reply.
                _ = connection.closed() => return,
            };
            if let Err(reason) = verdict {
                return reject(&mut send, connection, shared, from, reason).await;
            }
            let now = shared.clock.now_secs();
            let device = PairedDevice {
                node: from.clone(),
                label: label.clone(),
                paired_at: now,
                last_seen: now,
            };
            if let Err(e) = shared.store.update(&mut |list| list.upsert(device.clone())) {
                shared
                    .session
                    .warn(&format!("could not store the pairing of device {from}"), &e);
                return reject(&mut send, connection, shared, from, Reason::Closed).await;
            }
            shared.session.paired(from, &label);
            if write_frame(&mut send, &HostMsg::Paired).await.is_err() {
                return;
            }
            None
        }
        Auth::Resume { epoch, last_seq } => {
            match shared.store.devices() {
                Ok(list) if list.contains(from) => {}
                Ok(_) => {
                    let reason = shared.pairing.unpaired(from);
                    return reject(&mut send, connection, shared, from, reason).await;
                }
                Err(e) => {
                    shared.session.warn("could not read the paired devices", &e);
                    return reject(&mut send, connection, shared, from, Reason::NotPaired).await;
                }
            }
            epoch.zip(last_seq)
        }
    };
    let Some(registration) = shared.hub.register(from, &label, resume) else {
        return reject(&mut send, connection, shared, from, Reason::Closed).await;
    };
    let now = shared.clock.now_secs();
    if let Err(e) = shared.store.update(&mut |list| list.touch(from, now)) {
        shared.session.warn(
            &format!("could not record when device {from} was last seen"),
            &e,
        );
    }
    shared.session.attached(from, &label);
    run(connection, shared, from, &label, &registration, send, recv).await;
    shared.session.detached(from);
}

/// Whether the device's entry is gone from the paired list. A list that cannot be read cannot
/// vouch for the device either, so that counts as gone.
fn revoked(shared: &Shared, from: &Sender) -> bool {
    match shared.store.devices() {
        Ok(list) => !list.contains(from),
        Err(e) => {
            shared.session.warn(
                &format!("could not read the paired devices, so device {from} was disconnected"),
                &e,
            );
            true
        }
    }
}

async fn write_limited(send: &mut SendStream, frame: &[u8]) -> bool {
    matches!(
        tokio::time::timeout(WRITE_LIMIT, write_body(send, frame)).await,
        Ok(Ok(()))
    )
}

/// Turns what the session decided about a device's message into what that device is told.
fn tell(registration: &Registration, handled: Handled) {
    let message = match handled {
        Handled::Accepted { message_id } => HostMsg::Accepted { message_id },
        Handled::Rejected { message_id, reason } => HostMsg::PromptRejected { message_id, reason },
        Handled::Notice { severity, text } => HostMsg::Notice { severity, text },
        Handled::Silent => return,
    };
    registration.queue.push(encode_frame(&message));
}

enum Wire {
    Message(ClientMsg),
    Bad(Reason),
}

async fn read_client(recv: &mut RecvStream, out: mpsc::Sender<Wire>) {
    loop {
        let wire = match read_frame(recv, CLIENT_FRAME_CAP, Wait::Body).await {
            Ok(body) => match parse_client(&body) {
                Ok(message) => Wire::Message(message),
                Err(reason) => Wire::Bad(reason),
            },
            Err(FrameError::TooLarge) => Wire::Bad(Reason::TooLarge),
            Err(_) => return,
        };
        let stop = matches!(wire, Wire::Bad(_));
        if out.send(wire).await.is_err() || stop {
            return;
        }
    }
}

/// Checks what the crate can check of a device's message before the session sees it.
fn precheck(message: &ClientMsg) -> Result<(), (String, Reason)> {
    let line = match message {
        ClientMsg::Prompt { message_id, text } => Some((message_id, text.as_str())),
        ClientMsg::Slash { message_id, line } => Some((message_id, line.as_str())),
        _ => None,
    };
    let Some((id, text)) = line else {
        return Ok(());
    };
    if id.is_empty() || id.len() > MAX_MESSAGE_ID_BYTES || text.trim().is_empty() {
        return Err((id.clone(), Reason::Malformed));
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err((id.clone(), Reason::TooLarge));
    }
    Ok(())
}

async fn run(
    connection: &Connection,
    shared: &Shared,
    from: &Sender,
    label: &str,
    registration: &Registration,
    mut send: SendStream,
    mut recv: RecvStream,
) {
    // The first frame tells the device it is attached, whether or not anything is to replay.
    registration.queue.push(encode_frame(&HostMsg::Ping));
    let (wire_tx, mut wire_rx) = mpsc::channel(32);
    let reader = tokio::spawn(async move { read_client(&mut recv, wire_tx).await });
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.reset();
    let mut last_heard = Instant::now();
    loop {
        match registration.queue.take() {
            Next::Frames(frames) => {
                for frame in frames {
                    if !write_limited(&mut send, &frame).await {
                        reader.abort();
                        return;
                    }
                }
                continue;
            }
            Next::Closed(how) => {
                match how {
                    Close::Reject(reason) => {
                        shared.session.refused(reason, from);
                        let _ = write_frame(&mut send, &HostMsg::Rejected { reason }).await;
                    }
                    Close::Bye => {
                        let _ = write_frame(&mut send, &HostMsg::Bye).await;
                    }
                    Close::Overflow => {}
                }
                let _ = send.finish();
                let _ = tokio::time::timeout(LINGER, connection.closed()).await;
                reader.abort();
                return;
            }
            Next::Empty => {}
        }
        tokio::select! {
            () = registration.queue.wait() => {}
            wire = wire_rx.recv() => {
                let Some(wire) = wire else {
                    return;
                };
                last_heard = Instant::now();
                if revoked(shared, from) {
                    registration.queue.close(Close::Reject(Reason::Revoked));
                    continue;
                }
                match wire {
                    Wire::Bad(reason) => registration.queue.close(Close::Reject(reason)),
                    Wire::Message(message) => dispatch(shared, registration, from, label, message),
                }
            }
            _ = keepalive.tick() => {
                if last_heard.elapsed() >= IDLE_LIMIT {
                    reader.abort();
                    return;
                }
                if revoked(shared, from) {
                    registration.queue.close(Close::Reject(Reason::Revoked));
                    continue;
                }
                registration.queue.push(encode_frame(&HostMsg::Ping));
            }
        }
    }
}

fn dispatch(
    shared: &Shared,
    registration: &Registration,
    from: &Sender,
    label: &str,
    message: ClientMsg,
) {
    match &message {
        ClientMsg::Hello { .. } => {
            registration.queue.close(Close::Reject(Reason::Malformed));
            return;
        }
        ClientMsg::Ping => return,
        ClientMsg::ApprovalAnswer { answer, .. }
            if answer
                .feedback
                .as_ref()
                .is_some_and(|f| f.len() > MAX_FEEDBACK_BYTES) =>
        {
            tell(
                registration,
                Handled::Notice {
                    severity: Level::Warning,
                    text: format!("deny feedback is capped at {MAX_FEEDBACK_BYTES} bytes"),
                },
            );
            return;
        }
        _ => {}
    }
    if let Err((message_id, reason)) = precheck(&message) {
        tell(registration, Handled::Rejected { message_id, reason });
        return;
    }
    tell(
        registration,
        shared.session.client_message(from, label, message),
    );
}
