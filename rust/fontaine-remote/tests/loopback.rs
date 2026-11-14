use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_remote::{
    Clock, Delivery, DeliveryError, Reason, RelayMode, RemoteServer, SendError, SendOptions,
    Sender, SystemClock, Ticket, send,
};
use iroh::TransportAddr;
use iroh::tls::CaTlsConfig;

#[derive(Default)]
struct Host {
    accepted: Mutex<Vec<(Sender, String, String)>>,
    rejected: Mutex<Vec<Reason>>,
    cap: Mutex<Option<usize>>,
}

impl Delivery for Host {
    fn accept(&self, from: &Sender, id: &str, text: String) -> Result<(), DeliveryError> {
        let mut accepted = self.accepted.lock().unwrap();
        if self
            .cap
            .lock()
            .unwrap()
            .is_some_and(|cap| accepted.len() >= cap)
        {
            return Err(DeliveryError::Full);
        }
        accepted.push((from.clone(), id.into(), text));
        Ok(())
    }

    fn on_reject(&self, reason: Reason, _node: &Sender) {
        self.rejected.lock().unwrap().push(reason);
    }
}

struct Skewed(u64);
impl Clock for Skewed {
    fn now_secs(&self) -> u64 {
        SystemClock.now_secs() + self.0
    }
}

async fn direct_server(host: Arc<Host>) -> RemoteServer {
    RemoteServer::start(host, Arc::new(SystemClock), RelayMode::Disabled)
        .await
        .expect("start")
}

fn options() -> SendOptions {
    SendOptions {
        timeout: Duration::from_secs(15),
        ..SendOptions::default()
    }
}

fn is_rejected(result: &Result<fontaine_remote::Accepted, SendError>, reason: Reason) -> bool {
    matches!(result, Err(SendError::Rejected(r)) if *r == reason)
}

#[tokio::test]
async fn a_direct_message_is_accepted_once_and_the_ticket_is_then_spent() {
    let host = Arc::new(Host::default());
    let server = direct_server(host.clone()).await;
    let ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
    let parsed = Ticket::parse(&ticket.encode()).unwrap();
    let accepted = send(&parsed, "hello there", options()).await.unwrap();
    assert!(accepted.message_id.starts_with("msg-"));
    {
        let queued = host.accepted.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].1, accepted.message_id);
        assert_eq!(queued[0].2, "hello there");
    }
    let again = send(&parsed, "second", options()).await;
    assert!(is_rejected(&again, Reason::Reused), "{again:?}");
    assert!(!server.has_live_ticket());
    server.shutdown().await;
}

#[tokio::test]
async fn an_invalid_secret_is_a_bad_ticket() {
    let host = Arc::new(Host::default());
    let server = direct_server(host.clone()).await;
    let mut ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
    ticket.secret[0] ^= 0xff;
    let result = send(&ticket, "x", options()).await;
    assert!(is_rejected(&result, Reason::BadTicket), "{result:?}");
    assert!(host.accepted.lock().unwrap().is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn an_expired_ticket_is_refused() {
    let host = Arc::new(Host::default());
    let server = RemoteServer::start(host, Arc::new(Skewed(0)), RelayMode::Disabled)
        .await
        .unwrap();
    let mut ticket = server.mint_ticket(Duration::from_secs(1)).unwrap();
    ticket.expires_at = 0;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let result = send(&ticket, "x", options()).await;
    assert!(is_rejected(&result, Reason::Expired), "{result:?}");
    server.shutdown().await;
}

#[tokio::test]
async fn an_oversized_message_is_too_large_and_a_full_inbox_keeps_the_ticket_live() {
    let host = Arc::new(Host::default());
    let server = direct_server(host.clone()).await;
    let ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
    let big = "x".repeat(fontaine_remote::MAX_MESSAGE_BYTES + 1);
    let result = send(&ticket, &big, options()).await;
    assert!(is_rejected(&result, Reason::TooLarge), "{result:?}");

    *host.cap.lock().unwrap() = Some(0);
    let full = send(&ticket, "x", options()).await;
    assert!(is_rejected(&full, Reason::InboxFull), "{full:?}");
    assert!(server.has_live_ticket());
    *host.cap.lock().unwrap() = None;
    send(&ticket, "x", options())
        .await
        .expect("same ticket retries");
    server.shutdown().await;
}

#[tokio::test]
async fn a_raw_oversized_frame_is_too_large() {
    use fontaine_remote::protocol::{MAX_FRAME, read_frame};
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    let host = Arc::new(Host::default());
    let server = direct_server(host.clone()).await;
    let ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
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
    tx.write_all(&((MAX_FRAME as u32) + 1).to_be_bytes())
        .await
        .unwrap();
    let body = read_frame(&mut rx).await.unwrap();
    let response: fontaine_remote::Response = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        response,
        fontaine_remote::Response::rejected(Reason::TooLarge)
    );
    assert!(server.has_live_ticket());
    client.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_the_endpoint_so_a_later_send_fails_to_connect() {
    let host = Arc::new(Host::default());
    let server = direct_server(host).await;
    let ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
    server.shutdown().await;
    let result = send(
        &ticket,
        "x",
        SendOptions {
            timeout: Duration::from_secs(4),
            retry: false,
            ..SendOptions::default()
        },
    )
    .await;
    assert!(
        matches!(
            result,
            Err(SendError::Connect(_) | SendError::Closed | SendError::Timeout)
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_lost_ack_retry_queues_exactly_once() {
    use fontaine_remote::protocol::{ALPN, Request, VERSION, read_frame, write_frame};
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    let host = Arc::new(Host::default());
    let server = direct_server(host.clone()).await;
    let ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
    let client = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let request = Request {
        v: VERSION,
        secret: data_encoding::BASE32_NOPAD.encode(&ticket.secret),
        message_id: "msg-lost".into(),
        text: "once".into(),
    };
    // First attempt: the connection drops before the response is read.
    let connection = client.connect(ticket.node.clone(), ALPN).await.unwrap();
    let (mut tx, _rx) = connection.open_bi().await.unwrap();
    write_frame(&mut tx, &request).await.unwrap();
    tx.finish().unwrap();
    for _ in 0..50 {
        if !host.accepted.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    connection.close(0u32.into(), b"drop");
    // The retry, same endpoint and message id, gets the original acceptance.
    let connection = client.connect(ticket.node.clone(), ALPN).await.unwrap();
    let (mut tx, mut rx) = connection.open_bi().await.unwrap();
    write_frame(&mut tx, &request).await.unwrap();
    tx.finish().unwrap();
    let body = read_frame(&mut rx).await.unwrap();
    let response: fontaine_remote::Response = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        response,
        fontaine_remote::Response::accepted("msg-lost".into())
    );
    assert_eq!(host.accepted.lock().unwrap().len(), 1);
    client.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_relayed_message_reaches_the_session_through_a_local_relay() {
    let (map, _url, _relay) = iroh::test_utils::run_relay_server().await.unwrap();
    let host = Arc::new(Host::default());
    let server = RemoteServer::start_with(
        host.clone(),
        Arc::new(SystemClock),
        RelayMode::Custom(map.clone()),
        Some(CaTlsConfig::insecure_skip_verify()),
    )
    .await
    .unwrap();
    let mut ticket = server.mint_ticket(Duration::from_secs(60)).unwrap();
    assert!(ticket.node.addrs.iter().any(TransportAddr::is_relay));
    // Strip the direct addresses so the only path is the relay.
    ticket.node.addrs.retain(TransportAddr::is_relay);
    let accepted = send(
        &ticket,
        "via relay",
        SendOptions {
            relay: Some(RelayMode::Custom(map)),
            ca_tls: Some(CaTlsConfig::insecure_skip_verify()),
            ..options()
        },
    )
    .await
    .unwrap();
    assert_eq!(host.accepted.lock().unwrap()[0].1, accepted.message_id);
    server.shutdown().await;
}
