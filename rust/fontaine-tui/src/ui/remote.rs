//! The remote inbox in the interface loop: the `/remote-inbox` command, the wake-up that starts
//! or answers, the dialog hold, the idle drain of remote lines, and teardown. The terminal is
//! the only place the inbox can be enabled; a headless run never reaches any of this.

use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::audit::Decision;
use fontaine_core::config::RemoteRelay;
use fontaine_core::ledger::{MessageOrigin, RemoteSender};
use fontaine_core::steering::{DialogGuard, Retracted};
use fontaine_remote::{RelayMode, RemoteServer, SystemClock};
use fontaine_tools::error_chain::chain;

use crate::app::RemoteDialog;
use crate::remote::{RemoteCommand, RemoteHost, parse_remote_command};
use crate::ticket_prompt::TicketPrompt;
use crate::ui::turn::{Submission, group};
use crate::{Action, App, Input, Ui};

pub(crate) enum RemoteState {
    Off,
    /// The endpoint is being started on a task; its result waits in the host for the loop.
    Starting {
        host: Arc<RemoteHost>,
        ttl: Duration,
    },
    Running {
        host: Arc<RemoteHost>,
        server: RemoteServer,
    },
}

/// What the open remote-answerable dialog holds in the steering queue. It is replaced when the
/// dialog, or the answers it takes, change.
pub(crate) struct DialogHold {
    dialog: RemoteDialog,
    guard: DialogGuard,
}

fn lifetime_text(ttl: Duration) -> String {
    let secs = ttl.as_secs();
    if secs.is_multiple_of(3600) {
        format!("{} hour(s)", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{} minute(s)", secs / 60)
    } else {
        format!("{secs} second(s)")
    }
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/remote-inbox`. A remote line may turn the inbox off or ask its status; it cannot mint
    /// a ticket, whose only display is this terminal.
    pub(crate) fn remote_command(&mut self, runtime: &AgentRuntime, argument: &str) {
        let command = match parse_remote_command(argument) {
            Ok(command) => command,
            Err(usage) => {
                self.app.transcript.error(usage);
                return;
            }
        };
        match command {
            RemoteCommand::Status => self.show_remote_status(),
            RemoteCommand::Off => {
                if matches!(self.remote, RemoteState::Off) {
                    self.app
                        .transcript
                        .notice("the remote inbox is not enabled");
                } else {
                    self.remote_teardown(runtime);
                    self.app.transcript.notice("remote inbox off");
                }
            }
            RemoteCommand::On(_) | RemoteCommand::Ticket(_) if self.current_remote.is_some() => {
                self.app.transcript.error(
                    "a ticket is shown only on this terminal, so it cannot be minted from a remote line",
                );
            }
            RemoteCommand::On(ttl) => match self.remote {
                RemoteState::Off => self.start_remote(runtime, ttl),
                _ => self.show_remote_status(),
            },
            RemoteCommand::Ticket(ttl) => self.mint_remote_ticket(runtime, ttl),
        }
    }

    fn show_remote_status(&mut self) {
        let text = match &self.remote {
            RemoteState::Off => "the remote inbox is off; /remote-inbox on enables it".to_string(),
            RemoteState::Starting { .. } => "the remote inbox is starting".to_string(),
            RemoteState::Running { server, .. } => {
                if server.has_live_ticket() {
                    "the remote inbox is on; a ticket is live".to_string()
                } else {
                    "the remote inbox is on; no live ticket: /remote-inbox ticket mints one"
                        .to_string()
                }
            }
        };
        self.app.transcript.notice(text);
    }

    fn start_remote(&mut self, runtime: &AgentRuntime, ttl: Duration) {
        let Some(wake) = self.wake.clone() else {
            self.app
                .transcript
                .error("the remote inbox is not available in this terminal session");
            return;
        };
        let host = Arc::new(RemoteHost::new(
            runtime.steering.clone(),
            runtime.state.clone(),
            wake,
        ));
        host.set_skills(
            runtime
                .slash_skills()
                .into_iter()
                .map(|(name, ..)| name.to_string()),
        );
        let relay = match runtime.config.remote.relay {
            RemoteRelay::Default => RelayMode::Default,
            RemoteRelay::Disabled => RelayMode::Disabled,
        };
        self.remote = RemoteState::Starting {
            host: host.clone(),
            ttl,
        };
        self.app.transcript.notice("starting the remote inbox…");
        tokio::spawn(async move {
            let result = RemoteServer::start(host.clone(), Arc::new(SystemClock), relay)
                .await
                .map_err(|e| chain(&e));
            match result {
                // Switched off while it started: nothing waits for this endpoint.
                Ok(server) if host.is_closed() => server.shutdown().await,
                other => host.finish_start(other),
            }
        });
    }

    fn mint_remote_ticket(&mut self, runtime: &AgentRuntime, ttl: Duration) {
        match &self.remote {
            RemoteState::Off => self
                .app
                .transcript
                .error("the remote inbox is off; enable it first: /remote-inbox on"),
            RemoteState::Starting { .. } => self
                .app
                .transcript
                .notice("the remote inbox is still starting; try again in a moment"),
            RemoteState::Running { server, .. } => match server.mint_ticket(ttl) {
                Ok(ticket) => {
                    self.app
                        .begin_ticket(TicketPrompt::new(ticket.encode(), lifetime_text(ttl)));
                    runtime.state.record_remote_event(
                        "ticket_minted",
                        &format!(
                            "a ticket valid for {} was minted; any earlier one is revoked",
                            lifetime_text(ttl)
                        ),
                        Decision::Allow,
                    );
                }
                Err(e) => self.app.transcript.error(e.to_string()),
            },
        }
    }

    /// What a wake-up input means: the endpoint finished starting, or a remote line may answer
    /// the open dialog. Returns what the loop does next, like a key would.
    pub(crate) fn on_remote_wake(&mut self, runtime: &AgentRuntime) -> Action {
        self.finish_remote_start(runtime);
        self.take_remote_answer(runtime)
    }

    fn finish_remote_start(&mut self, runtime: &AgentRuntime) {
        let RemoteState::Starting { host, ttl } = &self.remote else {
            return;
        };
        let Some(result) = host.take_started() else {
            return;
        };
        let (host, ttl) = (host.clone(), *ttl);
        match result {
            Err(e) => {
                self.remote = RemoteState::Off;
                self.app
                    .transcript
                    .error(format!("could not start the remote inbox: {e}"));
            }
            Ok(server) => match server.mint_ticket(ttl) {
                Ok(ticket) => {
                    self.app
                        .begin_ticket(TicketPrompt::new(ticket.encode(), lifetime_text(ttl)));
                    runtime.state.record_remote_event(
                        "enabled",
                        &format!(
                            "the remote inbox was enabled; a ticket valid for {} was minted",
                            lifetime_text(ttl)
                        ),
                        Decision::Allow,
                    );
                    self.remote = RemoteState::Running { host, server };
                    self.app.transcript.notice(
                        "remote inbox on; the ticket is shown once, in the dialog (it is not kept in the transcript)",
                    );
                }
                Err(e) => {
                    host.close();
                    tokio::spawn(server.shutdown());
                    self.remote = RemoteState::Off;
                    self.app.transcript.error(e.to_string());
                }
            },
        }
    }

    /// Keeps what the status bar, the command classifier and the dialog hold know about the
    /// inbox in step with the session. Cheap and idempotent: every draw calls it.
    pub(crate) fn sync_remote(&mut self, runtime: &AgentRuntime) {
        // Level-triggered: a modal loop can swallow the one wake-up the start sends.
        self.finish_remote_start(runtime);
        self.app.remote_indicator = match &self.remote {
            RemoteState::Off => None,
            RemoteState::Starting { .. } => Some("inbox: starting"),
            RemoteState::Running { server, .. } => Some(if server.has_live_ticket() {
                "inbox: ticket live"
            } else {
                "inbox: no live ticket"
            }),
        };
        let wanted = match &self.remote {
            RemoteState::Running { host, .. } => {
                host.set_skills(
                    runtime
                        .slash_skills()
                        .into_iter()
                        .map(|(name, ..)| name.to_string()),
                );
                self.app.remote_dialog()
            }
            _ => None,
        };
        let held = self.dialog_guard.as_ref().map(|hold| &hold.dialog);
        if held == wanted.as_ref() {
            return;
        }
        self.dialog_guard = wanted.map(|dialog| {
            let guard = runtime.steering.open_dialog(&dialog.options);
            DialogHold { dialog, guard }
        });
    }

    /// Takes the remote line that answers the open dialog, if one arrived after it opened, and
    /// applies it as the person's pick would be applied.
    fn take_remote_answer(&mut self, runtime: &AgentRuntime) -> Action {
        self.sync_remote(runtime);
        let Some(hold) = &self.dialog_guard else {
            return Action::None;
        };
        let Some(line) = runtime.steering.take_dialog_answer(&hold.guard) else {
            return Action::None;
        };
        let kind = hold.dialog.kind;
        let from = line
            .remote
            .clone()
            .expect("a dialog answer is a remote line");
        runtime.state.record_remote_event(
            "dialog_answer",
            &format!(
                "message {} from {} answered the {} dialog",
                from.message_id,
                from.node,
                kind.name()
            ),
            Decision::Allow,
        );
        self.app.transcript.notice(format!(
            "remote line from {} answered the {} dialog",
            from.short_node(),
            kind.name()
        ));
        let action = self.app.answer_remote_dialog(kind, line.raw());
        if matches!(action, Action::GuardianOff(_) | Action::AfkReview(_)) {
            self.remote_confirm = Some(from);
        }
        self.sync_remote(runtime);
        action
    }

    /// At an idle prompt with nothing covering the input, runs what the remote inbox queued, in
    /// arrival order, the way typed lines run. A line a dialog holds is not ready yet.
    pub(crate) async fn remote_idle(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
    ) -> ControlFlow<()> {
        self.sync_remote(runtime);
        if self.app.covered() || !runtime.steering.has_remote_ready() {
            return ControlFlow::Continue(());
        }
        let submissions: Vec<Submission> = group(runtime.steering.retract());
        self.converse(
            runtime,
            launch_dir,
            VecDeque::from(submissions),
            // Never `Prompt`: the sender did not see the agent's last reply.
            MessageOrigin::Other,
        )
        .await
    }

    /// Writes the audit record for a remote line that never ran.
    pub(crate) fn audit_remote_drop(
        &mut self,
        runtime: &AgentRuntime,
        from: &RemoteSender,
        why: &str,
    ) {
        runtime.state.record_remote_event(
            "message_dropped",
            &format!(
                "message {} from {} was dropped: {why}",
                from.message_id, from.node
            ),
            Decision::Deny,
        );
    }

    /// Drops accepted remote entries that will not run: each gets an audit record, and the
    /// transcript says how many. Their senders were told `accepted`, which meant queued.
    pub(crate) fn drop_remote_entries(
        &mut self,
        runtime: &AgentRuntime,
        dropped: Vec<Retracted>,
        why: &str,
    ) {
        if dropped.is_empty() {
            return;
        }
        for entry in &dropped {
            if let Some(from) = &entry.remote {
                self.audit_remote_drop(runtime, from, why);
            }
        }
        self.app.transcript.notice(format!(
            "dropped {} queued remote message(s): {why}",
            dropped.len()
        ));
    }

    /// Takes the endpoint down at once: from here on a request is answered `Closed`, and the
    /// network side finishes closing on its own task. The ticket and what it opened are gone.
    pub(crate) fn remote_teardown(&mut self, runtime: &AgentRuntime) {
        self.dialog_guard = None;
        match std::mem::replace(&mut self.remote, RemoteState::Off) {
            RemoteState::Off => return,
            RemoteState::Starting { host, .. } => {
                host.close();
                // A start that finished after the last look would otherwise leak its endpoint.
                if let Some(Ok(server)) = host.take_started() {
                    tokio::spawn(server.shutdown());
                }
            }
            RemoteState::Running { host, server } => {
                host.close();
                tokio::spawn(server.shutdown());
            }
        }
        runtime.state.record_remote_event(
            "disabled",
            "the remote inbox was disabled",
            Decision::Allow,
        );
    }

    /// The end of a session, or a switch to another: the endpoint closes for good, and entries
    /// still queued are dropped instead of crossing into the next context.
    pub(crate) async fn remote_shutdown(&mut self, runtime: &AgentRuntime, why: &str) {
        self.dialog_guard = None;
        match std::mem::replace(&mut self.remote, RemoteState::Off) {
            RemoteState::Off => {}
            RemoteState::Starting { host, .. } => {
                host.close();
                if let Some(Ok(server)) = host.take_started() {
                    server.shutdown().await;
                }
            }
            RemoteState::Running { host, server } => {
                host.close();
                server.shutdown().await;
            }
        }
        let dropped = runtime.steering.drain_remote();
        self.drop_remote_entries(runtime, dropped, why);
    }

    /// `/clear`: the endpoint is torn down first, so nothing more is accepted, then the entries
    /// already queued are dropped with the context they were meant for.
    pub(crate) fn remote_clear(&mut self, runtime: &AgentRuntime) -> usize {
        self.remote_teardown(runtime);
        let dropped = runtime.steering.drain_remote();
        let count = dropped.len();
        for entry in &dropped {
            if let Some(from) = &entry.remote {
                self.audit_remote_drop(runtime, from, "the conversation was cleared");
            }
        }
        count
    }

    /// The input a loop awaiting a dialog reads: a wake-up may carry an answer.
    pub(crate) fn dialog_input(&mut self, runtime: &AgentRuntime, input: Input) -> Action {
        match input {
            Input::Remote => self.on_remote_wake(runtime),
            other => self.app.handle_input(other),
        }
    }
}
