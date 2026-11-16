//! Remote control as the terminal UI hosts it: argument parsing for `/remote`, the surface the
//! network tasks reach the session through, the event mirror devices render from, the stores
//! under the data directory, and the client that drives a host from another machine. The network
//! side lives in `fontaine-remote`, which knows nothing of the session.

use std::time::Duration;

use fontaine_sdk::core::duration::parse_duration;

mod client_ui;
mod event;
mod store;
pub(crate) mod surface;

pub use client_ui::{AttachEnd, AttachParams, run_attached};
pub(crate) use event::{Dialog, MirrorTracker};
pub use store::FileStore;
pub(crate) use store::ensure_dir;
pub(crate) use surface::{DeviceInput, RemoteSurface};

/// How long a ticket lives when `--ttl` is not given.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemoteCommand {
    On(Duration),
    Off,
    Status,
    Ticket(Duration),
    Devices,
    Revoke(String),
}

pub(crate) const USAGE: &str = "usage: /remote on [--ttl <duration>] | off | status | ticket [--ttl <duration>] | devices | revoke <device>";

fn ttl_of(value: &str) -> Result<Duration, String> {
    let ttl = parse_duration(value)
        .ok_or_else(|| format!("`{value}` is not a duration (try 10m or 2h)"))?;
    if ttl.is_zero() || ttl > fontaine_remote::MAX_TTL {
        return Err("--ttl must be between 1s and 24h".to_string());
    }
    Ok(ttl)
}

pub(crate) fn parse_remote_command(argument: &str) -> Result<RemoteCommand, String> {
    let mut words = argument.split_whitespace();
    let verb = words.next().unwrap_or("status");
    let rest: Vec<&str> = words.collect();
    let ttl = || -> Result<Duration, String> {
        match rest.as_slice() {
            [] => Ok(DEFAULT_TTL),
            ["--ttl", value] => ttl_of(value),
            [flag] if flag.starts_with("--ttl=") => ttl_of(&flag["--ttl=".len()..]),
            _ => Err(USAGE.to_string()),
        }
    };
    match (verb, rest.as_slice()) {
        ("on", _) => Ok(RemoteCommand::On(ttl()?)),
        ("ticket", _) => Ok(RemoteCommand::Ticket(ttl()?)),
        ("off", []) => Ok(RemoteCommand::Off),
        ("status", []) => Ok(RemoteCommand::Status),
        ("devices", []) => Ok(RemoteCommand::Devices),
        ("revoke", [device]) => Ok(RemoteCommand::Revoke((*device).to_string())),
        _ => Err(USAGE.to_string()),
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
        assert_eq!(parse_remote_command("devices"), Ok(RemoteCommand::Devices));
        assert_eq!(
            parse_remote_command("revoke laptop"),
            Ok(RemoteCommand::Revoke("laptop".into()))
        );
        assert!(parse_remote_command("revoke").is_err());
        assert!(parse_remote_command("revoke a b").is_err());
        assert!(parse_remote_command("off --ttl 5m").is_err());
        assert!(parse_remote_command("on --ttl soon").is_err());
        assert!(parse_remote_command("on --ttl 0s").is_err());
        assert!(parse_remote_command("ticket --ttl 25h").is_err());
        assert!(parse_remote_command("maybe").is_err());
        assert!(parse_remote_command("on extra").is_err());
    }
}
