use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, Incoming, presets};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, RelayMode};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

use crate::inbox::{Clock, Delivery, MintError, RemoteInbox};
use crate::protocol::{
    ALPN, FrameError, Reason, Response, Sender, parse_request, read_frame, write_frame,
};
use crate::ticket::Ticket;

/// The application error code a client sees when the session closed its inbox.
pub const CLOSED_CODE: u32 = 1;
const MAX_CONNECTIONS: usize = 32;
const ONLINE_WAIT: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const STREAM_TIMEOUT: Duration = crate::protocol::FRAME_TIMEOUT;
#[cfg(test)]
const STREAM_TIMEOUT: Duration = Duration::from_millis(300);
const RESPONSE_LINGER: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum ServerError {
    Bind(iroh::endpoint::BindError),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Bind(_) => f.write_str("could not open the network endpoint"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServerError::Bind(e) => Some(e),
        }
    }
}

/// One live endpoint for one session. Its key is minted per start and never persisted, so the
/// node id in a ticket names this enable and nothing later.
pub struct RemoteServer {
    endpoint: Endpoint,
    inbox: Arc<RemoteInbox>,
    shutdown: watch::Sender<bool>,
    accept_loop: JoinHandle<()>,
}

impl RemoteServer {
    pub async fn start(
        delivery: Arc<dyn Delivery>,
        clock: Arc<dyn Clock>,
        relay: RelayMode,
    ) -> Result<RemoteServer, ServerError> {
        Self::start_with(delivery, clock, relay, None).await
    }

    /// `start` with an explicit relay trust store, for a relay with a self-signed certificate.
    pub async fn start_with(
        delivery: Arc<dyn Delivery>,
        clock: Arc<dyn Clock>,
        relay: RelayMode,
        ca_tls: Option<CaTlsConfig>,
    ) -> Result<RemoteServer, ServerError> {
        let relayed = !matches!(relay, RelayMode::Disabled);
        // No address-lookup service: the ticket carries the addresses, so nothing is published.
        let mut builder = Endpoint::builder(presets::Minimal)
            .relay_mode(relay)
            .alpns(vec![ALPN.to_vec()]);
        if let Some(ca_tls) = ca_tls {
            builder = builder.ca_tls_config(ca_tls);
        }
        let endpoint = builder.bind().await.map_err(ServerError::Bind)?;
        if relayed {
            // A relay that cannot be reached leaves a direct-only ticket rather than a hang.
            let _ = tokio::time::timeout(ONLINE_WAIT, endpoint.online()).await;
        }
        let inbox = Arc::new(RemoteInbox::new(delivery, clock));
        let (shutdown, closing) = watch::channel(false);
        let accept_loop = tokio::spawn(accept_loop(endpoint.clone(), inbox.clone(), closing));
        Ok(RemoteServer {
            endpoint,
            inbox,
            shutdown,
            accept_loop,
        })
    }

    /// Mints a ticket for the current addresses, revoking the previous one.
    pub fn mint_ticket(&self, ttl: Duration) -> Result<Ticket, MintError> {
        let minted = self.inbox.mint(ttl)?;
        Ok(Ticket::new(
            self.endpoint.addr(),
            minted.secret,
            minted.expires_at,
        ))
    }

    pub fn has_live_ticket(&self) -> bool {
        self.inbox.has_live_ticket()
    }

    /// Stops accepting, tells in-flight clients the inbox is closed and closes the endpoint.
    pub async fn shutdown(self) {
        self.inbox.close();
        let _ = self.shutdown.send(true);
        self.accept_loop.abort();
        self.endpoint.close().await;
    }
}

async fn accept_loop(endpoint: Endpoint, inbox: Arc<RemoteInbox>, closing: watch::Receiver<bool>) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            // Over the connection cap: refuse rather than queue stranger traffic.
            incoming.refuse();
            continue;
        };
        let inbox = inbox.clone();
        let closing = closing.clone();
        tokio::spawn(async move {
            serve(incoming, inbox, closing).await;
            drop(permit);
        });
    }
}

async fn serve(incoming: Incoming, inbox: Arc<RemoteInbox>, mut closing: watch::Receiver<bool>) {
    let Ok(connection) = incoming.await else {
        return;
    };
    let from: Sender = connection.remote_id().to_string();
    tokio::select! {
        () = exchange(&connection, &inbox, &from) => {}
        _ = closing.wait_for(|closed| *closed) => {
            connection.close(CLOSED_CODE.into(), b"closed");
            return;
        }
    }
    connection.close(0u32.into(), b"done");
}

async fn exchange(connection: &Connection, inbox: &RemoteInbox, from: &Sender) {
    // iroh keep-alives hold an idle connection open, so a peer that never opens a stream would
    // otherwise pin its connection slot for good.
    let Ok(Ok((mut send, mut recv))) =
        tokio::time::timeout(STREAM_TIMEOUT, connection.accept_bi()).await
    else {
        return;
    };
    let response = match read_frame(&mut recv).await {
        Ok(body) => match parse_request(&body) {
            Ok(request) => inbox.handle(from, request),
            Err(reason) => reject(inbox, from, reason),
        },
        Err(FrameError::TooLarge) => reject(inbox, from, Reason::TooLarge),
        Err(_) => return,
    };
    if write_frame(&mut send, &response).await.is_err() {
        return;
    }
    let _ = send.finish();
    // The response is acknowledged once the client closes; a client that never does is cut off.
    let _ = tokio::time::timeout(RESPONSE_LINGER, connection.closed()).await;
}

fn reject(inbox: &RemoteInbox, from: &Sender, reason: Reason) -> Response {
    inbox.reject(from, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::SystemClock;
    use crate::protocol::Reason;

    struct Nobody;
    impl Delivery for Nobody {
        fn accept(
            &self,
            _: &Sender,
            _: &str,
            _: String,
        ) -> Result<(), crate::inbox::DeliveryError> {
            Ok(())
        }
        fn on_reject(&self, _: Reason, _: &Sender) {}
    }

    #[tokio::test]
    async fn a_connection_that_opens_no_stream_is_closed() {
        let server =
            RemoteServer::start(Arc::new(Nobody), Arc::new(SystemClock), RelayMode::Disabled)
                .await
                .expect("start");
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .expect("bind");
        let connection = client
            .connect(server.endpoint.addr(), ALPN)
            .await
            .expect("connect");
        tokio::time::timeout(Duration::from_secs(5), connection.closed())
            .await
            .expect("the server frees an idle connection");
        client.close().await;
        server.shutdown().await;
    }
}
