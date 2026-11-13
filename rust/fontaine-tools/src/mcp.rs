//! MCP servers as Fontaine tools. Each server's tools register behind the same gate as the
//! built-ins, always as `Arbitrary`, under `mcp__<server>__<tool>`; what a server declares about
//! a tool reaches the guardian as untrusted text and never earns a fast path. The one declared
//! value that changes a decision is `requiresUserInteraction`, and it only forces a prompt.
//! Oversized results are saved to a file instead of entering the model's context, and a server's
//! request for input is shown to the person, never answered by the harness.

pub mod auth;
pub mod deferral;
pub mod elicitation;
pub mod listings;
mod meta;
pub mod output;
pub mod register;
pub mod servers;

pub use deferral::{DeferMode, Deferral, LoadedSets, TOOL_SEARCH, search};
pub use elicitation::{
    CallGuard, CancelAll, Choice, ElicitForm, ElicitReply, ElicitRequest, ElicitUrl, Elicitor,
    Field, FieldKind, TextFormat, openable_url,
};
pub use listings::{
    Listings, McpResourceQueries, NoMcpResources, PromptArgument, PromptSummary, ResourceSummary,
    pull_listings,
};
pub use meta::{MAX_RESULT_SIZE_CHARS, REQUIRES_USER_INTERACTION};
pub use output::OutputLimits;
pub use register::{GuardedSink, mcp_tool_name};
pub use rig_rmcp::McpConnection as McpConnectionHandle;
pub use servers::{
    CONNECT_TIMEOUT, CallScope, Connected, McpError, McpErrorKind, McpOptions, ServerSpec,
    Transport, connect, connect_transport,
};
