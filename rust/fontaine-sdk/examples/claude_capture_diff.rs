//! Diffs two consecutive requests of a `claude-subscription` capture (`FONTAINE_CLAUDE_CAPTURE`).
//!
//! `cargo run -p fontaine-sdk --example claude_capture_diff -- <file> <session> <index>` prints
//! how request `index` differs from request `index - 1`: where it stopped extending the earlier
//! request, then the first differing frame and the JSON paths that changed in it.

use std::path::Path;
use std::process::ExitCode;

use fontaine_sdk::core::providers::claude_subscription::capture::diff;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [file, session, index] = args.as_slice() else {
        eprintln!("usage: claude_capture_diff <file> <session> <index>");
        return ExitCode::from(2);
    };
    let Some(index) = index.parse::<usize>().ok().filter(|i| *i > 0) else {
        eprintln!("the index must be a request number of at least 1");
        return ExitCode::from(2);
    };
    match diff(Path::new(file), session, index - 1, index) {
        Ok(found) => {
            print!("{found}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", fontaine_sdk::tools::error_chain::chain(&error));
            ExitCode::FAILURE
        }
    }
}
