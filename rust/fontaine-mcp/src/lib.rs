//! MCP servers as Fontaine tools. Each server's tools register behind the same gate as the
//! built-ins, always as `Arbitrary`, under `mcp__<server>__<tool>`; what a server declares about
//! a tool reaches the guardian as untrusted text and never earns a fast path.

pub mod register;
pub mod servers;

pub use register::{GuardedSink, mcp_tool_name};
pub use rig_rmcp::McpConnection as McpConnectionHandle;
pub use servers::{Connected, McpError, ServerSpec, Transport, connect, connect_transport};
