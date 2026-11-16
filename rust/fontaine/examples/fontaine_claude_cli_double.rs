//! The scripted Claude CLI of `fontaine_sdk::core`, built in this package so the headless tests can
//! start it.

#[path = "../../fontaine-sdk/examples/claude_cli_double.rs"]
mod claude_cli_double;

fn main() {
    claude_cli_double::main();
}
