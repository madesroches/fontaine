//! What fills the model's context window, as the heuristic token counter rig-memory's compaction
//! threshold uses estimates it.

use std::fmt;
use std::path::PathBuf;

use rig_core::completion::{Message, ToolDefinition};
use rig_memory::{HeuristicTokenCounter, TokenCounter};

/// The prefix of the tools an MCP server provides.
const MCP_PREFIX: &str = "mcp__";

/// Names with their estimated tokens.
pub type Named = Vec<(String, u64)>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextUsage {
    /// The main model, as `account/model`.
    pub model: String,
    /// The main model's window, from the catalog.
    pub window: Option<u32>,
    /// The conversation size above which compaction runs, `None` when it never does.
    pub autocompact_at: Option<u64>,
    /// `[context] autocompact_window`.
    pub configured: Option<u64>,
    /// The summarizer's reserve of the window.
    pub reserve: u64,
    /// Whether the model is in the catalog at all.
    pub listed: bool,
    /// The base layer (or `--system-prompt`) and `--append-system-prompt`.
    pub system_prompt: u64,
    pub output_style: u64,
    pub instructions: Vec<(PathBuf, u64)>,
    /// The skill index in the prompt.
    pub skills: u64,
    /// Built-in and extension tools.
    pub tools: Vec<(String, u64)>,
    /// `mcp__*` tools.
    pub mcp_tools: Vec<(String, u64)>,
    /// The conversation's messages.
    pub messages: u64,
}

impl ContextUsage {
    /// Everything the next request would carry.
    pub fn total(&self) -> u64 {
        let sum = |items: &[(String, u64)]| items.iter().map(|(_, n)| n).sum::<u64>();
        self.system_prompt
            + self.output_style
            + self.instructions.iter().map(|(_, n)| n).sum::<u64>()
            + self.skills
            + sum(&self.tools)
            + sum(&self.mcp_tools)
            + self.messages
    }
}

/// The estimated tokens of prompt text.
pub fn text_tokens(text: &str) -> u64 {
    HeuristicTokenCounter::default().count(&Message::system(text)) as u64
}

/// The tools split into built-in and MCP ones, each with its estimated tokens.
pub fn tool_tokens(defs: &[ToolDefinition]) -> (Named, Named) {
    let counter = HeuristicTokenCounter::default();
    let (mcp, builtin): (Vec<_>, Vec<_>) = defs
        .iter()
        .map(|def| (def.name.to_string(), counter.count_tool(def) as u64))
        .partition(|(name, _)| name.starts_with(MCP_PREFIX));
    (builtin, mcp)
}

/// The usage could not be read.
#[derive(Debug)]
pub enum ContextError {
    Conversation(rig_memory::SessionError),
    Tools(Box<rig_agent::tool::server::ToolServerError>),
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ContextError::Conversation(_) => "the conversation could not be read",
            ContextError::Tools(_) => "the tool definitions could not be read",
        })
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ContextError::Conversation(e) => Some(e),
            ContextError::Tools(e) => Some(e.as_ref()),
        }
    }
}
