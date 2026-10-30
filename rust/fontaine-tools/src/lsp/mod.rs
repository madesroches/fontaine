//! Language-server code intelligence: the `lsp` tool, the connection to a server a user (or an
//! installed plugin) defined, and the diagnostics a server reports after an `edit` or `write`.
//! Servers execute code, so only user-scope configuration and installed plugins define them.

pub mod client;
pub mod diagnostics;
pub mod manager;
pub mod tool;

pub use client::Encoding;
pub use diagnostics::{DiagnosticStore, DiagnosticsStage};
pub use manager::{LspServerSpec, LspServers, LspWorkspace};
pub use tool::{LspArgs, LspTool, Operation};
