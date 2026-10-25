/// What a tool call can do, assigned once when the tool is registered. The class is the
/// deterministic half of the permission decision: it never comes from a tool's own hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RiskClass {
    /// Reads only. It earns the fast path solely when every path it names is inside a
    /// workspace root; the label alone earns nothing.
    ReadOnly,
    /// Writes files that sit inside a workspace root.
    WorkspaceWrite,
    /// Anything else: a shell, the network, an MCP tool.
    Arbitrary,
}

impl RiskClass {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskClass::ReadOnly => "read_only",
            RiskClass::WorkspaceWrite => "workspace_write",
            RiskClass::Arbitrary => "arbitrary",
        }
    }
}

/// Where a registered tool comes from; reported on every `tool_result` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolSource {
    Builtin,
    Mcp,
}

impl ToolSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolSource::Builtin => "builtin",
            ToolSource::Mcp => "mcp",
        }
    }
}

/// The class of any tool this build registers by name.
pub fn registered_risk(name: &str) -> Option<RiskClass> {
    crate::descriptor::find(name).map(|d| d.risk)
}
