//! A hook command for tests: `hook_marker [--append] [--exit CODE] [--stderr TEXT] FILE
//! [REPLY_JSON]` records that it ran by writing FILE (or, with `--append`, adding the event as one
//! line to it, so a test can count runs) and prints REPLY_JSON, so a test can tell whether a
//! hook started and what it said. `--exit` and `--stderr` make it block as an exit code 2 does,
//! and `--block-first N` (with `--append`) exits 2 until FILE holds more than N lines.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let mut append = false;
    let mut exit = 0;
    let mut stderr = None;
    let mut block_first = 0usize;
    let mut rest = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--append" => append = true,
            "--exit" => exit = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--stderr" => stderr = args.next(),
            "--block-first" => block_first = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            _ => rest.push(arg),
        }
    }
    let mut rest = rest.into_iter();
    if let Some(path) = rest.next() {
        if append {
            let line = input.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("open the marker");
            writeln!(file, "{line}").expect("append to the marker");
            drop(file);
            let lines = std::fs::read_to_string(&path).map_or(0, |text| text.lines().count());
            if block_first > 0 && lines <= block_first {
                exit = 2;
            }
        } else {
            std::fs::write(path, input).expect("write the marker");
        }
    }
    if let Some(text) = stderr {
        eprintln!("{text}");
    }
    if let Some(reply) = rest.next() {
        println!("{reply}");
    }
    std::process::exit(exit);
}
