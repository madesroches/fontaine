//! Remote control in the interface loop: the `/remote` command, the wake-up that starts the
//! endpoint or applies a device's answer, the pairing confirmation, the mirror of the loop's state
//! that devices render from, the idle drain of remote lines, and teardown. The terminal is the
//! only place remote control can be enabled; a headless run never reaches any of this.

use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fontaine_remote::{
    Clock, DeviceStore, Hub, HubLimits, Level, RelayMode, RemoteServer, ServerConfig, SystemClock,
};
use fontaine_sdk::core::agent::AgentRuntime;
use fontaine_sdk::core::audit::Decision;
use fontaine_sdk::core::config::RemoteRelay;
use fontaine_sdk::core::file_lock::{self, FileLock};
use fontaine_sdk::core::ledger::{MessageOrigin, RemoteSender};
use fontaine_sdk::core::notice::Severity;
use fontaine_sdk::core::steering::Retracted;
use fontaine_sdk::tools::error_chain::chain;

use crate::remote::surface::{InputWaker, PairingRequest};
use crate::remote::{
    DeviceInput, FileStore, MirrorTracker, RemoteCommand, RemoteSurface, parse_remote_command,
};
use crate::ticket_prompt::TicketPrompt;
use crate::ui::turn::{Submission, group};
use crate::{Action, App, Input, Ui};

pub(crate) struct Running {
    surface: Arc<RemoteSurface>,
    server: RemoteServer,
    /// Held while remote control is on, so one session at a time is the remote host.
    lock: FileLock,
    store: Arc<FileStore>,
    mirror: MirrorTracker,
    /// The pairing the open confirmation dialog stands for.
    pairing: Option<PairingRequest>,
    interrupt: bool,
}

pub(crate) enum RemoteState {
    Off,
    /// The endpoint is being started on a task; its result waits in the surface for the loop.
    Starting {
        surface: Arc<RemoteSurface>,
        ttl: Duration,
        lock: FileLock,
        store: Arc<FileStore>,
    },
    Running(Box<Running>),
}

/// A device interrupt stored while idle or in a modal loop would cancel a later, unrelated turn.
fn stored_interrupt(stored: bool, turn_running: bool) -> bool {
    stored || turn_running
}

/// Endpoints whose shutdown task still holds the host lock.
static STOPPING: AtomicUsize = AtomicUsize::new(0);

/// The lock is released only once the endpoint has closed, so a quick `/remote on` has to be told
/// to wait instead of blaming another session.
fn shut_down_in_background(server: RemoteServer, lock: FileLock) {
    STOPPING.fetch_add(1, Ordering::SeqCst);
    tokio::spawn(async move {
        server.shutdown().await;
        drop(lock);
        STOPPING.fetch_sub(1, Ordering::SeqCst);
    });
}

/// A start still in flight shuts its own rejected endpoint down; the lock waits for that.
fn hold_lock_until_start_ends(surface: &RemoteSurface, lock: FileLock) {
    STOPPING.fetch_add(1, Ordering::SeqCst);
    surface.after_start_ends(Box::new(move || {
        drop(lock);
        STOPPING.fetch_sub(1, Ordering::SeqCst);
    }));
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

const AFK_ON_NOTICE: &str = "AFK mode is on: approvals and questions are being answered unattended on the host, so nothing will ask this device";
const AFK_OFF_NOTICE: &str = "AFK mode is off: approvals and questions are asked again";

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/remote`. Enabling and minting a ticket are for the terminal only; a command that came in
    /// as a remote line is refused here as well as at the door.
    pub(crate) fn remote_command(&mut self, runtime: &AgentRuntime, argument: &str) {
        if self.current_remote.is_some() {
            self.app
                .transcript
                .error("remote control can only be managed at the host terminal");
            return;
        }
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
                    self.app.transcript.notice("remote control is not enabled");
                } else {
                    self.remote_teardown(runtime);
                    self.app.transcript.notice("remote control off");
                }
            }
            RemoteCommand::On(ttl) => match self.remote {
                RemoteState::Off => self.start_remote(runtime, ttl),
                _ => self.show_remote_status(),
            },
            RemoteCommand::Ticket(ttl) => self.mint_remote_ticket(runtime, ttl),
            RemoteCommand::Devices => self.list_devices(),
            RemoteCommand::Revoke(selector) => self.revoke_device(runtime, &selector),
        }
    }

    fn show_remote_status(&mut self) {
        let text = match &self.remote {
            RemoteState::Off => "remote control is off; /remote on enables it".to_string(),
            RemoteState::Starting { .. } => "remote control is starting".to_string(),
            RemoteState::Running(running) => {
                let devices = running.surface.hub().device_count();
                let ticket = if running.server.has_live_ticket() {
                    "a ticket is live"
                } else {
                    "no live ticket: /remote ticket mints one"
                };
                format!("remote control is on; {devices} device(s) attached; {ticket}")
            }
        };
        self.app.transcript.notice(text);
    }

    fn store_for_devices(&self) -> Arc<FileStore> {
        match &self.remote {
            RemoteState::Running(running) => running.store.clone(),
            RemoteState::Starting { store, .. } => store.clone(),
            RemoteState::Off => Arc::new(FileStore::new(&self.data_dir)),
        }
    }

    fn list_devices(&mut self) {
        let store = self.store_for_devices();
        let attached: Vec<String> = match &self.remote {
            RemoteState::Running(running) => running
                .surface
                .hub()
                .devices()
                .into_iter()
                .map(|(node, _)| node)
                .collect(),
            _ => Vec::new(),
        };
        match store.devices() {
            Err(e) => self
                .app
                .transcript
                .error(format!("could not read the paired devices: {}", chain(&e))),
            Ok(list) if list.devices.is_empty() => {
                self.app.transcript.notice("no devices are paired");
            }
            Ok(list) => {
                let rows: Vec<String> = list
                    .devices
                    .iter()
                    .map(|device| {
                        format!(
                            "{} · {} · paired {} · last seen {}{}",
                            device.label,
                            fontaine_remote::short_node(&device.node),
                            when(device.paired_at),
                            when(device.last_seen),
                            if attached.contains(&device.node) {
                                " · attached"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect();
                self.app
                    .transcript
                    .notice(format!("paired devices:\n{}", rows.join("\n")));
            }
        }
    }

    fn revoke_device(&mut self, runtime: &AgentRuntime, selector: &str) {
        let store = self.store_for_devices();
        let mut outcome = None;
        if let Err(e) = store.update(&mut |list| outcome = Some(list.revoke(selector))) {
            self.app.transcript.error(format!(
                "could not update the paired devices: {}",
                chain(&e)
            ));
            return;
        }
        match outcome {
            Some(Ok(device)) => {
                let live = match &self.remote {
                    RemoteState::Running(running) => running.server.revoke(&device.node),
                    _ => false,
                };
                runtime.state.record_remote_event(
                    "device_revoked",
                    &format!("device {} (\"{}\") was revoked", device.node, device.label),
                    Decision::Allow,
                );
                self.app.transcript.notice(format!(
                    "revoked {} ({}){}",
                    device.label,
                    fontaine_remote::short_node(&device.node),
                    if live {
                        "; its connection was closed"
                    } else {
                        ""
                    }
                ));
            }
            Some(Err(e)) => self.app.transcript.error(format!("cannot revoke: {e}")),
            None => {}
        }
    }

    /// Enables remote control: takes the host lock, then starts the endpoint on a task.
    pub(crate) fn start_remote(&mut self, runtime: &AgentRuntime, ttl: Duration) {
        let Some(wake) = self.wake.clone() else {
            self.app
                .transcript
                .error("remote control is not available in this terminal session");
            return;
        };
        let store = Arc::new(FileStore::new(&self.data_dir));
        if let Err(e) = crate::remote::ensure_dir(&self.data_dir) {
            self.app.transcript.error(format!(
                "could not prepare the data directory: {}",
                chain(&e)
            ));
            return;
        }
        let lock = match file_lock::try_lock(&store.host_lock_path()) {
            Ok(Some(lock)) => lock,
            Ok(None) if STOPPING.load(Ordering::SeqCst) > 0 => {
                self.app
                    .transcript
                    .error("remote control is still shutting down; try again in a moment");
                return;
            }
            Ok(None) => {
                self.app
                    .transcript
                    .error("another session is the remote host; turn it off there first");
                return;
            }
            Err(e) => {
                self.app.transcript.error(format!(
                    "could not take the remote host lock: {}",
                    chain(&e)
                ));
                return;
            }
        };
        let hub = Hub::new(HubLimits::default(), Arc::new(InputWaker(wake.clone())));
        let surface = Arc::new(RemoteSurface::new(
            runtime.steering.clone(),
            runtime.state.clone(),
            wake,
            hub.clone(),
        ));
        surface.set_skills(
            runtime
                .slash_skills()
                .into_iter()
                .map(|(name, ..)| name.to_string()),
        );
        let relay = match runtime.config.remote.relay {
            RemoteRelay::Default => RelayMode::Default,
            RemoteRelay::Disabled => RelayMode::Disabled,
        };
        let config = ServerConfig::new(
            store.clone(),
            surface.clone(),
            hub,
            Arc::new(SystemClock),
            relay,
        );
        self.remote = RemoteState::Starting {
            surface: surface.clone(),
            ttl,
            lock,
            store,
        };
        self.app.transcript.notice("starting remote control…");
        tokio::spawn(async move {
            let result = RemoteServer::start(config).await.map_err(|e| chain(&e));
            surface.finish_start(result).await;
        });
    }

    fn mint_remote_ticket(&mut self, runtime: &AgentRuntime, ttl: Duration) {
        match &self.remote {
            RemoteState::Off => self
                .app
                .transcript
                .error("remote control is off; enable it first: /remote on"),
            RemoteState::Starting { .. } => self
                .app
                .transcript
                .notice("remote control is still starting; try again in a moment"),
            RemoteState::Running(running) => match running.server.mint_ticket(ttl) {
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

    /// What a wake-up input means: the endpoint finished starting, a device answered or
    /// interrupted, or a pairing waits. Returns what the loop does next, like a key would.
    pub(crate) fn on_remote_wake(&mut self, runtime: &AgentRuntime) -> Action {
        self.sync_remote(runtime);
        let RemoteState::Running(running) = &mut self.remote else {
            return Action::None;
        };
        let interrupt = std::mem::take(&mut running.interrupt);
        if interrupt && self.app.running {
            Action::Interrupt
        } else {
            Action::None
        }
    }

    fn finish_remote_start(&mut self, runtime: &AgentRuntime) {
        let RemoteState::Starting { surface, .. } = &self.remote else {
            return;
        };
        let Some(result) = surface.take_started() else {
            return;
        };
        let RemoteState::Starting {
            surface,
            ttl,
            lock,
            store,
        } = std::mem::replace(&mut self.remote, RemoteState::Off)
        else {
            return;
        };
        match result {
            Err(e) => {
                self.app
                    .transcript
                    .error(format!("could not start remote control: {e}"));
            }
            Ok(server) => match server.mint_ticket(ttl) {
                Ok(ticket) => {
                    self.app
                        .begin_ticket(TicketPrompt::new(ticket.encode(), lifetime_text(ttl)));
                    runtime.state.record_remote_event(
                        "enabled",
                        &format!(
                            "remote control was enabled; a ticket valid for {} was minted",
                            lifetime_text(ttl)
                        ),
                        Decision::Allow,
                    );
                    self.remote = RemoteState::Running(Box::new(Running {
                        surface,
                        server,
                        lock,
                        store,
                        mirror: MirrorTracker::new(self.app.afk),
                        pairing: None,
                        interrupt: false,
                    }));
                    self.app.transcript.notice(
                        "remote control on; the ticket is shown once, in the dialog (it is not kept in the transcript)",
                    );
                }
                Err(e) => {
                    surface.close();
                    shut_down_in_background(server, lock);
                    self.app.transcript.error(e.to_string());
                }
            },
        }
    }

    /// Keeps what the status bar, the command classifier and the pairing dialog know about remote
    /// control in step with the session, and applies what devices asked. Cheap and idempotent:
    /// every draw calls it.
    pub(crate) fn sync_remote(&mut self, runtime: &AgentRuntime) {
        // Level-triggered: a modal loop can swallow the one wake-up the start sends.
        self.finish_remote_start(runtime);
        self.app.remote_indicator = match &self.remote {
            RemoteState::Off => None,
            RemoteState::Starting { .. } => Some("remote: starting".to_string()),
            RemoteState::Running(running) => {
                let devices = running.surface.hub().device_count();
                Some(if devices > 0 {
                    format!("remote: {devices} device(s)")
                } else if running.server.has_live_ticket() {
                    "remote: ticket live".to_string()
                } else {
                    "remote: no live ticket".to_string()
                })
            }
        };
        let RemoteState::Running(running) = &mut self.remote else {
            return;
        };
        running.surface.set_skills(
            runtime
                .slash_skills()
                .into_iter()
                .map(|(name, ..)| name.to_string()),
        );
        for (level, text) in running.surface.take_notices() {
            match level {
                Level::Info => self.app.transcript.notice(text),
                Level::Warning => {
                    self.app
                        .transcript
                        .push_notice(fontaine_sdk::core::notice::Notice {
                            severity: Severity::Warning,
                            headline: text,
                            hint: None,
                        })
                }
            }
        }
        for input in running.surface.take_inputs() {
            match input {
                DeviceInput::Interrupt { from } => {
                    running.interrupt = stored_interrupt(running.interrupt, self.app.running);
                    running.surface.audit(
                        "interrupt",
                        &format!("device {from} interrupted the turn"),
                        Decision::Allow,
                    );
                }
                DeviceInput::Answer { by, id, answer } => {
                    apply_answer(&mut self.app, running, by, id, answer);
                }
            }
        }
        // The device that asked went away: its confirmation closes with no reply.
        if running
            .pairing
            .as_ref()
            .is_some_and(|pairing| pairing.reply.is_closed())
        {
            let gone = running.pairing.take().expect("checked above");
            self.app.pairing_prompt = None;
            running.surface.audit(
                "pairing_cancelled",
                &format!("device {} left before the pairing was confirmed", gone.node),
                Decision::Deny,
            );
        }
        // Only when nothing else holds the keyboard, so the confirmation is never answered by a
        // key meant for another dialog.
        if running.pairing.is_none()
            && !self.app.covered()
            && let Some(request) = running.surface.take_pairing()
        {
            self.app.begin_pairing(crate::guardian_prompt::pairing(
                &fontaine_remote::short_node(&request.node),
                &request.label,
            ));
            running.pairing = Some(request);
        }
    }

    /// The person answered the pairing confirmation at this terminal.
    pub(crate) fn answer_pairing(&mut self, runtime: &AgentRuntime, answer: bool) {
        let RemoteState::Running(running) = &mut self.remote else {
            return;
        };
        let Some(request) = running.pairing.take() else {
            return;
        };
        if request.reply.is_closed() {
            runtime.state.record_remote_event(
                "pairing_cancelled",
                &format!(
                    "device {} left or timed out before the pairing was confirmed",
                    request.node
                ),
                Decision::Deny,
            );
            self.app.transcript.notice(format!(
                "device {} already left or timed out; nothing was paired",
                fontaine_remote::short_node(&request.node)
            ));
            return;
        }
        runtime.state.record_remote_event(
            if answer {
                "pairing_confirmed"
            } else {
                "pairing_declined"
            },
            &format!(
                "device {} (\"{}\") was {} at the host terminal",
                request.node,
                request.label,
                if answer { "confirmed" } else { "declined" }
            ),
            if answer {
                Decision::Allow
            } else {
                Decision::Deny
            },
        );
        let _ = request.reply.send(answer);
    }

    /// Publishes what changed since the last draw to every attached device, and gives a device
    /// that needs one the whole state. Called with the app complete, just before it renders.
    pub(crate) fn publish_remote(&mut self) {
        let RemoteState::Running(running) = &mut self.remote else {
            return;
        };
        let hub = running.surface.hub().clone();
        if running.mirror.afk_told != self.app.afk {
            running.mirror.afk_told = self.app.afk;
            hub.notice(
                Level::Info,
                if self.app.afk {
                    AFK_ON_NOTICE
                } else {
                    AFK_OFF_NOTICE
                },
            );
        }
        for event in running.mirror.diff(&self.app) {
            hub.publish(serde_json::to_value(&event).expect("a remote event serializes"));
        }
        if hub.snapshot_wanted() {
            let snapshot =
                serde_json::to_string(&running.mirror.snapshot()).expect("a snapshot serializes");
            hub.provide_snapshot(&snapshot);
        }
    }

    /// At an idle prompt with nothing covering the input, runs what devices queued, in arrival
    /// order, the way typed lines run.
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

    /// Takes the endpoint down at once: devices are told goodbye, a request is answered `Closed`,
    /// and the network side finishes closing on its own task. The ticket and what it opened are
    /// gone; paired devices stay in the file and reattach after the next enable.
    pub(crate) fn remote_teardown(&mut self, runtime: &AgentRuntime) {
        match std::mem::replace(&mut self.remote, RemoteState::Off) {
            RemoteState::Off => return,
            RemoteState::Starting { surface, lock, .. } => {
                surface.close();
                // A start that finished after the last look would otherwise leak its endpoint.
                match surface.take_started() {
                    Some(Ok(server)) => shut_down_in_background(server, lock),
                    Some(Err(_)) => {}
                    None => hold_lock_until_start_ends(&surface, lock),
                }
            }
            RemoteState::Running(running) => {
                let Running {
                    surface,
                    server,
                    lock,
                    ..
                } = *running;
                // Dropping the sender of a pairing still waiting declines it.
                surface.close();
                shut_down_in_background(server, lock);
            }
        }
        self.app.pairing_prompt = None;
        self.record_remote_disabled(runtime);
    }

    fn record_remote_disabled(&self, runtime: &AgentRuntime) {
        runtime.state.record_remote_event(
            "disabled",
            "remote control was disabled",
            Decision::Allow,
        );
    }

    /// The end of a session, or a switch to another: the endpoint closes for good, and entries
    /// still queued are dropped instead of crossing into the next context.
    pub(crate) async fn remote_shutdown(&mut self, runtime: &AgentRuntime, why: &str) {
        self.app.pairing_prompt = None;
        match std::mem::replace(&mut self.remote, RemoteState::Off) {
            RemoteState::Off => {}
            RemoteState::Starting { surface, lock, .. } => {
                surface.close();
                match surface.take_started() {
                    Some(Ok(server)) => {
                        server.shutdown().await;
                        drop(lock);
                    }
                    Some(Err(_)) => drop(lock),
                    None => hold_lock_until_start_ends(&surface, lock),
                }
                self.record_remote_disabled(runtime);
            }
            RemoteState::Running(running) => {
                let Running {
                    surface,
                    server,
                    lock,
                    ..
                } = *running;
                surface.close();
                server.shutdown().await;
                drop(lock);
                self.record_remote_disabled(runtime);
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

    /// The input a loop awaiting a dialog reads: a wake-up may carry a device's answer, and the
    /// pairing confirmation is answered here, wherever the loop that read the key runs.
    pub(crate) fn dialog_input(&mut self, runtime: &AgentRuntime, input: Input) -> Action {
        match input {
            Input::Remote => self.on_remote_wake(runtime),
            other => match self.app.handle_input(other) {
                Action::Pairing(answer) => {
                    self.answer_pairing(runtime, answer);
                    Action::None
                }
                action => action,
            },
        }
    }
}

fn when(secs: u64) -> String {
    let now = SystemClock.now_secs();
    let ago = now.saturating_sub(secs);
    match ago {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", ago / 60),
        3600..=86_399 => format!("{}h ago", ago / 3600),
        _ => format!("{}d ago", ago / 86_400),
    }
}

/// Applies a device's answer to the open dialog; a refusal is told to that device and audited,
/// and the dialog stays as it was.
fn apply_answer(
    app: &mut App,
    running: &Running,
    by: RemoteSender,
    id: u64,
    answer: crate::app::DeviceAnswer,
) {
    let node = by.node.clone();
    match app.device_answer(id, answer, by) {
        Ok(()) => running.surface.audit(
            "dialog_answer",
            &format!("device {node} answered dialog {id}"),
            Decision::Allow,
        ),
        Err(refusal) => {
            running.surface.audit(
                "answer_refused",
                &format!(
                    "device {node} answered dialog {id}, refused: {}",
                    refusal.text()
                ),
                Decision::Deny,
            );
            running
                .surface
                .hub()
                .notice_to(&node, Level::Warning, refusal.text());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stored_interrupt;

    #[test]
    fn a_device_interrupt_outside_a_turn_is_not_kept() {
        assert!(!stored_interrupt(false, false));
        assert!(stored_interrupt(false, true));
        assert!(stored_interrupt(true, false));
    }
}
