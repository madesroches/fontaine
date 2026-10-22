//! A hook command for tests, portable because it is a program rather than a shell script:
//! `hook_double [--record FILE] [--report FILE NAME...] [--reply JSON] [--stderr TEXT]
//! [--exit CODE] [--sleep SECS]`. It copies its stdin to `--record`, writes whether each
//! `--report` variable is set, then answers as told.

use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let mut at = 0;
    let mut exit = 0;
    let mut reply: Option<String> = None;
    let mut stderr: Option<String> = None;
    let mut sleep = 0u64;
    while at < args.len() {
        let value = args.get(at + 1).cloned().unwrap_or_default();
        match args[at].as_str() {
            "--record" => {
                std::fs::write(&value, &stdin).expect("record the event");
                at += 2;
            }
            "--report" => {
                let mut report = String::new();
                at += 2;
                while at < args.len() && !args[at].starts_with("--") {
                    let state = if std::env::var_os(&args[at]).is_some() {
                        "present"
                    } else {
                        "absent"
                    };
                    report.push_str(&format!("{}={state}\n", args[at]));
                    at += 1;
                }
                std::fs::write(&value, report).expect("write the report");
            }
            "--reply" => {
                reply = Some(value);
                at += 2;
            }
            "--stderr" => {
                stderr = Some(value);
                at += 2;
            }
            "--exit" => {
                exit = value.parse().unwrap_or(1);
                at += 2;
            }
            "--sleep" => {
                sleep = value.parse().unwrap_or(0);
                at += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    if sleep > 0 {
        std::thread::sleep(std::time::Duration::from_secs(sleep));
    }
    if let Some(text) = stderr {
        let _ = writeln!(std::io::stderr(), "{text}");
    }
    if let Some(text) = reply {
        let _ = writeln!(std::io::stdout(), "{text}");
    }
    std::process::exit(exit);
}
