//! The final exit's clock: the options `drive` is given for it, and the watchdog that ends the
//! process at the deadline if the orderly teardown has not.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use fontaine_tools::lock::lock_recovering;

/// How long everything between a quit and the process ending may take, measured from the first
/// quit request. Fixed, not a setting.
pub const FINAL_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// What `drive` needs beyond the session itself.
pub struct DriveOptions {
    /// The whole-exit budget a final quit shares among every stage.
    pub budget: Duration,
    /// Called once, with the deadline, as a final exit starts: the production caller arms the
    /// process watchdog from it.
    pub on_quit: Option<Box<dyn FnMut(Instant)>>,
}

impl Default for DriveOptions {
    fn default() -> Self {
        DriveOptions {
            budget: FINAL_SHUTDOWN_BUDGET,
            on_quit: None,
        }
    }
}

/// Ends the process at a deadline unless disarmed first. Its thread does not depend on the
/// runtime, the screen or any lock the interface holds, so a stuck destructor or exporter
/// cannot keep the process, or a raw-mode terminal, alive.
#[must_use = "dropping the watchdog disarms it"]
pub struct ExitWatchdog {
    disarmed: Arc<(Mutex<bool>, Condvar)>,
    deadline: Instant,
}

impl ExitWatchdog {
    /// At `deadline`, calls `restore` and then `exit`, unless disarmed or dropped before.
    pub fn arm(
        deadline: Instant,
        restore: impl FnOnce() + Send + 'static,
        exit: impl FnOnce() + Send + 'static,
    ) -> ExitWatchdog {
        let disarmed = Arc::new((Mutex::new(false), Condvar::new()));
        let seen = disarmed.clone();
        // A failed spawn leaves the orderly teardown without its backstop; there is nothing
        // better to do at the moment the process is leaving.
        let _ = std::thread::Builder::new()
            .name("fontaine-exit-watchdog".to_owned())
            .spawn(move || {
                let (flag, wake) = &*seen;
                let mut done = lock_recovering(flag);
                while !*done {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        drop(done);
                        restore();
                        exit();
                        return;
                    }
                    done = wake
                        .wait_timeout(done, left)
                        .expect("nothing panics while holding the flag")
                        .0;
                }
            });
        ExitWatchdog { disarmed, deadline }
    }

    /// When the watchdog fires if nothing disarms it.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn disarm(self) {
        drop(self);
    }
}

impl Drop for ExitWatchdog {
    fn drop(&mut self) {
        let (flag, wake) = &*self.disarmed;
        // Only a bool the watchdog and this drop touch: a poisoned lock still holds it.
        *lock_recovering(flag) = true;
        wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    #[test]
    fn it_restores_the_terminal_then_exits_at_the_deadline() {
        let (tx, rx) = mpsc::channel();
        let (restore_tx, exit_tx) = (tx.clone(), tx);
        let started = Instant::now();
        let watchdog = ExitWatchdog::arm(
            started + Duration::from_millis(100),
            move || restore_tx.send("restore").unwrap(),
            move || exit_tx.send("exit").unwrap(),
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "restore");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "exit");
        assert!(started.elapsed() >= Duration::from_millis(100));
        std::mem::forget(watchdog);
    }

    #[test]
    fn disarming_prevents_a_later_firing() {
        let fired = Arc::new(AtomicUsize::new(0));
        let (a, b) = (fired.clone(), fired.clone());
        let watchdog = ExitWatchdog::arm(
            Instant::now() + Duration::from_millis(100),
            move || {
                a.fetch_add(1, Ordering::SeqCst);
            },
            move || {
                b.fetch_add(1, Ordering::SeqCst);
            },
        );
        watchdog.disarm();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dropping_disarms_too() {
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = fired.clone();
        drop(ExitWatchdog::arm(
            Instant::now() + Duration::from_millis(100),
            || {},
            move || {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        ));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }
}
