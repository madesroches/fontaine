//! A program for tests that must see whether something started it: it writes its first
//! argument as a file, prints its second argument if there is one, waits a moment and exits.
//! As a hook it ignores its input; as a server it never speaks MCP.

fn main() {
    let mut args = std::env::args().skip(1);
    if let Some(path) = args.next() {
        std::fs::write(path, "started").expect("write the marker");
    }
    if let Some(reply) = args.next() {
        println!("{reply}");
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}
