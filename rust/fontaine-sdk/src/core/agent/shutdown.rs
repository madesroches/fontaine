//! The session's final exit: cancel everything the session owns at once, then settle what can be
//! settled inside a budget the caller fixes. Session switching and headless runs keep their own
//! graceful teardown; nothing here changes `kill_all`, `terminate_all` or `shutdown_all`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::AgentRuntime;

/// How long an MCP server gets to leave when its stdin closes, before its process group is
/// signalled. A small slice of the shared budget, never a stage of its own.
const MCP_GRACEFUL_CLOSE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct Progress {
    begun: AtomicBool,
    finished: AtomicBool,
}

impl AgentRuntime {
    /// The first half of the final exit, synchronous and idempotent: no agent can start or wake,
    /// nested agent tasks are aborted, every background job's process group is killed, and MCP
    /// reconnects and watchers are retired. Nothing here waits, and no model, tool or job
    /// notification is produced.
    pub fn begin_final_shutdown(&self) {
        if self.final_shutdown.begun.swap(true, Ordering::SeqCst) {
            return;
        }
        self.state.agents().abort_all();
        self.setup.jobs.force_stop_all();
        self.mcp.retire_all();
    }

    /// The second half: the `session_end` hooks, the jobs' confirmation, the MCP connections,
    /// the language servers and the SearXNG instance the session owns, all at once and all under
    /// `deadline`; what has not finished by then is dropped, which signals the process groups it
    /// holds. Idempotent: a later call returns at once.
    pub async fn finish_final_shutdown(&self, deadline: Instant) {
        self.begin_final_shutdown();
        if self.final_shutdown.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let work = async {
            tokio::join!(
                self.fire_session_end("prompt_input_exit"),
                self.setup.jobs.wait_stopped(deadline),
                self.mcp.close_all_final(MCP_GRACEFUL_CLOSE),
                self.setup.lsp.terminate_all(),
                self.setup.searxng.terminate_owned(remaining / 2),
            );
        };
        let _ = tokio::time::timeout(remaining, work).await;
    }
}
