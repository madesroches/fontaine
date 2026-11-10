//! Shared by the tests that time out a command whose backgrounded descendant must die.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// A shell command that backgrounds a 30 s sleeper, records its pid in `pid_file` and waits.
/// The subshell keeps the sleeper a descendant on MSYS, where a lone `sleep` is exec'd in place
/// of the shell; MSYS `$!` is not a Windows pid, so Windows records `winpid`.
pub fn sleeper_command(pid_file: &Path) -> String {
    let record = if cfg!(windows) {
        format!("cat /proc/$!/winpid > '{}'", pid_file.display())
    } else {
        format!("echo $! > '{}'", pid_file.display())
    };
    format!("(sleep 30; true) & {record}; wait")
}

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

/// Panics unless the pid recorded in `pid_file` is gone within a few seconds.
pub async fn assert_gone(pid_file: &Path) {
    let pid = std::fs::read_to_string(pid_file).expect("the command recorded the sleeper's pid");
    let pid = pid.trim();
    for _ in 0..100 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the backgrounded sleeper {pid} survived the timeout");
}
