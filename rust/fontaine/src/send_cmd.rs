//! `fontaine send`: delivers one message to a live session through its remote inbox. It reads
//! no config and starts no session; the ticket names the endpoint and carries the secret.

use std::io::Read;
use std::process::ExitCode;

use fontaine_remote::{Reason, SendError, SendOptions, Ticket, send};
use fontaine_tools::error_chain::chain;

use crate::cli::SendArgs;
use crate::{Host, usage};

/// The environment variable that holds the ticket for a caller that would rather not put it on
/// the command line.
pub const TICKET_VAR: &str = "FONTAINE_TICKET";

const EXIT_BAD_TICKET: u8 = 2;
const EXIT_REFUSED: u8 = 3;
const EXIT_UNREACHABLE: u8 = 4;
const EXIT_PROTOCOL: u8 = 5;

fn read_stdin() -> std::io::Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(text)
}

/// Which exit code a failure earns: the ticket's fault, a full or too small inbox, no way to
/// reach the session, or a protocol mismatch.
pub fn exit_code(error: &SendError) -> u8 {
    match error {
        SendError::Rejected(reason) => match reason {
            Reason::BadTicket | Reason::Expired | Reason::Reused | Reason::Locked => {
                EXIT_BAD_TICKET
            }
            Reason::InboxFull | Reason::TooLarge => EXIT_REFUSED,
            Reason::Closed => EXIT_UNREACHABLE,
            Reason::UnsupportedVersion | Reason::Malformed => EXIT_PROTOCOL,
        },
        SendError::Closed | SendError::Timeout | SendError::Connect(_) => EXIT_UNREACHABLE,
        SendError::Protocol(_) => EXIT_PROTOCOL,
    }
}

/// The ticket and text this invocation names, or the usage error that explains what is missing.
fn inputs(args: &SendArgs, host: &Host) -> Result<(String, String), String> {
    let (ticket, text) = match args.words.as_slice() {
        [ticket, text] => (Some(ticket.clone()), text.clone()),
        [text] => (None, text.clone()),
        _ => return Err("expected `fontaine send <ticket> <text>`".to_string()),
    };
    if args.ticket_stdin && text == "-" {
        return Err("--ticket-stdin and a text of `-` both read stdin; use one".to_string());
    }
    let ticket = match ticket {
        Some(ticket) => ticket,
        None if args.ticket_stdin => {
            read_stdin().map_err(|e| format!("cannot read the ticket from stdin: {e}"))?
        }
        None => (host.env)(TICKET_VAR)
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| {
                format!("no ticket: pass it before the text, or use --ticket-stdin or {TICKET_VAR}")
            })?,
    };
    let text = if text == "-" {
        read_stdin().map_err(|e| format!("cannot read the message from stdin: {e}"))?
    } else {
        text
    };
    let text = text.trim_end().to_string();
    if text.trim().is_empty() {
        return Err("the message is empty".to_string());
    }
    Ok((ticket, text))
}

pub fn run(args: &SendArgs, host: &Host) -> ExitCode {
    let (ticket, text) = match inputs(args, host) {
        Ok(inputs) => inputs,
        Err(message) => return usage(message),
    };
    let ticket = match Ticket::parse(&ticket) {
        Ok(ticket) => ticket,
        Err(e) => {
            eprintln!("fontaine: {}", chain(&e));
            return ExitCode::from(EXIT_BAD_TICKET);
        }
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
    match runtime.block_on(send(&ticket, &text, SendOptions::default())) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_the_documented_exit_code() {
        let code = |reason| exit_code(&SendError::Rejected(reason));
        for reason in [
            Reason::BadTicket,
            Reason::Expired,
            Reason::Reused,
            Reason::Locked,
        ] {
            assert_eq!(code(reason), 2, "{reason:?}");
        }
        for reason in [Reason::InboxFull, Reason::TooLarge] {
            assert_eq!(code(reason), 3, "{reason:?}");
        }
        assert_eq!(code(Reason::Closed), 4);
        for reason in [Reason::UnsupportedVersion, Reason::Malformed] {
            assert_eq!(code(reason), 5, "{reason:?}");
        }
        assert_eq!(exit_code(&SendError::Timeout), 4);
        assert_eq!(exit_code(&SendError::Closed), 4);
        assert_eq!(exit_code(&SendError::Connect("x".into())), 4);
    }

    fn host(env: Option<&'static str>) -> Host {
        Host {
            cwd: std::path::PathBuf::new(),
            env: std::sync::Arc::new(move |name| {
                (name == TICKET_VAR)
                    .then(|| env.map(std::ffi::OsString::from))
                    .flatten()
            }),
            default_config_dir: None,
            default_data_dir: None,
            env_names: Box::new(Vec::new),
        }
    }

    fn args(words: &[&str], ticket_stdin: bool) -> SendArgs {
        SendArgs {
            words: words.iter().map(|w| w.to_string()).collect(),
            ticket_stdin,
        }
    }

    #[test]
    fn a_ticket_on_stdin_with_a_text_of_dash_is_a_usage_error() {
        let error = inputs(&args(&["-"], true), &host(None)).unwrap_err();
        assert!(error.contains("both read stdin"), "{error}");
    }

    #[test]
    fn the_ticket_comes_from_the_argument_then_the_environment() {
        let (ticket, text) = inputs(&args(&["t1", "hello"], false), &host(Some("env"))).unwrap();
        assert_eq!((ticket.as_str(), text.as_str()), ("t1", "hello"));
        let (ticket, text) = inputs(&args(&["hello"], false), &host(Some("env"))).unwrap();
        assert_eq!((ticket.as_str(), text.as_str()), ("env", "hello"));
        let error = inputs(&args(&["hello"], false), &host(None)).unwrap_err();
        assert!(error.contains("no ticket"), "{error}");
    }

    #[test]
    fn an_empty_message_is_refused() {
        let error = inputs(&args(&["t1", "  "], false), &host(None)).unwrap_err();
        assert!(error.contains("empty"), "{error}");
    }
}
