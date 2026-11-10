//! Stopping a child process and everything it started: SIGTERM to its process group, a grace
//! period, then SIGKILL. Callers spawn with `process_group(0)` on Unix. On Windows there is no
//! group, so the kill walks the tree with `taskkill /T /F`, which reaches descendants only while
//! the child itself is still running, and then closes a Job Object holding the child, which
//! terminates every process still in it, orphans of an exited leader included.

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
/// running). On Windows it kills the leader's whole tree, then the leader itself.
pub fn force_kill(child: &mut Child, pid: Option<u32>) {
    #[cfg(unix)]
    signal_group(pid, "KILL");
    let leader_running = !matches!(child.try_wait(), Ok(Some(_)));
    #[cfg(windows)]
    if leader_running {
        kill_tree(pid);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
    if leader_running {
        let _ = child.start_kill();
    }
}

/// A reaped pid may be reused, so callers only reach this while the leader is running.
#[cfg(windows)]
fn kill_tree(pid: Option<u32>) {
    use std::process::Stdio;

    let Some(pid) = pid else { return };
    // Best effort: the tree may already be gone.
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A Job Object that kills its processes when the handle closes, until [`Job::release`].
/// A child is assigned after it spawns, so a descendant started before that is not covered.
#[cfg(windows)]
#[derive(Debug)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a job handle is a kernel object reference usable from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}

#[cfg(windows)]
impl Job {
    fn new(child: &Child) -> Option<Job> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};

        let process = child.raw_handle()?;
        // SAFETY: plain FFI calls; the handle is closed on every path that does not return it.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let job = Job(job);
            if !job.set_kill_on_close(true) || AssignProcessToJobObject(job.0, process as _) == 0 {
                CloseHandle(job.0);
                std::mem::forget(job);
                return None;
            }
            Some(job)
        }
    }

    fn set_kill_on_close(&self, on: bool) -> bool {
        use windows_sys::Win32::System::JobObjects::{
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        };

        // SAFETY: an all-zero limit struct is valid; the pointer and size describe it.
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            if on {
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            }
            SetInformationJobObject(
                self.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
        }
    }

    /// Clears the kill flag before the handle closes, so daemons the command backgrounded on
    /// purpose outlive a run that finished.
    fn release(self) {
        if !self.set_kill_on_close(false) {
            // Closing now would kill them; leaking the handle until exit keeps them alive.
            std::mem::forget(self);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Owns a child whose whole tree must die if the owner is dropped before [`TreeGuard::disarm`],
/// as when a timeout cancels the future that holds it.
#[derive(Debug)]
pub struct TreeGuard {
    // Taken by `disarm` and `drop`.
    child: Option<Child>,
    // Recorded at spawn: tokio's `Child::id` is None once the leader has been reaped, but the
    // group may outlive it.
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<Job>,
}

impl TreeGuard {
    pub fn new(child: Child) -> Self {
        TreeGuard {
            pid: child.id(),
            #[cfg(windows)]
            job: Job::new(&child),
            child: Some(child),
        }
    }

    pub fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("the child is only taken by disarm and drop")
    }

    /// Gives the child back for a run that finished, so the drop does not kill it.
    pub fn disarm(mut self) -> Child {
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            job.release();
        }
        self.child
            .take()
            .expect("the child is only taken by disarm and drop")
    }
}

/// `Child::wait_with_output` on a borrowed child, so a [`TreeGuard`] keeps ownership and a
/// cancelled wait still kills the tree.
pub async fn collect_output(child: &mut Child) -> std::io::Result<std::process::Output> {
    use tokio::io::{AsyncRead, AsyncReadExt};

    async fn read_all<R: AsyncRead + Unpin>(pipe: Option<R>) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes).await?;
        }
        Ok(bytes)
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (stdout, stderr, status) = tokio::join!(read_all(stdout), read_all(stderr), child.wait());
    Ok(std::process::Output {
        status: status?,
        stdout: stdout?,
        stderr: stderr?,
    })
}

impl Drop for TreeGuard {
    /// The kill is synchronous, because it is what closes the pipes a descendant inherited;
    /// reaping runs detached so the dropping task does not block.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        force_kill(&mut child, self.pid);
        // Closing the job kills what taskkill could not reach.
        #[cfg(windows)]
        drop(self.job.take());
        detached_settle(child, self.pid, Duration::ZERO);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Stdio;

    fn alive(pid: &str) -> bool {
        #[cfg(unix)]
        return std::process::Command::new("kill")
            .args(["-0", pid])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        // `tasklist` exits 0 either way, so a live pid is told by its row in the output.
        #[cfg(windows)]
        return std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .any(|word| word == pid)
            });
    }

    async fn pid_in(path: &Path) -> String {
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(path) {
                let text = text.trim();
                if !text.is_empty() && text.parse::<u32>().is_ok() {
                    return text.to_string();
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no pid in {}", path.display());
    }

    #[tokio::test]
    async fn force_kill_takes_down_a_backgrounded_descendant_holding_the_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // MSYS `$!` is not a Windows pid, which `taskkill` cannot see. The subshell keeps the
        // sleeper from being exec'd in place, which MSYS reparents out of the leader's tree.
        let record = if cfg!(windows) {
            format!("cat /proc/$!/winpid > '{}'", pid_file.display())
        } else {
            format!("echo $! > '{}'", pid_file.display())
        };
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", &format!("(sleep 30; true) & {record}; wait")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let Ok(child) = command.spawn() else {
            eprintln!("skipped: no `sh` on PATH");
            return;
        };
        let mut guard = TreeGuard::new(child);
        let pid = pid_in(&pid_file).await;
        assert!(alive(&pid), "the sleeper runs before the kill");

        let leader = guard.pid;
        force_kill(guard.child(), leader);
        for _ in 0..100 {
            if !alive(&pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the backgrounded sleeper {pid} survived force_kill");
    }

    #[tokio::test]
    async fn a_disarmed_guard_leaves_the_child_alone() {
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(child) = command.spawn() else {
            eprintln!("skipped: no `sh` on PATH");
            return;
        };
        let mut child = TreeGuard::new(child).disarm();
        assert!(child.wait().await.unwrap().success());
    }
}
