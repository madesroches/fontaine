//! A program for tests that must see whether something started it: it writes its first
//! argument as a file, prints its second argument if there is one, waits a moment and exits.
//! With `--append FILE` it adds the event it was given as one line to FILE instead, so a test can
//! count runs. As a hook it ignores its input otherwise; as a server it never speaks MCP.

use std::io::{Read, Write};

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("--append") {
        args.next();
        let path = args.next().expect("a file to append to");
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        let line = input.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open the marker");
        writeln!(file, "{line}").expect("append to the marker");
    } else if let Some(path) = args.next() {
        std::fs::write(path, "started").expect("write the marker");
    }
    if let Some(reply) = args.next() {
        println!("{reply}");
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}
