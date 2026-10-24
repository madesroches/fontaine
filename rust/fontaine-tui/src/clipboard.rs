//! Putting text on the system clipboard from the terminal UI.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ratatui::crossterm::clipboard::CopyToClipboard;

/// How long a `tmux load-buffer` may take before it is given up on.
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// Copies `text` the way Claude Code does: inside tmux, through `tmux load-buffer -w -` first,
/// then with an OSC 52 escape to the terminal. tmux's default `set-clipboard external` drops an
/// application's OSC 52 but forwards a buffer loaded with `-w` to the outer terminal.
pub fn copy(text: &str) -> std::io::Result<()> {
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        // Without `-w` the text still lands in a tmux buffer, so prefix-] pastes it.
        if !tmux_load_buffer(&["load-buffer", "-w", "-"], text) {
            tmux_load_buffer(&["load-buffer", "-"], text);
        }
    }
    ratatui::crossterm::execute!(std::io::stdout(), CopyToClipboard::to_clipboard_from(text))
}

/// Runs `tmux <args>` with `text` on its stdin; whether it exited successfully in time. A tmux
/// that is missing, fails or hangs only means the OSC 52 escape is the one route left.
fn tmux_load_buffer(args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new("tmux")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    let deadline = Instant::now() + TMUX_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return written && status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}
