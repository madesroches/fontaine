//! `fontaine attach` and `fontaine remote`: the device side of remote control, and the offline
//! management of the host's paired devices. Neither reads project config, starts a session or
//! loads provider credentials: a ticket or a paired host is everything.

use std::io::Read;
use std::process::ExitCode;
use std::sync::Arc;

use fontaine_remote::{
    AttachError, AttachOptions, ClientStore, DeviceStore, Reason, RelayMode, Target, Ticket,
};
use fontaine_sdk::tools::error_chain::chain;
use fontaine_tui::remote::{AttachEnd, AttachParams, FileStore, run_attached};

use crate::cli::{AttachArgs, RemoteAction, RemoteArgs};
use crate::{Host, resolve_dirs, usage};

/// The environment variable that holds the ticket for a caller that would rather not put it on
/// the command line.
pub const TICKET_VAR: &str = "FONTAINE_TICKET";

const EXIT_AUTH: u8 = 2;
const EXIT_REFUSED: u8 = 3;
const EXIT_UNREACHABLE: u8 = 4;
const EXIT_PROTOCOL: u8 = 5;

/// Which exit code a refusal earns: the ticket, the pairing or the device's standing; a full or
/// too small queue; no way to reach the session; or a protocol mismatch.
pub fn reason_code(reason: Reason) -> u8 {
    match reason {
        Reason::BadTicket
        | Reason::Expired
        | Reason::Reused
        | Reason::Locked
        | Reason::NotPaired
        | Reason::PairingDeclined
        | Reason::PairingTimeout
        | Reason::Revoked => EXIT_AUTH,
        Reason::InboxFull | Reason::TooLarge => EXIT_REFUSED,
        Reason::Closed | Reason::Superseded => EXIT_UNREACHABLE,
        Reason::UnsupportedVersion | Reason::Malformed => EXIT_PROTOCOL,
    }
}

pub fn exit_code(error: &AttachError) -> u8 {
    match error {
        AttachError::Rejected(reason) => reason_code(*reason),
        AttachError::Closed
        | AttachError::Timeout
        | AttachError::Connect(_)
        | AttachError::Store(_) => EXIT_UNREACHABLE,
        AttachError::Protocol(_) => EXIT_PROTOCOL,
    }
}

fn read_stdin() -> std::io::Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(text)
}

/// The text of the one message, when the run is a one-shot.
fn message_text(args: &AttachArgs) -> Result<Option<String>, String> {
    let Some(message) = &args.message else {
        return Ok(None);
    };
    if args.ticket_stdin && message == "-" {
        return Err("--ticket-stdin and `--message -` both read stdin; use one".to_string());
    }
    let text = if message == "-" {
        read_stdin().map_err(|e| format!("cannot read the message from stdin: {e}"))?
    } else {
        message.clone()
    };
    let text = text.trim_end().to_string();
    if text.trim().is_empty() {
        return Err("the message is empty".to_string());
    }
    Ok(Some(text))
}

/// What the invocation names: a ticket to pair with, or a stored host to resume.
fn target_of(
    args: &AttachArgs,
    host: &Host,
    store: &dyn ClientStore,
) -> Result<Target, (String, u8)> {
    let usage_error = |message: String| (message, crate::EXIT_CONFIG);
    let text = match (&args.target, args.ticket_stdin) {
        (Some(_), true) => {
            return Err(usage_error(
                "pass a ticket or a host, or use --ticket-stdin, not both".to_string(),
            ));
        }
        (Some(text), false) => text.clone(),
        (None, true) => read_stdin()
            .map_err(|e| usage_error(format!("cannot read the ticket from stdin: {e}")))?,
        (None, false) => (host.env)(TICKET_VAR)
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| {
                usage_error(format!(
                    "no ticket or host: pass one, or use --ticket-stdin or {TICKET_VAR}"
                ))
            })?,
    };
    let trimmed = text.trim();
    if trimmed.starts_with("fontaine-") {
        return match Ticket::parse(trimmed) {
            Ok(ticket) => Ok(Target::Ticket(Box::new(ticket))),
            Err(e) => Err((chain(&e), EXIT_AUTH)),
        };
    }
    let hosts = store.hosts().map_err(|e| {
        (
            format!("cannot read the known hosts: {}", chain(&e)),
            EXIT_UNREACHABLE,
        )
    })?;
    match hosts.find(trimmed) {
        Ok(stored) => Ok(Target::Host(Box::new(stored.clone()))),
        Err(e) => Err((
            format!(
                "`{trimmed}` is not a ticket and names no host this device is paired with ({e}); attach with a ticket first"
            ),
            EXIT_AUTH,
        )),
    }
}

/// The relay mode from the user-scope config only: project scope can never reach this.
fn user_relay(dirs: &fontaine_sdk::core::dirs::Dirs) -> RelayMode {
    let path = dirs.config.join(fontaine_sdk::core::config::USER_FILE);
    let relay = fontaine_sdk::core::file_lock::read_shared(&path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| {
            table
                .get("remote")?
                .get("relay")?
                .as_str()
                .map(str::to_string)
        });
    match relay.as_deref() {
        Some("disabled") => RelayMode::Disabled,
        _ => RelayMode::Default,
    }
}

fn device_label(args: &AttachArgs, host: &Host) -> String {
    if let Some(name) = &args.name {
        return name.clone();
    }
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|var| (host.env)(var).and_then(|v| v.into_string().ok()))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "device".to_string())
}

pub fn run(args: &AttachArgs, host: &Host) -> ExitCode {
    let message = match message_text(args) {
        Ok(message) => message,
        Err(why) => return usage(why),
    };
    let dirs = match resolve_dirs(host) {
        Ok(dirs) => dirs,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(crate::EXIT_CONFIG);
        }
    };
    if message.is_none()
        && !(std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stdout()))
    {
        return usage("attaching needs a terminal; use `--message` for a one-shot send");
    }
    let store = Arc::new(FileStore::new(&dirs.data));
    let target = match target_of(args, host, store.as_ref()) {
        Ok(target) => target,
        Err((message, code)) => {
            eprintln!("fontaine: {message}");
            return ExitCode::from(code);
        }
    };
    // A ticket carries the host's addresses and relay; a stored host follows the user's setting.
    let relay = match &target {
        Target::Ticket(_) => None,
        Target::Host(_) => Some(user_relay(&dirs)),
    };
    let relay = match (relay, user_relay(&dirs)) {
        (Some(relay), _) => Some(relay),
        (None, RelayMode::Disabled) => Some(RelayMode::Disabled),
        (None, _) => None,
    };
    let options = AttachOptions {
        relay,
        ..AttachOptions::new(device_label(args, host))
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("fontaine: cannot start the async runtime: {e}");
            return ExitCode::from(1);
        }
    };
    match message {
        Some(text) => {
            match runtime.block_on(fontaine_remote::send_message(target, &text, options, store)) {
                Ok(accepted) => {
                    println!("accepted {}", accepted.message_id);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("fontaine: {}", chain(&e));
                    ExitCode::from(exit_code(&e))
                }
            }
        }
        None => {
            let params = AttachParams {
                target,
                options,
                store,
                linear: false,
            };
            match runtime.block_on(run_attached(params)) {
                Ok(AttachEnd::Detached) => ExitCode::SUCCESS,
                Ok(AttachEnd::HostClosed) => {
                    eprintln!("fontaine: the session turned remote control off or ended");
                    ExitCode::SUCCESS
                }
                Ok(AttachEnd::Failed(e)) => {
                    eprintln!("fontaine: {}", chain(&e));
                    ExitCode::from(exit_code(&e))
                }
                Err(message) => {
                    eprintln!("fontaine: {message}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// `fontaine remote devices|revoke`: reads and writes the paired list directly. A running host
/// re-reads the file on every device message and keepalive and closes a revoked connection.
pub fn run_remote(args: &RemoteArgs, host: &Host) -> ExitCode {
    let dirs = match resolve_dirs(host) {
        Ok(dirs) => dirs,
        Err(e) => {
            eprintln!("fontaine: {e}");
            return ExitCode::from(crate::EXIT_CONFIG);
        }
    };
    let store = FileStore::new(&dirs.data);
    match &args.action {
        RemoteAction::Devices => match store.devices() {
            Ok(list) if list.devices.is_empty() => {
                println!("no devices are paired");
                ExitCode::SUCCESS
            }
            Ok(list) => {
                for device in &list.devices {
                    println!(
                        "{}\t{}\tpaired {}\tlast seen {}",
                        device.label, device.node, device.paired_at, device.last_seen
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("fontaine: cannot read the paired devices: {}", chain(&e));
                ExitCode::from(1)
            }
        },
        RemoteAction::Revoke { device } => {
            let mut outcome = None;
            if let Err(e) = store.update(&mut |list| outcome = Some(list.revoke(device))) {
                eprintln!("fontaine: cannot update the paired devices: {}", chain(&e));
                return ExitCode::from(1);
            }
            match outcome {
                Some(Ok(removed)) => {
                    println!("revoked {} ({})", removed.label, removed.node);
                    ExitCode::SUCCESS
                }
                Some(Err(e)) => {
                    eprintln!("fontaine: cannot revoke `{device}`: {e}");
                    ExitCode::from(1)
                }
                None => ExitCode::from(1),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fontaine_remote::{MemoryClientStore, StoredHost};

    use super::*;

    #[test]
    fn every_reason_has_the_documented_exit_code() {
        let code = |reason| exit_code(&AttachError::Rejected(reason));
        for reason in [
            Reason::BadTicket,
            Reason::Expired,
            Reason::Reused,
            Reason::Locked,
            Reason::NotPaired,
            Reason::PairingDeclined,
            Reason::PairingTimeout,
            Reason::Revoked,
        ] {
            assert_eq!(code(reason), 2, "{reason:?}");
        }
        for reason in [Reason::InboxFull, Reason::TooLarge] {
            assert_eq!(code(reason), 3, "{reason:?}");
        }
        for reason in [Reason::Closed, Reason::Superseded] {
            assert_eq!(code(reason), 4, "{reason:?}");
        }
        for reason in [Reason::UnsupportedVersion, Reason::Malformed] {
            assert_eq!(code(reason), 5, "{reason:?}");
        }
        assert_eq!(exit_code(&AttachError::Timeout), 4);
        assert_eq!(exit_code(&AttachError::Closed), 4);
        assert_eq!(exit_code(&AttachError::Connect("x".into())), 4);
        assert_eq!(exit_code(&AttachError::Protocol("x".into())), 5);
    }

    fn host(env: Option<&'static str>) -> Host {
        Host {
            cwd: std::path::PathBuf::new(),
            env: Arc::new(move |name| {
                (name == TICKET_VAR)
                    .then(|| env.map(std::ffi::OsString::from))
                    .flatten()
            }),
            default_config_dir: None,
            default_data_dir: None,
            env_names: Box::new(Vec::new),
        }
    }

    fn args(target: Option<&str>, ticket_stdin: bool, message: Option<&str>) -> AttachArgs {
        AttachArgs {
            target: target.map(str::to_string),
            ticket_stdin,
            message: message.map(str::to_string),
            name: None,
        }
    }

    #[test]
    fn a_ticket_on_stdin_with_a_message_of_dash_is_a_usage_error() {
        let error = message_text(&args(None, true, Some("-"))).unwrap_err();
        assert!(error.contains("both read stdin"), "{error}");
    }

    #[test]
    fn an_empty_message_is_refused() {
        assert!(message_text(&args(None, false, Some("  "))).is_err());
        assert_eq!(message_text(&args(None, false, None)), Ok(None));
        assert_eq!(
            message_text(&args(None, false, Some("hello\n"))),
            Ok(Some("hello".to_string()))
        );
    }

    #[test]
    fn the_target_is_a_ticket_a_stored_host_or_the_environment() {
        let store = MemoryClientStore::default();
        let key = fontaine_remote::SecretKey::from_bytes(&[3; 32]).public();
        store
            .update_hosts(&mut |hosts| {
                hosts.upsert(StoredHost {
                    node: key.to_string(),
                    label: "work".into(),
                    addr: fontaine_remote::EndpointAddr::new(key),
                })
            })
            .unwrap();
        let ticket = Ticket::new(fontaine_remote::EndpointAddr::new(key), [1; 32], 5).encode();
        assert!(matches!(
            target_of(&args(Some(&ticket), false, None), &host(None), &store),
            Ok(Target::Ticket(_))
        ));
        assert!(matches!(
            target_of(&args(Some("work"), false, None), &host(None), &store),
            Ok(Target::Host(_))
        ));
        assert!(matches!(
            target_of(
                &args(None, false, None),
                &host(Some(Box::leak(ticket.clone().into_boxed_str()))),
                &store
            ),
            Ok(Target::Ticket(_))
        ));
        let (message, code) = target_of(&args(Some("nobody"), false, None), &host(None), &store)
            .err()
            .unwrap();
        assert!(message.contains("names no host"), "{message}");
        assert_eq!(code, 2);
        let (message, _) = target_of(&args(None, false, None), &host(None), &store)
            .err()
            .unwrap();
        assert!(message.contains("no ticket or host"), "{message}");
        // An old `fontaine send` ticket fails at parse time with the ticket error.
        let (message, code) = target_of(
            &args(Some("fontaine-send1abc"), false, None),
            &host(None),
            &store,
        )
        .err()
        .unwrap();
        assert!(message.contains("removed `fontaine send`"), "{message}");
        assert_eq!(code, 2);
    }
}
