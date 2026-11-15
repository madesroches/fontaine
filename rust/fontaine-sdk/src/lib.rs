//! The embeddable Fontaine agent. The module trees are public for the binary, the TUI and the
//! evals; the documented, stable surface is the crate root.

pub mod agents;
pub mod assemble;
pub mod core;
pub mod error;
pub mod events;
pub mod guardian;
pub mod run;
pub mod tools;
