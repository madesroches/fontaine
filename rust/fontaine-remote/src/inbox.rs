use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use data_encoding::BASE32_NOPAD;
use sha2::{Digest, Sha256};

use crate::protocol::{
    MAX_MESSAGE_BYTES, MAX_MESSAGE_ID_BYTES, MAX_TTL, Reason, Request, Response, Sender, Status,
    VERSION,
};

const MAX_BAD_ATTEMPTS: u32 = 5;
const LOCKOUT_SECS: u64 = 30;
const MAX_TRACKED_NODES: usize = 256;

pub trait Clock: Send + Sync {
    /// Unix seconds.
    fn now_secs(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryError {
    Full,
    Closed,
}

/// The host side of the inbox: where an authenticated line goes and where rejects are reported.
pub trait Delivery: Send + Sync {
    /// Queues the line. Must not block: it runs under the inbox lock so two concurrent first
    /// requests cannot both be accepted.
    fn accept(&self, from: &Sender, message_id: &str, text: String) -> Result<(), DeliveryError>;

    fn on_reject(&self, reason: Reason, node: &Sender);
}

#[derive(Debug, PartialEq, Eq)]
pub enum MintError {
    ZeroTtl,
    TtlTooLong,
}

impl std::fmt::Display for MintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MintError::ZeroTtl => f.write_str("a ticket lifetime must be positive"),
            MintError::TtlTooLong => write!(
                f,
                "a ticket lifetime is at most {} hours",
                MAX_TTL.as_secs() / 3600
            ),
        }
    }
}

impl std::error::Error for MintError {}

#[derive(Default)]
struct Attempts {
    count: u32,
    window_start: u64,
    locked_until: u64,
}

struct Live {
    secret_hash: [u8; 32],
    expires_at: u64,
    /// Set by the first accepted request: the node the secret is bound to.
    bound: Option<Sender>,
}

#[derive(Default)]
struct State {
    live: Option<Live>,
    /// The stored original response per (node, message id), for lost-ack retries.
    seen: HashMap<(Sender, String), Response>,
    attempts: HashMap<Sender, Attempts>,
    closed: bool,
}

pub struct RemoteInbox {
    state: Mutex<State>,
    delivery: Arc<dyn Delivery>,
    clock: Arc<dyn Clock>,
}

fn hash(secret: &[u8]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Minted {
    pub secret: [u8; 32],
    pub expires_at: u64,
}

impl RemoteInbox {
    pub fn new(delivery: Arc<dyn Delivery>, clock: Arc<dyn Clock>) -> RemoteInbox {
        RemoteInbox {
            state: Mutex::new(State::default()),
            delivery,
            clock,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // The state feeds an authentication decision, so a poisoned lock is a bug to surface.
        self.state
            .lock()
            .expect("the remote inbox lock is poisoned")
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Mints a fresh one-time secret and revokes the previous one, consumed or not.
    pub fn mint(&self, ttl: Duration) -> Result<Minted, MintError> {
        if ttl.is_zero() {
            return Err(MintError::ZeroTtl);
        }
        if ttl > MAX_TTL {
            return Err(MintError::TtlTooLong);
        }
        let mut secret = [0u8; 32];
        secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let expires_at = self.clock.now_secs().saturating_add(ttl.as_secs());
        let mut state = self.state();
        state.seen.clear();
        state.live = Some(Live {
            secret_hash: hash(&secret),
            expires_at,
            bound: None,
        });
        Ok(Minted { secret, expires_at })
    }

    /// Whether a ticket could still be used for a first message.
    pub fn has_live_ticket(&self) -> bool {
        let now = self.clock.now_secs();
        let state = self.state();
        !state.closed
            && state
                .live
                .as_ref()
                .is_some_and(|live| live.bound.is_none() && now < live.expires_at)
    }

    /// Stops accepting: every later request is `Closed` and consumes nothing.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.live = None;
        state.seen.clear();
    }

    pub fn handle(&self, from: &Sender, request: Request) -> Response {
        let response = self.decide(from, request);
        if let Status::Rejected { reason } = response.status {
            self.delivery.on_reject(reason, from);
        }
        response
    }

    /// A rejection decided before the request reached `handle` (a bad frame or version).
    pub fn reject(&self, from: &Sender, reason: Reason) -> Response {
        self.delivery.on_reject(reason, from);
        Response::rejected(reason)
    }

    fn decide(&self, from: &Sender, request: Request) -> Response {
        if request.v != VERSION {
            return Response::rejected(Reason::UnsupportedVersion);
        }
        if request.message_id.is_empty()
            || request.message_id.len() > MAX_MESSAGE_ID_BYTES
            || request.text.trim().is_empty()
        {
            return Response::rejected(Reason::Malformed);
        }
        let now = self.clock.now_secs();
        let mut guard = self.state();
        let state = &mut *guard;
        if state.closed {
            return Response::rejected(Reason::Closed);
        }
        if state
            .attempts
            .get(from)
            .is_some_and(|a| a.locked_until > now)
        {
            return Response::rejected(Reason::Locked);
        }
        let presented = BASE32_NOPAD
            .decode(request.secret.to_uppercase().as_bytes())
            .ok()
            .map(|bytes| hash(&bytes));
        let matches = match (&state.live, presented) {
            (Some(live), Some(presented)) => constant_time_eq(&live.secret_hash, &presented),
            _ => false,
        };
        if !matches {
            record_bad_attempt(&mut state.attempts, from, now);
            return Response::rejected(Reason::BadTicket);
        }
        let live = state.live.as_ref().expect("a matching secret is live");
        if now >= live.expires_at {
            return Response::rejected(Reason::Expired);
        }
        if let Some(bound) = &live.bound {
            let key = (from.clone(), request.message_id.clone());
            if bound == from
                && let Some(original) = state.seen.get(&key)
            {
                return original.clone();
            }
            return Response::rejected(Reason::Reused);
        }
        if request.text.len() > MAX_MESSAGE_BYTES {
            return Response::rejected(Reason::TooLarge);
        }
        match self
            .delivery
            .accept(from, &request.message_id, request.text)
        {
            Ok(()) => {
                let response = Response::accepted(request.message_id.clone());
                state
                    .live
                    .as_mut()
                    .expect("a matching secret is live")
                    .bound = Some(from.clone());
                state
                    .seen
                    .insert((from.clone(), request.message_id), response.clone());
                response
            }
            Err(DeliveryError::Full) => Response::rejected(Reason::InboxFull),
            Err(DeliveryError::Closed) => Response::rejected(Reason::Closed),
        }
    }
}

fn record_bad_attempt(attempts: &mut HashMap<Sender, Attempts>, from: &Sender, now: u64) {
    attempts.retain(|_, a| a.locked_until > now || now < a.window_start + LOCKOUT_SECS);
    if !attempts.contains_key(from) && attempts.len() >= MAX_TRACKED_NODES {
        // Under a flood the oldest window goes first; the stranger loses only their own count.
        if let Some(oldest) = attempts
            .iter()
            .min_by_key(|(_, a)| a.window_start)
            .map(|(node, _)| node.clone())
        {
            attempts.remove(&oldest);
        }
    }
    let entry = attempts.entry(from.clone()).or_default();
    if entry.count == 0 || now >= entry.window_start + LOCKOUT_SECS {
        entry.count = 0;
        entry.window_start = now;
    }
    entry.count += 1;
    if entry.count >= MAX_BAD_ATTEMPTS {
        entry.locked_until = now + LOCKOUT_SECS;
        entry.count = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct TestClock(AtomicU64);
    impl Clock for TestClock {
        fn now_secs(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct Host {
        accepted: Mutex<Vec<(Sender, String, String)>>,
        rejected: Mutex<Vec<(Reason, Sender)>>,
        cap: Mutex<Option<usize>>,
        closed: Mutex<bool>,
    }

    impl Delivery for Host {
        fn accept(&self, from: &Sender, id: &str, text: String) -> Result<(), DeliveryError> {
            if *self.closed.lock().unwrap() {
                return Err(DeliveryError::Closed);
            }
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

        fn on_reject(&self, reason: Reason, node: &Sender) {
            self.rejected.lock().unwrap().push((reason, node.clone()));
        }
    }

    fn setup() -> (Arc<RemoteInbox>, Arc<Host>, Arc<TestClock>) {
        let host = Arc::new(Host::default());
        let clock = Arc::new(TestClock(AtomicU64::new(1_000)));
        let inbox = Arc::new(RemoteInbox::new(host.clone(), clock.clone()));
        (inbox, host, clock)
    }

    fn request(secret: &[u8; 32], id: &str, text: &str) -> Request {
        Request {
            v: VERSION,
            secret: BASE32_NOPAD.encode(secret),
            message_id: id.into(),
            text: text.into(),
        }
    }

    fn reason(response: &Response) -> Option<Reason> {
        match response.status {
            Status::Rejected { reason } => Some(reason),
            Status::Accepted { .. } => None,
        }
    }

    const NODE: &str = "node-a";

    fn node(name: &str) -> Sender {
        name.to_string()
    }

    #[test]
    fn a_first_message_is_accepted_and_consumes_the_secret() {
        let (inbox, host, _) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        assert!(inbox.has_live_ticket());
        let ok = inbox.handle(&node(NODE), request(&minted.secret, "m1", "hello"));
        assert_eq!(ok, Response::accepted("m1".into()));
        assert!(!inbox.has_live_ticket());
        let again = inbox.handle(&node(NODE), request(&minted.secret, "m2", "more"));
        assert_eq!(reason(&again), Some(Reason::Reused));
        let other = inbox.handle(&node("node-b"), request(&minted.secret, "m1", "hello"));
        assert_eq!(reason(&other), Some(Reason::Reused));
        assert_eq!(host.accepted.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_lost_ack_retry_returns_the_original_acceptance_without_a_second_enqueue() {
        let (inbox, host, clock) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        let first = inbox.handle(&node(NODE), request(&minted.secret, "m1", "hello"));
        let retry = inbox.handle(&node(NODE), request(&minted.secret, "m1", "hello"));
        assert_eq!(first, retry);
        assert_eq!(host.accepted.lock().unwrap().len(), 1);
        clock.0.store(2_000, Ordering::SeqCst);
        let late = inbox.handle(&node(NODE), request(&minted.secret, "m1", "hello"));
        assert_eq!(reason(&late), Some(Reason::Expired));
    }

    #[test]
    fn a_newer_ticket_revokes_the_older_one_and_a_late_retry_is_a_bad_ticket() {
        let (inbox, _, _) = setup();
        let old = inbox.mint(Duration::from_secs(60)).unwrap();
        inbox.handle(&node(NODE), request(&old.secret, "m1", "hi"));
        let new = inbox.mint(Duration::from_secs(60)).unwrap();
        let late = inbox.handle(&node(NODE), request(&old.secret, "m1", "hi"));
        assert_eq!(reason(&late), Some(Reason::BadTicket));
        let ok = inbox.handle(&node(NODE), request(&new.secret, "m2", "again"));
        assert!(reason(&ok).is_none());
    }

    #[test]
    fn a_wrong_secret_is_rejected_before_expiry_and_reuse_are_consulted() {
        let (inbox, _, clock) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        let wrong = [1u8; 32];
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&wrong, "m", "x"))),
            Some(Reason::BadTicket)
        );
        inbox.handle(&node(NODE), request(&minted.secret, "m1", "x"));
        assert_eq!(
            reason(&inbox.handle(&node("node-b"), request(&wrong, "m", "x"))),
            Some(Reason::BadTicket)
        );
        clock.0.store(5_000, Ordering::SeqCst);
        assert_eq!(
            reason(&inbox.handle(&node("node-b"), request(&wrong, "m", "x"))),
            Some(Reason::BadTicket)
        );
        assert_eq!(
            reason(&inbox.handle(&node("node-b"), request(&minted.secret, "m", "x"))),
            Some(Reason::Expired)
        );
    }

    #[test]
    fn five_bad_attempts_lock_that_node_alone_for_thirty_seconds() {
        let (inbox, _, clock) = setup();
        let minted = inbox.mint(Duration::from_secs(600)).unwrap();
        let wrong = [3u8; 32];
        for _ in 0..5 {
            inbox.handle(&node("bad"), request(&wrong, "m", "x"));
        }
        assert_eq!(
            reason(&inbox.handle(&node("bad"), request(&minted.secret, "m", "x"))),
            Some(Reason::Locked)
        );
        let ok = inbox.handle(&node("good"), request(&minted.secret, "m", "x"));
        assert!(reason(&ok).is_none());
        clock.0.fetch_add(31, Ordering::SeqCst);
        assert_eq!(
            reason(&inbox.handle(&node("bad"), request(&minted.secret, "m", "x"))),
            Some(Reason::Reused)
        );
    }

    #[test]
    fn the_lockout_map_is_capped() {
        let (inbox, _, _) = setup();
        inbox.mint(Duration::from_secs(60)).unwrap();
        for i in 0..(MAX_TRACKED_NODES + 50) {
            inbox.handle(&node(&format!("n{i}")), request(&[0; 32], "m", "x"));
        }
        assert!(inbox.state().attempts.len() <= MAX_TRACKED_NODES);
    }

    #[test]
    fn full_oversized_and_closed_do_not_consume_the_secret() {
        let (inbox, host, _) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        *host.cap.lock().unwrap() = Some(0);
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&minted.secret, "m1", "x"))),
            Some(Reason::InboxFull)
        );
        *host.cap.lock().unwrap() = None;
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&minted.secret, "m1", &big))),
            Some(Reason::TooLarge)
        );
        *host.closed.lock().unwrap() = true;
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&minted.secret, "m1", "x"))),
            Some(Reason::Closed)
        );
        *host.closed.lock().unwrap() = false;
        assert!(inbox.has_live_ticket());
        let ok = inbox.handle(&node(NODE), request(&minted.secret, "m1", "x"));
        assert!(reason(&ok).is_none());
        assert_eq!(host.rejected.lock().unwrap().len(), 3);
    }

    #[test]
    fn two_concurrent_first_requests_give_exactly_one_acceptance() {
        let (inbox, host, _) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let inbox = inbox.clone();
                let request = request(&minted.secret, &format!("m{i}"), "x");
                std::thread::spawn(move || inbox.handle(&format!("node-{i}"), request))
            })
            .collect();
        let responses: Vec<Response> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let accepted = responses.iter().filter(|r| reason(r).is_none()).count();
        assert_eq!(accepted, 1);
        assert!(
            responses
                .iter()
                .filter_map(reason)
                .all(|r| r == Reason::Reused)
        );
        assert_eq!(host.accepted.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_ticket_lifetime_is_capped_at_a_day() {
        let (inbox, _, _) = setup();
        assert_eq!(inbox.mint(Duration::ZERO).err(), Some(MintError::ZeroTtl));
        assert!(inbox.mint(MAX_TTL).is_ok());
        assert_eq!(
            inbox.mint(MAX_TTL + Duration::from_secs(1)).err(),
            Some(MintError::TtlTooLong)
        );
    }

    #[test]
    fn a_closed_inbox_accepts_nothing() {
        let (inbox, host, _) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        inbox.close();
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&minted.secret, "m", "x"))),
            Some(Reason::Closed)
        );
        assert!(!inbox.has_live_ticket());
        assert!(host.accepted.lock().unwrap().is_empty());
    }

    #[test]
    fn a_bad_version_or_message_id_is_refused() {
        let (inbox, _, _) = setup();
        let minted = inbox.mint(Duration::from_secs(60)).unwrap();
        let mut bad = request(&minted.secret, "m", "x");
        bad.v = 7;
        assert_eq!(
            reason(&inbox.handle(&node(NODE), bad)),
            Some(Reason::UnsupportedVersion)
        );
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&minted.secret, "", "x"))),
            Some(Reason::Malformed)
        );
        assert_eq!(
            reason(&inbox.handle(&node(NODE), request(&minted.secret, "m", " \n"))),
            Some(Reason::Malformed)
        );
        assert!(inbox.has_live_ticket());
    }
}
