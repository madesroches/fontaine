//! A hook command for tests: `hook_marker FILE [REPLY_JSON]` records that it ran by writing FILE
//! and prints REPLY_JSON, so a test can tell whether a repository's hook started and what it said.

use std::io::Read;

fn main() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let mut args = std::env::args().skip(1);
    if let Some(path) = args.next() {
        std::fs::write(path, input).expect("write the marker");
    }
    if let Some(reply) = args.next() {
        println!("{reply}");
    }
}
