//! Stopping a child process that runs in its own process group: SIGTERM to the group, a grace
//! period, then SIGKILL. On Windows there is no group, so only the direct child is killed and a
//! wrapper that started the real program may leave it running.

use std::time::{Duration, Instant};

use tokio::process::Child;

pub const STOP_POLL: Duration = Duration::from_millis(50);

/// SIGTERM to the process group: a `docker run` CLI forwards it to the container, which SIGKILL
/// to the CLI alone could not.
pub fn terminate(pid: Option<u32>) {
    #[cfg(unix)]
    signal_group(pid, "TERM");
    #[cfg(not(unix))]
    let _ = pid;
}

/// Kills the group, even when the leader already exited (a wrapper may have left the server
/// running), and the child itself where there is no group to signal.
pub fn force_kill(child: &mut Child, pid: Option<u32>) {
    #[cfg(unix)]
    signal_group(pid, "KILL");
    #[cfg(not(unix))]
    let _ = pid;
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.start_kill();
    }
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: &str) {
    use std::process::Stdio;

    let Some(pid) = pid else { return };
    // Best effort: the group may already be gone.
    let _ = std::process::Command::new("kill")
        .args(["-s", signal, "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub async fn stop(child: &mut Child, pid: Option<u32>, grace: Duration) {
    terminate(pid);
    settle(child, pid, grace).await;
}

/// Waits for the exit that a SIGTERM already asked for, then kills what is left.
pub async fn settle(child: &mut Child, pid: Option<u32>, grace: Duration) {
    let deadline = Instant::now() + grace;
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        tokio::time::sleep(STOP_POLL).await;
    }
    force_kill(child, pid);
    let _ = child.wait().await;
}

/// [`settle`] without blocking the caller, which is a destructor: on the runtime when there is
/// one, on a thread when the runtime is already gone.
pub fn detached_settle(mut child: Child, pid: Option<u32>, grace: Duration) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            settle(&mut child, pid, grace).await;
        });
    } else {
        std::thread::spawn(move || {
            let deadline = Instant::now() + grace;
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(STOP_POLL);
            }
            force_kill(&mut child, pid);
            for _ in 0..20 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(STOP_POLL);
            }
        });
    }
}
