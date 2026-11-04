//! The question about a repository-defined MCP server: it starts only when the user says so,
//! and the repository cannot say so for them.

use crate::confirm_prompt::{Block, ConfirmPrompt};

/// `description` is the command line or URL the repository gave the server. `Some(true)` from
/// the prompt starts it, `Some(false)` leaves it off.
pub fn new(name: &str, description: &str) -> ConfirmPrompt {
    ConfirmPrompt::new(
        format!("Start the MCP server `{name}`?"),
        vec![
            Block::Text("This repository defines it. It runs as a program with your permissions, and its tools are always checked like any other tool that can do anything.".into()),
            Block::Bold(description.to_string()),
        ],
        "y starts it and remembers the answer for this definition · n leaves it off",
    )
}
