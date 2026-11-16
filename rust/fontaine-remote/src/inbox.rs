//! The one-time pairing secret: how a ticket authorizes the first connection of a device, and the
//! per-node lockout that bad attempts feed. Authentication of a paired device is the connection's
//! node id; nothing here is a session credential.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use data_encoding::BASE32_NOPAD;
use sha2::{Digest, Sha256};

use crate::protocol::{MAX_TTL, Reason, Sender};

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
    /// Set by the first valid presentation: the node that consumed the secret.
    consumed_by: Option<Sender>,
}

#[derive(Default)]
struct State {
    live: Option<Live>,
    attempts: HashMap<Sender, Attempts>,
    closed: bool,
}

pub struct Pairing {
    state: Mutex<State>,
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

impl Pairing {
    pub fn new(clock: Arc<dyn Clock>) -> Pairing {
        Pairing {
            state: Mutex::new(State::default()),
            clock,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // The state feeds an authentication decision, so a poisoned lock is a bug to surface.
        self.state.lock().expect("the pairing lock is poisoned")
    }

    pub fn now_secs(&self) -> u64 {
        self.clock.now_secs()
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
        self.state().live = Some(Live {
            secret_hash: hash(&secret),
            expires_at,
            consumed_by: None,
        });
        Ok(Minted { secret, expires_at })
    }

    /// Whether a ticket could still start a pairing.
    pub fn has_live_ticket(&self) -> bool {
        let now = self.clock.now_secs();
        let state = self.state();
        !state.closed
            && state
                .live
                .as_ref()
                .is_some_and(|live| live.consumed_by.is_none() && now < live.expires_at)
    }

    /// Stops accepting: every later presentation is `Closed` and consumes nothing.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.live = None;
    }

    /// Checks a presented secret in the order secret, expiry, reuse, and consumes it on success.
    /// A valid secret authorizes nothing by itself: the host still asks its person.
    pub fn verify(&self, from: &Sender, presented: &str) -> Result<(), Reason> {
        let now = self.clock.now_secs();
        let mut guard = self.state();
        let state = &mut *guard;
        if state.closed {
            return Err(Reason::Closed);
        }
        if Self::locked(state, from, now) {
            return Err(Reason::Locked);
        }
        let presented = BASE32_NOPAD
            .decode(presented.to_uppercase().as_bytes())
            .ok()
            .map(|bytes| hash(&bytes));
        let matches = match (&state.live, presented) {
            (Some(live), Some(presented)) => constant_time_eq(&live.secret_hash, &presented),
            _ => false,
        };
        if !matches {
            record_bad_attempt(&mut state.attempts, from, now);
            return Err(Reason::BadTicket);
        }
        let live = state.live.as_mut().expect("a matching secret is live");
        if now >= live.expires_at {
            return Err(Reason::Expired);
        }
        if live.consumed_by.is_some() {
            return Err(Reason::Reused);
        }
        live.consumed_by = Some(from.clone());
        Ok(())
    }

    /// A resume from a node that is not paired: refused, and counted toward the lockout.
    pub fn unpaired(&self, from: &Sender) -> Reason {
        let now = self.clock.now_secs();
        let mut guard = self.state();
        let state = &mut *guard;
        if state.closed {
            return Reason::Closed;
        }
        if Self::locked(state, from, now) {
            return Reason::Locked;
        }
        record_bad_attempt(&mut state.attempts, from, now);
        Reason::NotPaired
    }

    fn locked(state: &State, from: &Sender, now: u64) -> bool {
        state
            .attempts
            .get(from)
            .is_some_and(|a| a.locked_until > now)
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

    fn setup() -> (Arc<Pairing>, Arc<TestClock>) {
        let clock = Arc::new(TestClock(AtomicU64::new(1_000)));
        (Arc::new(Pairing::new(clock.clone())), clock)
    }

    fn text(secret: &[u8; 32]) -> String {
        BASE32_NOPAD.encode(secret)
    }

    fn node(name: &str) -> Sender {
        name.to_string()
    }

    #[test]
    fn a_valid_secret_is_consumed_and_a_replay_is_reused() {
        let (pairing, _) = setup();
        let minted = pairing.mint(Duration::from_secs(60)).unwrap();
        assert!(pairing.has_live_ticket());
        assert_eq!(pairing.verify(&node("a"), &text(&minted.secret)), Ok(()));
        assert!(!pairing.has_live_ticket());
        assert_eq!(
            pairing.verify(&node("a"), &text(&minted.secret)),
            Err(Reason::Reused)
        );
        assert_eq!(
            pairing.verify(&node("b"), &text(&minted.secret)),
            Err(Reason::Reused)
        );
    }

    #[test]
    fn a_newer_ticket_revokes_the_older_one() {
        let (pairing, _) = setup();
        let old = pairing.mint(Duration::from_secs(60)).unwrap();
        let new = pairing.mint(Duration::from_secs(60)).unwrap();
        assert_eq!(
            pairing.verify(&node("a"), &text(&old.secret)),
            Err(Reason::BadTicket)
        );
        assert_eq!(pairing.verify(&node("a"), &text(&new.secret)), Ok(()));
    }

    #[test]
    fn a_wrong_secret_is_rejected_before_expiry_and_reuse_are_consulted() {
        let (pairing, clock) = setup();
        let minted = pairing.mint(Duration::from_secs(60)).unwrap();
        let wrong = text(&[1u8; 32]);
        assert_eq!(pairing.verify(&node("a"), &wrong), Err(Reason::BadTicket));
        pairing.verify(&node("a"), &text(&minted.secret)).unwrap();
        assert_eq!(pairing.verify(&node("b"), &wrong), Err(Reason::BadTicket));
        clock.0.store(5_000, Ordering::SeqCst);
        assert_eq!(pairing.verify(&node("b"), &wrong), Err(Reason::BadTicket));
        assert_eq!(
            pairing.verify(&node("b"), &text(&minted.secret)),
            Err(Reason::Expired)
        );
    }

    #[test]
    fn five_bad_attempts_lock_that_node_alone_for_thirty_seconds() {
        let (pairing, clock) = setup();
        let minted = pairing.mint(Duration::from_secs(600)).unwrap();
        for _ in 0..5 {
            let _ = pairing.verify(&node("bad"), &text(&[3u8; 32]));
        }
        assert_eq!(
            pairing.verify(&node("bad"), &text(&minted.secret)),
            Err(Reason::Locked)
        );
        assert_eq!(pairing.verify(&node("good"), &text(&minted.secret)), Ok(()));
        clock.0.fetch_add(31, Ordering::SeqCst);
        assert_eq!(
            pairing.verify(&node("bad"), &text(&minted.secret)),
            Err(Reason::Reused)
        );
    }

    #[test]
    fn an_unpaired_resume_is_refused_and_counts_toward_the_lockout() {
        let (pairing, _) = setup();
        let minted = pairing.mint(Duration::from_secs(600)).unwrap();
        for _ in 0..5 {
            assert_eq!(pairing.unpaired(&node("stranger")), Reason::NotPaired);
        }
        assert_eq!(pairing.unpaired(&node("stranger")), Reason::Locked);
        assert_eq!(
            pairing.verify(&node("stranger"), &text(&minted.secret)),
            Err(Reason::Locked),
            "the same lockout covers a pairing attempt"
        );
        assert_eq!(pairing.unpaired(&node("other")), Reason::NotPaired);
    }

    #[test]
    fn the_lockout_map_is_capped() {
        let (pairing, _) = setup();
        pairing.mint(Duration::from_secs(60)).unwrap();
        for i in 0..(MAX_TRACKED_NODES + 50) {
            let _ = pairing.verify(&node(&format!("n{i}")), &text(&[0; 32]));
        }
        assert!(pairing.state().attempts.len() <= MAX_TRACKED_NODES);
    }

    #[test]
    fn a_ticket_lifetime_is_capped_at_a_day() {
        let (pairing, _) = setup();
        assert_eq!(pairing.mint(Duration::ZERO).err(), Some(MintError::ZeroTtl));
        assert!(pairing.mint(MAX_TTL).is_ok());
        assert_eq!(
            pairing.mint(MAX_TTL + Duration::from_secs(1)).err(),
            Some(MintError::TtlTooLong)
        );
    }

    #[test]
    fn a_closed_gate_accepts_nothing() {
        let (pairing, _) = setup();
        let minted = pairing.mint(Duration::from_secs(60)).unwrap();
        pairing.close();
        assert_eq!(
            pairing.verify(&node("a"), &text(&minted.secret)),
            Err(Reason::Closed)
        );
        assert_eq!(pairing.unpaired(&node("a")), Reason::Closed);
        assert!(!pairing.has_live_ticket());
    }

    #[test]
    fn two_concurrent_first_presentations_give_exactly_one_success() {
        let (pairing, _) = setup();
        let minted = pairing.mint(Duration::from_secs(60)).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let pairing = pairing.clone();
                let secret = text(&minted.secret);
                std::thread::spawn(move || pairing.verify(&format!("node-{i}"), &secret))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .filter_map(|r| r.err())
                .all(|r| r == Reason::Reused)
        );
    }
}
