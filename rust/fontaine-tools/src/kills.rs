//! Kill switches: which agents a kill flag has reached, and the waits the tool choke point
//! selects on, so a killed agent's in-flight call ends where the gate can refuse it cleanly.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::watch;

use crate::lock::lock;

/// How long a killed child's in-flight run is graced to settle at a hook boundary before its
/// drive future is dropped exactly as an instantaneous kill drops it today.
pub const KILL_GRACE: Duration = Duration::from_secs(30);

/// How long the session exit path waits, after `kill_all`, for killed children to settle and
/// save their transcripts: [`KILL_GRACE`] plus slack for the settle to land. The wait is a
/// bound only — it returns as soon as everything has settled.
pub const KILL_SETTLE: Duration = Duration::from_secs(35);

/// The kill switches of one session's agents. The child runner registers each subagent's kill
/// receiver when it spawns; the root agent registers nothing and can never be killed here.
#[derive(Default)]
pub struct KillSwitches {
    inner: Mutex<HashMap<String, watch::Receiver<bool>>>,
}

impl KillSwitches {
    pub fn new() -> KillSwitches {
        KillSwitches::default()
    }

    /// Wires `agent`'s kill flag: a call may select on [`KillSwitches::wait`] and test
    /// [`KillSwitches::killed`].
    pub fn register(&self, agent: &str, kill: watch::Receiver<bool>) {
        lock(&self.inner, "kill switches").insert(agent.to_string(), kill);
    }

    /// Whether `agent` has been killed. An agent with no switch is never killed.
    pub fn killed(&self, agent: &str) -> bool {
        lock(&self.inner, "kill switches")
            .get(agent)
            .is_some_and(|kill| *kill.borrow())
    }

    /// A future that resolves once `agent` is killed, and never resolves for an agent with no
    /// switch (the root's calls wait on nothing).
    pub fn wait(&self, agent: &str) -> KillWait {
        let inner = lock(&self.inner, "kill switches").get(agent).map(|kill| {
            // The future owns its receiver clone: `wait_for` borrows it, and the wait
            // registration must live as long as the future, not one poll.
            let mut kill = kill.clone();
            KillWaitInner::Flag(Box::pin(async move {
                let _ = kill.wait_for(|killed| *killed).await;
            }) as futures::future::BoxFuture<'static, ()>)
        });
        KillWait {
            inner: inner.unwrap_or(KillWaitInner::None),
        }
    }
}

enum KillWaitInner {
    Flag(futures::future::BoxFuture<'static, ()>),
    None,
}

/// What [`KillSwitches::wait`] returns: resolves when the flag fires; pending forever for an
/// agent with no switch.
pub struct KillWait {
    inner: KillWaitInner,
}

impl Future for KillWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match &mut self.get_mut().inner {
            KillWaitInner::Flag(wait) => wait.as_mut().poll(cx),
            KillWaitInner::None => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::ROOT_AGENT;

    fn switch() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    #[test]
    fn an_agent_without_a_switch_is_never_killed() {
        let kills = KillSwitches::new();
        assert!(!kills.killed(ROOT_AGENT));
        assert!(!kills.killed("agent-1"));
    }

    #[test]
    fn register_and_lookup_track_the_flag() {
        let kills = KillSwitches::new();
        let (tx, rx) = switch();
        kills.register("agent-1", rx);
        assert!(!kills.killed("agent-1"));
        tx.send(true).unwrap();
        assert!(kills.killed("agent-1"));
        // Other agents keep their own state.
        assert!(!kills.killed("agent-2"));
    }

    #[test]
    fn killed_does_not_consume_the_flag() {
        let kills = KillSwitches::new();
        let (tx, rx) = switch();
        kills.register("agent-1", rx);
        tx.send(true).unwrap();
        assert!(kills.killed("agent-1"));
        assert!(kills.killed("agent-1"));
    }

    #[tokio::test]
    async fn wait_resolves_once_the_flag_fires() {
        let kills = KillSwitches::new();
        let (tx, rx) = switch();
        kills.register("agent-1", rx);
        let waiter = kills.wait("agent-1");
        tokio::pin!(waiter);
        assert!(futures::poll!(&mut waiter).is_pending());
        tx.send(true).unwrap();
        assert!(futures::poll!(&mut waiter).is_ready());
    }

    #[tokio::test]
    async fn wait_is_pending_for_an_unregistered_agent() {
        let kills = KillSwitches::new();
        let waiter = kills.wait(ROOT_AGENT);
        tokio::pin!(waiter);
        // Pending on every poll: an agent with no switch is never killed.
        assert!(futures::poll!(&mut waiter).is_pending());
        assert!(futures::poll!(&mut waiter).is_pending());
    }

    #[tokio::test]
    async fn a_closed_channel_completes_the_wait_without_the_flag() {
        let kills = KillSwitches::new();
        let (tx, rx) = switch();
        kills.register("agent-1", rx);
        drop(tx);
        // The senders are gone with the child's task: the flag can never fire, and no call
        // is left to cut short, so the wait completes without refusing anything.
        kills.wait("agent-1").await;
    }
}
