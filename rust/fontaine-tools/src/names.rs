/// Built-in tool names, as they appear in permission rules and agent type toolsets.
pub const BUILTIN_TOOL_NAMES: [&str; 11] = [
    "shell",
    "read",
    "edit",
    "write",
    "ls",
    "glob",
    "grep",
    "webfetch",
    "agent",
    "skill",
    "send_message",
];

/// MCP tools are namespaced `mcp__<server>__<tool>` and are not enumerable at config time.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

pub fn is_builtin(name: &str) -> bool {
    BUILTIN_TOOL_NAMES.contains(&name)
}

pub fn is_mcp(name: &str) -> bool {
    name.starts_with(MCP_TOOL_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_and_mcp_names_are_distinct_namespaces() {
        assert!(is_builtin("shell"));
        assert!(!is_builtin("mcp__github__create_issue"));
        assert!(is_mcp("mcp__github__create_issue"));
        assert!(!is_mcp("shell"));
    }
}
