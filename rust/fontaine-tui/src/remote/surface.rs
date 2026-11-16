//! The session's side of an attach: what the network tasks reach the interface loop through. It
//! queues a device's prompts and commands as remote-tagged steering entries, parks its answers,
//! interrupts and pairing requests for the loop to apply, and audits everything a device does.
//! Nothing here touches the `App`: the loop owns it.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fontaine_remote::{
    BoxFuture, ClientMsg, Handled, HostSession, Hub, Level, Reason, RemoteServer, Sender, Waker,
};
use fontaine_sdk::core::audit::Decision;
use fontaine_sdk::core::gate::SessionState;
use fontaine_sdk::core::ledger::RemoteSender;
use fontaine_sdk::core::steering::{Queued, Steering};
use fontaine_sdk::tools::lock::lock;
use tokio::sync::mpsc::WeakUnboundedSender;
use tokio::sync::oneshot;

use crate::Input;
use crate::app::DeviceAnswer;
use crate::commands::{Command, parse_command};

/// What a device asked of the loop beyond queueing a line.
pub(crate) enum DeviceInput {
    Answer {
        by: RemoteSender,
        id: u64,
        answer: DeviceAnswer,
    },
    Interrupt {
        from: Sender,
    },
}

/// A device that presented a valid secret and waits for the person at the host to confirm.
pub(crate) struct PairingRequest {
    pub node: Sender,
    pub label: String,
    pub reply: oneshot::Sender<bool>,
}

/// Wakes the loop when the hub has a device waiting for a snapshot.
pub(crate) struct InputWaker(pub WeakUnboundedSender<Input>);

impl Waker for InputWaker {
    fn wake(&self) {
        if let Some(tx) = self.0.upgrade() {
            let _ = tx.send(Input::Remote);
        }
    }
}

struct WakeOnDrop(WeakUnboundedSender<Input>);

impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        InputWaker(self.0.clone()).wake();
    }
}

pub(crate) struct RemoteSurface {
    steering: Steering,
    state: Arc<SessionState>,
    wake: WeakUnboundedSender<Input>,
    hub: Arc<Hub>,
    skills: Mutex<HashSet<String>>,
    closed: AtomicBool,
    inputs: Mutex<VecDeque<DeviceInput>>,
    pairings: Mutex<VecDeque<PairingRequest>>,
    notices: Mutex<VecDeque<(Level, String)>>,
    /// Where the endpoint start leaves its result for the interface loop to pick up.
    started: Mutex<Option<Result<RemoteServer, String>>>,
}

impl RemoteSurface {
    pub(crate) fn new(
        steering: Steering,
        state: Arc<SessionState>,
        wake: WeakUnboundedSender<Input>,
        hub: Arc<Hub>,
    ) -> RemoteSurface {
        RemoteSurface {
            steering,
            state,
            wake,
            hub,
            skills: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            inputs: Mutex::new(VecDeque::new()),
            pairings: Mutex::new(VecDeque::new()),
            notices: Mutex::new(VecDeque::new()),
            started: Mutex::new(None),
        }
    }

    pub(crate) fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// From here on a device is refused and nothing is queued.
    pub(crate) fn close(&self) {
        // Under the `started` lock so a start finishing concurrently sees either the close or
        // its stored result, and never a server stored after the final `take_started`.
        let _started = lock(&self.started, "remote start slot");
        self.closed.store(true, Ordering::SeqCst);
        self.hub.close_all(fontaine_remote::Close::Bye);
        lock(&self.pairings, "pairing queue").clear();
        lock(&self.inputs, "device inputs").clear();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn set_skills(&self, names: impl IntoIterator<Item = String>) {
        *lock(&self.skills, "remote skill list") = names.into_iter().collect();
    }

    pub(crate) fn wake(&self) {
        if let Some(tx) = self.wake.upgrade() {
            let _ = tx.send(Input::Remote);
        }
    }

    /// Leaves the start's result and wakes the loop; the loop takes it with `take_started`.
    pub(crate) async fn finish_start(&self, result: Result<RemoteServer, String>) {
        let rejected = {
            let mut started = lock(&self.started, "remote start slot");
            if self.is_closed() {
                // Switched off while it started: nothing waits for this endpoint.
                result.ok()
            } else {
                *started = Some(result);
                None
            }
        };
        match rejected {
            Some(server) => server.shutdown().await,
            None => self.wake(),
        }
    }

    pub(crate) fn take_started(&self) -> Option<Result<RemoteServer, String>> {
        lock(&self.started, "remote start slot").take()
    }

    pub(crate) fn audit(&self, rule: &str, explanation: &str, decision: Decision) {
        self.state.record_remote_event(rule, explanation, decision);
    }

    pub(crate) fn take_inputs(&self) -> Vec<DeviceInput> {
        lock(&self.inputs, "device inputs").drain(..).collect()
    }

    pub(crate) fn take_pairing(&self) -> Option<PairingRequest> {
        let mut queue = lock(&self.pairings, "pairing queue");
        // One whose device went away has nothing left to ask.
        while let Some(front) = queue.pop_front() {
            if !front.reply.is_closed() {
                return Some(front);
            }
            self.audit(
                "pairing_cancelled",
                &format!(
                    "device {} left before the pairing was confirmed",
                    front.node
                ),
                Decision::Deny,
            );
        }
        None
    }

    pub(crate) fn take_notices(&self) -> Vec<(Level, String)> {
        lock(&self.notices, "remote notices").drain(..).collect()
    }

    fn note(&self, severity: Level, text: String) {
        lock(&self.notices, "remote notices").push_back((severity, text));
        self.wake();
    }

    fn refused_command(&self, from: &Sender, line: &str) -> Handled {
        let name = line.split_whitespace().next().unwrap_or("/");
        self.audit(
            "command_refused",
            &format!("device {from} sent {name}, which only the host terminal may run"),
            Decision::Deny,
        );
        Handled::Notice {
            severity: Level::Warning,
            text: format!("{name} can only be run at the host terminal"),
        }
    }
}

fn short(node: &str) -> String {
    node.chars().take(10).collect()
}

impl HostSession for RemoteSurface {
    fn client_message(&self, from: &Sender, _label: &str, message: ClientMsg) -> Handled {
        if self.is_closed() {
            return Handled::Notice {
                severity: Level::Warning,
                text: "remote control was turned off".to_string(),
            };
        }
        let sender = |id: &str| RemoteSender {
            node: from.clone(),
            message_id: id.to_string(),
        };
        match message {
            // A prompt that starts with `!` or `/` is plain text for the model, never a shell
            // line or a command.
            ClientMsg::Prompt { message_id, text } => {
                let size = text.len();
                match self
                    .steering
                    .queue_remote(sender(&message_id), Queued::Message(text))
                {
                    Ok(_) => {
                        self.audit(
                            "message_accepted",
                            &format!("message {message_id} ({size} bytes) from {from} queued"),
                            Decision::Allow,
                        );
                        self.wake();
                        Handled::Accepted { message_id }
                    }
                    Err(_) => Handled::Rejected {
                        message_id,
                        reason: Reason::InboxFull,
                    },
                }
            }
            ClientMsg::Slash { message_id, line } => {
                let line = line.trim().to_string();
                let Some(command) = parse_command(&line) else {
                    return Handled::Rejected {
                        message_id,
                        reason: Reason::Malformed,
                    };
                };
                if !command.device_allowed() {
                    return self.refused_command(from, &line);
                }
                let entry = match command {
                    Command::Other { name, argument }
                        if lock(&self.skills, "remote skill list").contains(&name) =>
                    {
                        Queued::Skill {
                            raw: line.clone(),
                            name,
                            argument,
                        }
                    }
                    _ => Queued::Command(line.clone()),
                };
                match self.steering.queue_remote(sender(&message_id), entry) {
                    Ok(_) => {
                        self.audit(
                            "message_accepted",
                            &format!(
                                "command {message_id} ({} bytes) from {from} queued",
                                line.len()
                            ),
                            Decision::Allow,
                        );
                        self.wake();
                        Handled::Accepted { message_id }
                    }
                    Err(_) => Handled::Rejected {
                        message_id,
                        reason: Reason::InboxFull,
                    },
                }
            }
            ClientMsg::ApprovalAnswer { id, answer } => {
                let by = sender(&id.to_string());
                lock(&self.inputs, "device inputs").push_back(DeviceInput::Answer {
                    by,
                    id,
                    answer: DeviceAnswer::Approval {
                        choice: answer.choice,
                        feedback: answer.feedback,
                    },
                });
                self.wake();
                Handled::Silent
            }
            ClientMsg::QuestionReply {
                id,
                index,
                options,
                reply,
            } => {
                let by = sender(&id.to_string());
                lock(&self.inputs, "device inputs").push_back(DeviceInput::Answer {
                    by,
                    id,
                    answer: DeviceAnswer::Question {
                        index,
                        options,
                        reply,
                    },
                });
                self.wake();
                Handled::Silent
            }
            ClientMsg::Interrupt => {
                lock(&self.inputs, "device inputs")
                    .push_back(DeviceInput::Interrupt { from: from.clone() });
                self.wake();
                Handled::Silent
            }
            ClientMsg::Hello { .. } | ClientMsg::Ping => Handled::Silent,
        }
    }

    fn confirm_pairing(&self, node: &Sender, label: &str) -> BoxFuture<bool> {
        let (reply, answer) = oneshot::channel();
        if self.is_closed() {
            return Box::pin(async { false });
        }
        lock(&self.pairings, "pairing queue").push_back(PairingRequest {
            node: node.clone(),
            label: label.to_string(),
            reply,
        });
        self.wake();
        // Dropped by the server on a timeout or a client disconnect; the guard wakes the loop so
        // it sees the sender closed and closes the dialog with no reply.
        let _wake = WakeOnDrop(self.wake.clone());
        Box::pin(async move {
            let _wake = _wake;
            answer.await.unwrap_or(false)
        })
    }

    fn paired(&self, node: &Sender, label: &str) {
        self.audit(
            "device_paired",
            &format!("device {node} (\"{label}\") was paired"),
            Decision::Allow,
        );
        self.note(
            Level::Info,
            format!("device {} (\"{label}\") paired", short(node)),
        );
    }

    fn attached(&self, node: &Sender, label: &str) {
        self.audit(
            "device_attached",
            &format!("device {node} (\"{label}\") attached"),
            Decision::Allow,
        );
        self.note(
            Level::Info,
            format!("device {} (\"{label}\") attached", short(node)),
        );
        if self.state.afk_on() {
            self.hub.notice_to(
                node,
                Level::Info,
                "AFK mode is on: approvals and questions are being answered unattended on the host, so nothing will ask this device",
            );
        }
    }

    fn detached(&self, node: &Sender) {
        self.audit(
            "device_detached",
            &format!("device {node} detached"),
            Decision::Allow,
        );
        self.note(Level::Info, format!("device {} detached", short(node)));
    }

    fn refused(&self, reason: Reason, node: &Sender) {
        self.audit(
            "device_refused",
            &format!("device {node} was refused: {}", reason.as_str()),
            Decision::Deny,
        );
    }

    fn warn(&self, text: &str) {
        self.note(Level::Warning, text.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_remote::HubLimits;

    use crate::ui::test_support::session_state;

    fn surface() -> (RemoteSurface, Steering) {
        let steering = Steering::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let hub = Hub::new(HubLimits::default(), Arc::new(InputWaker(tx.downgrade())));
        (
            RemoteSurface::new(steering.clone(), session_state(), tx.downgrade(), hub),
            steering,
        )
    }

    fn msg(text: &str) -> ClientMsg {
        ClientMsg::Prompt {
            message_id: "m".into(),
            text: text.into(),
        }
    }

    fn slash(line: &str) -> ClientMsg {
        ClientMsg::Slash {
            message_id: "s".into(),
            line: line.into(),
        }
    }

    #[test]
    fn a_prompt_that_looks_like_a_command_or_a_shell_line_is_plain_text() {
        let (surface, steering) = surface();
        let node: Sender = "node".into();
        for line in ["/guardian-disable", "!rm -rf x", "y", "/remote on"] {
            assert!(matches!(
                surface.client_message(&node, "l", msg(line)),
                Handled::Accepted { .. }
            ));
        }
        let queued = steering.retract();
        assert_eq!(queued.len(), 4);
        assert!(queued.iter().all(|q| matches!(q.entry, Queued::Message(_))));
    }

    #[test]
    fn a_refused_command_never_reaches_the_queue_and_is_audited() {
        let (surface, steering) = surface();
        let node: Sender = "node".into();
        for line in [
            "/guardian-disable",
            "/guardian-enable",
            "/remote on",
            "/remote-inbox on",
            "/afk review",
            "/export x",
            "/theme dark",
            "/model x",
        ] {
            let handled = surface.client_message(&node, "l", slash(line));
            // `/remote-inbox` no longer exists: it parses as an unknown name, a skill candidate.
            if line.starts_with("/remote-inbox") {
                assert!(matches!(handled, Handled::Accepted { .. }), "{line}");
                steering.retract();
                continue;
            }
            assert!(
                matches!(handled, Handled::Notice { .. }),
                "{line}: {handled:?}"
            );
        }
        assert!(steering.queued().is_empty());
        let audit = surface.state.audit.read().unwrap();
        assert!(
            audit
                .iter()
                .filter(|r| r.rule.as_deref() == Some("command_refused"))
                .count()
                >= 7
        );
    }

    #[test]
    fn an_allowed_command_and_a_known_skill_queue_as_entries() {
        let (surface, steering) = surface();
        surface.set_skills(["review".to_string()]);
        let node: Sender = "node".into();
        assert!(matches!(
            surface.client_message(&node, "l", slash("/compact")),
            Handled::Accepted { .. }
        ));
        assert!(matches!(
            surface.client_message(&node, "l", slash("/review 42")),
            Handled::Accepted { .. }
        ));
        let entries: Vec<Queued> = steering.retract().into_iter().map(|r| r.entry).collect();
        assert!(matches!(entries[0], Queued::Command(_)));
        assert!(matches!(entries[1], Queued::Skill { .. }));
    }

    #[test]
    fn a_slash_line_that_is_not_a_command_is_malformed() {
        let (surface, _) = surface();
        assert!(matches!(
            surface.client_message(&"n".into(), "l", slash("hello")),
            Handled::Rejected {
                reason: Reason::Malformed,
                ..
            }
        ));
    }

    #[test]
    fn answers_and_interrupts_are_parked_for_the_loop_with_the_device_named() {
        let (surface, steering) = surface();
        let node: Sender = "device-node".into();
        surface.client_message(
            &node,
            "l",
            ClientMsg::ApprovalAnswer {
                id: 4,
                answer: fontaine_remote::WireAnswer {
                    choice: "allow once".into(),
                    feedback: None,
                },
            },
        );
        surface.client_message(&node, "l", ClientMsg::Interrupt);
        let inputs = surface.take_inputs();
        assert_eq!(inputs.len(), 2);
        let DeviceInput::Answer { by, id, .. } = &inputs[0] else {
            panic!("an answer");
        };
        assert_eq!(
            (by.node.as_str(), by.message_id.as_str(), *id),
            ("device-node", "4", 4)
        );
        assert!(matches!(inputs[1], DeviceInput::Interrupt { .. }));
        // A prompt sent while a dialog is open is queued like any other message.
        assert!(steering.queued().is_empty());
    }

    #[tokio::test]
    async fn a_pairing_waits_for_the_host_and_a_dropped_future_cancels_it() {
        let (surface, _) = surface();
        let answered = surface.confirm_pairing(&"n1".into(), "laptop");
        let request = surface.take_pairing().expect("a pairing request");
        assert_eq!(
            (request.node.as_str(), request.label.as_str()),
            ("n1", "laptop")
        );
        request.reply.send(true).unwrap();
        assert!(answered.await);

        let abandoned = surface.confirm_pairing(&"n2".into(), "phone");
        drop(abandoned);
        assert!(surface.take_pairing().is_none());
    }
}
