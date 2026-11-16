//! The remote inbox as the terminal UI hosts it: argument parsing for `/remote-inbox`, the
//! `Delivery` that turns an authenticated line into a remote-tagged `Steering` entry, and the
//! audit of everything the inbox does. The network side lives in `fontaine-remote`, which knows
//! nothing of the session.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_remote::{Delivery, DeliveryError, Reason, RemoteServer, Sender};
use fontaine_sdk::core::audit::Decision;
use fontaine_sdk::core::duration::parse_duration;
use fontaine_sdk::core::gate::SessionState;
use fontaine_sdk::core::ledger::RemoteSender;
use fontaine_sdk::core::steering::{Queued, Steering};
use tokio::sync::mpsc::WeakUnboundedSender;

use crate::Input;
use crate::commands::{Command, parse_command};

/// How long a ticket lives when `--ttl` is not given.
pub(crate) const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemoteCommand {
    On(Duration),
    Off,
    Status,
    Ticket(Duration),
}

pub(crate) const USAGE: &str =
    "usage: /remote-inbox on [--ttl <duration>] | off | status | ticket [--ttl <duration>]";

pub(crate) fn parse_remote_command(argument: &str) -> Result<RemoteCommand, String> {
    let mut words = argument.split_whitespace();
    let verb = words.next().unwrap_or("status");
    let mut ttl = DEFAULT_TTL;
    let rest: Vec<&str> = words.collect();
    let takes_ttl = matches!(verb, "on" | "ticket");
    match (takes_ttl, rest.as_slice()) {
        (_, []) => {}
        (true, ["--ttl", value]) => {
            ttl = parse_duration(value)
                .ok_or_else(|| format!("`{value}` is not a duration (try 10m or 2h)"))?;
        }
        (true, [flag]) if flag.starts_with("--ttl=") => {
            let value = &flag["--ttl=".len()..];
            ttl = parse_duration(value)
                .ok_or_else(|| format!("`{value}` is not a duration (try 10m or 2h)"))?;
        }
        _ => return Err(USAGE.to_string()),
    }
    if ttl.is_zero() || ttl > fontaine_remote::MAX_TTL {
        return Err("--ttl must be between 1s and 24h".to_string());
    }
    match verb {
        "on" => Ok(RemoteCommand::On(ttl)),
        "off" if rest.is_empty() => Ok(RemoteCommand::Off),
        "status" if rest.is_empty() => Ok(RemoteCommand::Status),
        "ticket" => Ok(RemoteCommand::Ticket(ttl)),
        _ => Err(USAGE.to_string()),
    }
}

/// The session's side of an enabled inbox. It is shared with the network tasks, so everything
/// here is thread-safe.
pub(crate) struct RemoteHost {
    steering: Steering,
    state: Arc<SessionState>,
    wake: WeakUnboundedSender<Input>,
    skills: Mutex<HashSet<String>>,
    closed: AtomicBool,
    /// Where the endpoint start leaves its result for the interface loop to pick up.
    started: Mutex<Option<Result<RemoteServer, String>>>,
}

impl RemoteHost {
    pub(crate) fn new(
        steering: Steering,
        state: Arc<SessionState>,
        wake: WeakUnboundedSender<Input>,
    ) -> RemoteHost {
        RemoteHost {
            steering,
            state,
            wake,
            skills: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            started: Mutex::new(None),
        }
    }

    /// From here on `accept` answers `Closed`.
    pub(crate) fn close(&self) {
        // Under the `started` lock so a start finishing concurrently sees either the close or
        // its stored result, and never a server stored after the final `take_started`.
        let _started = fontaine_sdk::tools::lock::lock_recovering(&self.started);
        self.closed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn set_skills(&self, names: impl IntoIterator<Item = String>) {
        // A stale skill list only misfiles one line as a command; a poisoned lock holds it still.
        *fontaine_sdk::tools::lock::lock_recovering(&self.skills) = names.into_iter().collect();
    }

    pub(crate) fn wake(&self) {
        if let Some(tx) = self.wake.upgrade() {
            let _ = tx.send(Input::Remote);
        }
    }

    /// Leaves the start's result and wakes the loop; the loop takes it with `take_started`.
    pub(crate) async fn finish_start(&self, result: Result<RemoteServer, String>) {
        let rejected = {
            // The slot holds one result at a time and is read out whole; a poisoned lock still holds it.
            let mut started = fontaine_sdk::tools::lock::lock_recovering(&self.started);
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
        fontaine_sdk::tools::lock::lock_recovering(&self.started).take()
    }

    pub(crate) fn audit(&self, rule: &str, explanation: &str, decision: Decision) {
        self.state.record_remote_event(rule, explanation, decision);
    }

    /// How a line from a remote sender queues: a `!` line, a slash command or a skill wait for
    /// the turn to end like typed ones; plain text is a message.
    fn classify(&self, text: &str) -> Queued {
        let line = text.trim();
        if let Some(rest) = line.strip_prefix('!')
            && !rest.trim().is_empty()
        {
            return Queued::Shell(line.to_string());
        }
        match parse_command(line) {
            Some(Command::Other { name, argument })
                if fontaine_sdk::tools::lock::lock_recovering(&self.skills).contains(&name) =>
            {
                Queued::Skill {
                    raw: line.to_string(),
                    name,
                    argument,
                }
            }
            Some(_) => Queued::Command(line.to_string()),
            None => Queued::Message(text.to_string()),
        }
    }
}

impl Delivery for RemoteHost {
    fn accept(&self, from: &Sender, message_id: &str, text: String) -> Result<(), DeliveryError> {
        if self.is_closed() {
            return Err(DeliveryError::Closed);
        }
        let size = text.len();
        let entry = self.classify(&text);
        let sender = RemoteSender {
            node: from.clone(),
            message_id: message_id.to_string(),
        };
        if self.steering.queue_remote(sender, entry).is_err() {
            return Err(DeliveryError::Full);
        }
        self.audit(
            "message_accepted",
            &format!("message {message_id} ({size} bytes) from {from} queued"),
            Decision::Allow,
        );
        self.wake();
        Ok(())
    }

    fn on_reject(&self, reason: Reason, node: &Sender) {
        self.audit(
            "message_rejected",
            &format!("request from {node} rejected: {}", reason.as_str()),
            Decision::Deny,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_parse() {
        assert_eq!(
            parse_remote_command("on"),
            Ok(RemoteCommand::On(DEFAULT_TTL))
        );
        assert_eq!(
            parse_remote_command("on --ttl 2h"),
            Ok(RemoteCommand::On(Duration::from_secs(7200)))
        );
        assert_eq!(
            parse_remote_command("ticket --ttl=30m"),
            Ok(RemoteCommand::Ticket(Duration::from_secs(1800)))
        );
        assert_eq!(parse_remote_command("off"), Ok(RemoteCommand::Off));
        assert_eq!(parse_remote_command(""), Ok(RemoteCommand::Status));
        assert_eq!(parse_remote_command("status"), Ok(RemoteCommand::Status));
        assert!(parse_remote_command("off --ttl 5m").is_err());
        assert!(parse_remote_command("on --ttl soon").is_err());
        assert!(parse_remote_command("on --ttl 0s").is_err());
        assert!(parse_remote_command("ticket --ttl 25h").is_err());
        assert!(parse_remote_command("maybe").is_err());
        assert!(parse_remote_command("on extra").is_err());
    }
}
