//! A tool the embedding program provides. It is registered beside the built-ins and goes through
//! the same gate on every call, as an arbitrary-risk tool: the host cannot declare it safe.

use futures::future::BoxFuture;
use rig_core::tool::ToolExecutionError;
use serde_json::Value;

/// The longest name a host tool may have.
pub const MAX_NAME_CHARS: usize = 64;

pub trait HostTool: Send + Sync {
    /// The name the model calls it by: 1 to 64 characters of `A-Za-z0-9_-`, and not one of
    /// Fontaine's own tool names or an `mcp__` name.
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    /// The JSON Schema of the arguments.
    fn parameters(&self) -> Value;

    /// Runs the call. The gate has allowed it by now, and what is returned passes the result
    /// pipeline before the model sees it.
    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<String, ToolExecutionError>>;
}

/// Why `name` cannot name a host tool, if it cannot.
pub fn name_refusal(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a host tool name cannot be empty".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Some(format!(
            "the host tool name `{name}` is longer than {MAX_NAME_CHARS} characters"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Some(format!(
            "the host tool name `{name}` may only hold A-Z, a-z, 0-9, `_` and `-`"
        ));
    }
    if crate::tools::descriptor::find(name).is_some() {
        return Some(format!(
            "the host tool name `{name}` is one of Fontaine's own tools"
        ));
    }
    if name.starts_with("mcp__") {
        return Some(format!(
            "the host tool name `{name}` starts with `mcp__`, which is for MCP tools"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_tool_name_rules() {
        assert_eq!(name_refusal("my_tool-2"), None);
        assert_eq!(name_refusal(&"a".repeat(64)), None);
        for bad in [
            String::new(),
            "a".repeat(65),
            "has space".to_string(),
            "dots.here".to_string(),
            "shell".to_string(),
            "read".to_string(),
            "mcp__srv__tool".to_string(),
        ] {
            assert!(name_refusal(&bad).is_some(), "{bad:?}");
        }
    }
}
