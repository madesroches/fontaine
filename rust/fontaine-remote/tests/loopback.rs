//! Attach over in-process iroh endpoints: relay disabled, and a local relay for the relayed path.
//! No external service is contacted.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_remote::{
    AttachError, AttachOptions, BoxFuture, Client, ClientEvent, ClientMsg, ClientStore, Close,
    DeviceStore, Handled, HostSession, Hub, HubLimits, MemoryClientStore, MemoryDeviceStore,
    Reason, RelayMode, RemoteServer, Sender, ServerConfig, SystemClock, Target, Ticket, Waker,
    send_message,
};
use iroh::TransportAddr;
use iroh::tls::CaTlsConfig;

#[derive(Default)]
struct Session {
    confirm: Mutex<Option<bool>>,
    /// Never answers, as a person who walked away.
    hang: Mutex<bool>,
    messages: Mutex<Vec<(Sender, ClientMsg)>>,
    refused: Mutex<Vec<Reason>>,
    paired: Mutex<Vec<(Sender, String)>>,
    warnings: Mutex<Vec<String>>,
}

impl HostSession for Session {
    fn client_message(&self, from: &Sender, _label: &str, message: ClientMsg) -> Handled {
        let reply = match &message {
            ClientMsg::Prompt { message_id, .. } | ClientMsg::Slash { message_id, .. } => {
                Handled::Accepted {
                    message_id: message_id.clone(),
                }
            }
            _ => Handled::Silent,
        };
        self.messages.lock().unwrap().push((from.clone(), message));
        reply
    }

    fn confirm_pairing(&self, _node: &Sender, _label: &str) -> BoxFuture<bool> {
        if *self.hang.lock().unwrap() {
            return Box::pin(std::future::pending());
        }
        let answer = self.confirm.lock().unwrap().unwrap_or(true);
        Box::pin(async move { answer })
    }

    fn paired(&self, node: &Sender, label: &str) {
        self.paired
            .lock()
            .unwrap()
            .push((node.clone(), label.into()));
    }

    fn attached(&self, _node: &Sender, _label: &str) {}

    fn detached(&self, _node: &Sender) {}

    fn refused(&self, reason: Reason, _node: &Sender) {
        self.refused.lock().unwrap().push(reason);
    }

    fn warn(&self, text: &str) {
        self.warnings.lock().unwrap().push(text.into());
    }
}

struct NoWake;
impl Waker for NoWake {
    fn wake(&self) {}
}

struct Host {
    server: RemoteServer,
    session: Arc<Session>,
    store: Arc<MemoryDeviceStore>,
    hub: Arc<Hub>,
}

async fn host_with(relay: RelayMode, ca_tls: Option<CaTlsConfig>, timeout: Duration) -> Host {
    let session = Arc::new(Session::default());
    let store = Arc::new(MemoryDeviceStore::default());
    let hub = Hub::new(HubLimits::default(), Arc::new(NoWake));
    let mut config = ServerConfig::new(
        store.clone(),
        session.clone(),
        hub.clone(),
        Arc::new(SystemClock),
        relay,
    );
    config.ca_tls = ca_tls;
    config.pairing_timeout = timeout;
    let server = RemoteServer::start(config).await.expect("start");
    Host {
        server,
        session,
        store,
        hub,
    }
}

async fn host() -> Host {
    host_with(RelayMode::Disabled, None, Duration::from_secs(60)).await
}

fn ticket(host: &Host) -> Box<Ticket> {
    let ticket = host.server.mint_ticket(Duration::from_secs(60)).unwrap();
    Box::new(Ticket::parse(&ticket.encode()).unwrap())
}

fn options(label: &str) -> AttachOptions {
    AttachOptions {
        relay: Some(RelayMode::Disabled),
        ..AttachOptions::new(label)
    }
}

async fn next(client: &mut Client) -> ClientEvent {
    tokio::time::timeout(Duration::from_secs(20), client.events.recv())
        .await
        .expect("an event in time")
        .expect("the client is running")
}

/// The next event that is not a plain `Connected`/`Paired` step.
async fn until<T>(client: &mut Client, mut pick: impl FnMut(&ClientEvent) -> Option<T>) -> T {
    loop {
        let event = next(client).await;
        if let Some(found) = pick(&event) {
            return found;
        }
        if let ClientEvent::Failed(error) = &event {
            panic!("the attach failed: {error}");
        }
    }
}

fn failed(event: &ClientEvent) -> Option<&AttachError> {
    match event {
        ClientEvent::Failed(error) => Some(error),
        _ => None,
    }
}

#[tokio::test]
async fn a_ticket_pairs_after_confirmation_and_the_device_reattaches_by_node_id_without_one() {
    let host = host().await;
    let store = Arc::new(MemoryClientStore::default());
    let mut client = Client::start(
        Target::Ticket(ticket(&host)),
        options("laptop"),
        store.clone(),
    );
    until(&mut client, |e| {
        matches!(e, ClientEvent::Paired).then_some(())
    })
    .await;
    until(&mut client, |e| {
        matches!(e, ClientEvent::Connected).then_some(())
    })
    .await;
    assert_eq!(host.session.paired.lock().unwrap().len(), 1);
    assert_eq!(host.store.devices().unwrap().devices[0].label, "laptop");
    let stored = store.hosts().unwrap();
    assert_eq!(stored.hosts.len(), 1);
    assert_eq!(stored.hosts[0].node, host.server.node_id());
    assert!(!host.server.has_live_ticket(), "the secret is spent");
    client.send(ClientMsg::Prompt {
        message_id: "m1".into(),
        text: "hello".into(),
    });
    until(&mut client, |e| match e {
        ClientEvent::Accepted { message_id } => (message_id == "m1").then_some(()),
        _ => None,
    })
    .await;
    client.stop();

    // No ticket this time: the stored host and the paired list are enough.
    let stored = store.hosts().unwrap().hosts[0].clone();
    let mut again = Client::start(Target::Host(Box::new(stored)), options("laptop"), store);
    until(&mut again, |e| {
        matches!(e, ClientEvent::Connected).then_some(())
    })
    .await;
    again.stop();
    host.server.shutdown().await;
}

#[tokio::test]
async fn a_declined_pairing_is_refused_stores_nothing_and_spends_the_secret() {
    let host = host().await;
    *host.session.confirm.lock().unwrap() = Some(false);
    let ticket = ticket(&host);
    let store = Arc::new(MemoryClientStore::default());
    let mut client = Client::start(Target::Ticket(ticket.clone()), options("x"), store.clone());
    let error = until(&mut client, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("PairingDeclined"), "{error}");
    assert!(host.store.devices().unwrap().devices.is_empty());
    assert!(store.hosts().unwrap().hosts.is_empty());
    assert!(!host.server.has_live_ticket());
    *host.session.confirm.lock().unwrap() = Some(true);
    let mut retry = Client::start(Target::Ticket(ticket), options("x"), store);
    let error = until(&mut retry, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("Reused"), "{error}");
    host.server.shutdown().await;
}

#[tokio::test]
async fn nobody_confirming_in_time_is_a_pairing_timeout() {
    let host = host_with(RelayMode::Disabled, None, Duration::from_millis(300)).await;
    *host.session.hang.lock().unwrap() = true;
    let store = Arc::new(MemoryClientStore::default());
    let mut client = Client::start(Target::Ticket(ticket(&host)), options("x"), store);
    let error = until(&mut client, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("PairingTimeout"), "{error}");
    assert!(host.store.devices().unwrap().devices.is_empty());
    host.server.shutdown().await;
}

#[tokio::test]
async fn an_unpaired_node_without_a_secret_gets_not_paired() {
    let host = host().await;
    let stored = fontaine_remote::StoredHost {
        node: host.server.node_id(),
        label: "h".into(),
        addr: ticket(&host).node,
    };
    let store = Arc::new(MemoryClientStore::default());
    let mut client = Client::start(Target::Host(Box::new(stored)), options("x"), store);
    let error = until(&mut client, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("NotPaired"), "{error}");
    assert!(
        host.session
            .refused
            .lock()
            .unwrap()
            .contains(&Reason::NotPaired)
    );
    host.server.shutdown().await;
}

async fn paired_client(host: &Host, label: &str) -> (Client, Arc<MemoryClientStore>) {
    let store = Arc::new(MemoryClientStore::default());
    let mut client = Client::start(Target::Ticket(ticket(host)), options(label), store.clone());
    until(&mut client, |e| {
        matches!(e, ClientEvent::Connected).then_some(())
    })
    .await;
    (client, store)
}

#[tokio::test]
async fn a_second_connection_from_the_same_device_supersedes_the_first() {
    let host = host().await;
    let (first, store) = paired_client(&host, "a").await;
    let stored = store.hosts().unwrap().hosts[0].clone();
    let mut second = Client::start(Target::Host(Box::new(stored)), options("a"), store);
    until(&mut second, |e| {
        matches!(e, ClientEvent::Connected).then_some(())
    })
    .await;
    let mut first = first;
    let error = until(&mut first, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("Superseded"), "{error}");
    host.server.shutdown().await;
}

#[tokio::test]
async fn revoking_closes_the_live_connection_and_the_nodes_resume_gets_not_paired() {
    let host = host().await;
    let (mut client, store) = paired_client(&host, "phone").await;
    let node = host.store.devices().unwrap().devices[0].node.clone();
    host.store
        .update(&mut |list| {
            list.revoke(&node).unwrap();
        })
        .unwrap();
    assert!(host.server.revoke(&node));
    let error = until(&mut client, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("Revoked"), "{error}");
    let stored = store.hosts().unwrap().hosts[0].clone();
    let mut again = Client::start(Target::Host(Box::new(stored)), options("phone"), store);
    let error = until(&mut again, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("NotPaired"), "{error}");
    host.server.shutdown().await;
}

#[tokio::test]
async fn the_host_notices_a_revocation_in_the_file_on_the_next_client_message() {
    let host = host().await;
    let (mut client, _store) = paired_client(&host, "phone").await;
    let node = host.store.devices().unwrap().devices[0].node.clone();
    // Another process revoked it: the live connection is not told directly.
    host.store
        .update(&mut |list| {
            list.revoke(&node).unwrap();
        })
        .unwrap();
    client.send(ClientMsg::Interrupt);
    let error = until(&mut client, |e| failed(e).map(|e| format!("{e:?}"))).await;
    assert!(error.contains("Revoked"), "{error}");
    host.server.shutdown().await;
}

/// What the session's loop does when a device waits for a snapshot.
async fn serve_snapshot(host: &Host, state: &str) {
    for _ in 0..100 {
        if host.hub.snapshot_wanted() {
            host.hub.provide_snapshot(state);
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no device asked for a snapshot");
}

#[tokio::test]
async fn a_first_connection_gets_a_snapshot_then_events_in_order() {
    let host = host().await;
    let (mut client, store) = paired_client(&host, "a").await;
    serve_snapshot(&host, "{\"mirror\":0}").await;
    let (epoch, seq, state) = until(&mut client, |e| match e {
        ClientEvent::Snapshot { epoch, seq, state } => Some((*epoch, *seq, state.clone())),
        _ => None,
    })
    .await;
    assert_eq!(
        (epoch, seq, state.as_str()),
        (host.hub.epoch(), 0, "{\"mirror\":0}")
    );
    for n in 1..=3 {
        host.hub.publish(serde_json::json!({ "n": n }));
    }
    for want in 1..=3 {
        until(&mut client, |e| match e {
            ClientEvent::Event { seq, .. } => {
                assert_eq!(*seq, want);
                Some(())
            }
            _ => None,
        })
        .await;
    }
    client.stop();

    // A device with no position of its own is given the state as it stands.
    for n in 4..=6 {
        host.hub.publish(serde_json::json!({ "n": n }));
    }
    let stored = store.hosts().unwrap().hosts[0].clone();
    let mut again = Client::start(Target::Host(Box::new(stored)), options("a"), store);
    serve_snapshot(&host, "{\"mirror\":6}").await;
    let state = until(&mut again, |e| match e {
        ClientEvent::Snapshot { seq, state, .. } => Some((*seq, state.clone())),
        _ => None,
    })
    .await;
    assert_eq!(state, (6, "{\"mirror\":6}".to_string()));
    host.hub.publish(serde_json::json!({ "n": 7 }));
    until(&mut again, |e| match e {
        ClientEvent::Event { seq: 7, .. } => Some(()),
        _ => None,
    })
    .await;
    host.server.shutdown().await;
}

#[tokio::test]
async fn a_dropped_connection_reconnects_with_resume_and_replays_only_the_gap() {
    let host = host().await;
    let (mut client, _store) = paired_client(&host, "a").await;
    serve_snapshot(&host, "{}").await;
    host.hub.publish(serde_json::json!({ "n": 1 }));
    until(&mut client, |e| match e {
        ClientEvent::Event { seq: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    // Cut the connection from the host's side while the session goes on publishing.
    let node = host.store.devices().unwrap().devices[0].node.clone();
    host.hub.close_device(&node, Close::Overflow);
    host.hub.publish(serde_json::json!({ "n": 2 }));
    host.hub.publish(serde_json::json!({ "n": 3 }));
    until(&mut client, |e| {
        matches!(e, ClientEvent::Reconnecting { .. }).then_some(())
    })
    .await;
    // No snapshot is asked for: the ring covers the gap, so exactly events 2 and 3 arrive.
    let mut seqs = Vec::new();
    while seqs.len() < 2 {
        match next(&mut client).await {
            ClientEvent::Event { seq, .. } => seqs.push(seq),
            ClientEvent::Snapshot { .. } => panic!("a snapshot where a replay was possible"),
            ClientEvent::Failed(error) => panic!("failed: {error}"),
            _ => {}
        }
    }
    assert_eq!(seqs, vec![2, 3]);
    host.server.shutdown().await;
}

#[tokio::test]
async fn attach_message_pairs_sends_one_prompt_and_waits_for_the_acknowledgement() {
    let host = host().await;
    let accepted = send_message(
        Target::Ticket(ticket(&host)),
        "do the thing",
        options("ci"),
        Arc::new(MemoryClientStore::default()),
    )
    .await
    .unwrap();
    assert!(accepted.message_id.starts_with("msg-"));
    {
        let messages = host.session.messages.lock().unwrap();
        assert!(matches!(
            &messages[0].1,
            ClientMsg::Prompt { text, .. } if text == "do the thing"
        ));
    }
    host.server.shutdown().await;
}

#[tokio::test]
async fn an_oversized_message_is_refused_before_anything_is_sent() {
    let host = host().await;
    let big = "x".repeat(fontaine_remote::MAX_MESSAGE_BYTES + 1);
    let result = send_message(
        Target::Ticket(ticket(&host)),
        &big,
        options("ci"),
        Arc::new(MemoryClientStore::default()),
    )
    .await;
    assert!(matches!(
        result,
        Err(AttachError::Rejected(Reason::TooLarge))
    ));
    assert!(host.server.has_live_ticket(), "nothing was spent");
    host.server.shutdown().await;
}

#[tokio::test]
async fn a_raw_oversized_client_frame_is_too_large() {
    use fontaine_remote::protocol::{CLIENT_FRAME_CAP, HOST_FRAME_CAP, Wait, read_frame};
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    let host = host().await;
    let ticket = ticket(&host);
    let client = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let connection = client
        .connect(ticket.node.clone(), fontaine_remote::ALPN)
        .await
        .unwrap();
    let (mut tx, mut rx) = connection.open_bi().await.unwrap();
    tx.write_all(&((CLIENT_FRAME_CAP as u32) + 1).to_be_bytes())
        .await
        .unwrap();
    let body = read_frame(&mut rx, HOST_FRAME_CAP, Wait::Whole)
        .await
        .unwrap();
    let message: fontaine_remote::HostMsg = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        message,
        fontaine_remote::HostMsg::Rejected {
            reason: Reason::TooLarge
        }
    );
    assert!(host.server.has_live_ticket());
    client.close().await;
    host.server.shutdown().await;
}

#[tokio::test]
async fn a_connection_that_opens_no_stream_is_closed() {
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    let host = host().await;
    let ticket = ticket(&host);
    let client = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let connection = client
        .connect(ticket.node.clone(), fontaine_remote::ALPN)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), connection.closed())
        .await
        .expect("the server frees an idle connection");
    client.close().await;
    host.server.shutdown().await;
}

#[tokio::test]
async fn shutdown_says_goodbye_and_a_later_attach_cannot_connect() {
    let host = host().await;
    let (mut client, store) = paired_client(&host, "a").await;
    let ticket_node = ticket(&host).node;
    host.server.shutdown().await;
    until(&mut client, |e| matches!(e, ClientEvent::Bye).then_some(())).await;
    let stored = fontaine_remote::StoredHost {
        addr: ticket_node,
        ..store.hosts().unwrap().hosts[0].clone()
    };
    let result = send_message(
        Target::Host(Box::new(stored)),
        "late",
        AttachOptions {
            reconnect: false,
            ..options("a")
        },
        store,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(AttachError::Connect(_) | AttachError::Closed | AttachError::Timeout)
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_relayed_attach_reaches_the_session_through_a_local_relay() {
    let (map, _url, _relay) = iroh::test_utils::run_relay_server().await.unwrap();
    let host = host_with(
        RelayMode::Custom(map.clone()),
        Some(CaTlsConfig::insecure_skip_verify()),
        Duration::from_secs(60),
    )
    .await;
    let mut ticket = host.server.mint_ticket(Duration::from_secs(60)).unwrap();
    assert!(ticket.node.addrs.iter().any(TransportAddr::is_relay));
    // Strip the direct addresses so the only path is the relay.
    ticket.node.addrs.retain(TransportAddr::is_relay);
    let mut client = Client::start(
        Target::Ticket(Box::new(ticket)),
        AttachOptions {
            relay: Some(RelayMode::Custom(map)),
            ca_tls: Some(CaTlsConfig::insecure_skip_verify()),
            ..AttachOptions::new("relayed")
        },
        Arc::new(MemoryClientStore::default()),
    );
    until(&mut client, |e| {
        matches!(e, ClientEvent::Connected).then_some(())
    })
    .await;
    host.server.shutdown().await;
}
