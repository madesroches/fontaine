//! What a server lists beyond its tools: its resources and prompts. Both listings are advisory:
//! they feed the `@` mention popup, the `/mcp__<server>__<prompt>` commands and the
//! `list_mcp_resources` tool, and a failed pull never fails the connection that carried it.

use rig_rmcp::McpConnection;
use rmcp::model::ResourceContents;

/// One resource a server listed: readable content behind a URI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceSummary {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// One argument a prompt declared: `key=value` pairs the command line fills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptArgument {
    pub name: String,
    pub required: bool,
}

/// One prompt a server listed, invocable as `/mcp__<server>__<name>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptSummary {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<PromptArgument>,
}

/// Both listings of one server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listings {
    pub resources: Vec<ResourceSummary>,
    pub prompts: Vec<PromptSummary>,
}

impl Listings {
    /// Whether the server declared neither primitive: the common tools-only case.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty() && self.prompts.is_empty()
    }
}

/// Pulls both listings from `connection` once. Each listing is skipped when the server did not
/// declare the capability in its initialize result (`peer_info`); an absent `peer_info` means
/// no capabilities. `None` when a declared listing could not be pulled, so a refresh can keep
/// the listings it already holds.
pub async fn pull_listings(connection: &McpConnection) -> Option<Listings> {
    let Some(info) = connection.peer().peer_info() else {
        return Some(Listings::default());
    };
    let capabilities = &info.capabilities;
    let resources = if capabilities.resources.is_some() {
        let listed = connection.resources().await.ok()?;
        listed
            .into_iter()
            .map(|resource| ResourceSummary {
                uri: resource.uri,
                name: resource.name,
                description: resource.description,
                mime_type: resource.mime_type,
            })
            .collect()
    } else {
        Vec::new()
    };
    let prompts = if capabilities.prompts.is_some() {
        let listed = connection.peer().list_all_prompts().await.ok()?;
        listed
            .into_iter()
            .map(|prompt| PromptSummary {
                name: prompt.name,
                description: prompt.description,
                arguments: prompt
                    .arguments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|argument| PromptArgument {
                        name: argument.name,
                        required: argument.required.unwrap_or(false),
                    })
                    .collect(),
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(Listings { resources, prompts })
}

/// How the built-in resource tools reach the session's live MCP connections. Implemented by the
/// runtime over its connected servers, so this crate stays free of runtime types.
pub trait McpResourceQueries: Send + Sync {
    /// The name and current listings of every connected server, in name order.
    fn connected(&self) -> Vec<(String, Listings)>;

    /// Reads `uri` from the connected server `server`. `Err` names what is connected or what the
    /// server answered.
    fn read_resource<'a>(
        &'a self,
        server: &'a str,
        uri: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<Vec<ResourceContents>, String>>;
}

/// The no-server implementation: an empty listing and a refused read.
#[derive(Default)]
pub struct NoMcpResources;

impl McpResourceQueries for NoMcpResources {
    fn connected(&self) -> Vec<(String, Listings)> {
        Vec::new()
    }

    fn read_resource<'a>(
        &'a self,
        server: &'a str,
        _uri: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<Vec<ResourceContents>, String>> {
        Box::pin(async move { Err(format!("no MCP server named `{server}` is connected")) })
    }
}
