//! What a server lists beyond its tools: its resources and prompts. Both listings are advisory:
//! they feed the `@` mention popup, the `/mcp__<server>__<prompt>` commands and the
//! `list_mcp_resources` tool, and a failed pull never fails the connection that carried it.

use std::time::Duration;

use rig_rmcp::McpConnection;
use rmcp::model::ResourceContents;

/// How long one listing request may go unanswered before the pull gives up on that listing
/// and keeps its previous value. The two listings pull concurrently, so the bound covers
/// each, and a server that accepts a request but never answers cannot stall the other.
const LISTING_PULL_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Pulls both listings from `connection` once, starting from `previous`. Each listing is
/// skipped when the server did not declare the capability in its initialize result
/// (`peer_info`); an absent `peer_info` means no capabilities. The two listings pull
/// concurrently under their own deadlines: a declared listing that fails or goes unanswered
/// keeps the field's previous value and never stalls or erases the other.
pub async fn pull_listings(connection: &McpConnection, previous: &Listings) -> Listings {
    let Some(info) = connection.peer().peer_info() else {
        return Listings::default();
    };
    let capabilities = &info.capabilities;
    let mut listings = previous.clone();
    let resources = async {
        if capabilities.resources.is_none() {
            listings.resources = Vec::new();
            return;
        }
        if let Ok(Ok(listed)) =
            tokio::time::timeout(LISTING_PULL_TIMEOUT, connection.resources()).await
        {
            listings.resources = listed
                .into_iter()
                .map(|resource| ResourceSummary {
                    uri: resource.uri,
                    name: resource.name,
                    description: resource.description,
                    mime_type: resource.mime_type,
                })
                .collect();
        }
    };
    let prompts = async {
        if capabilities.prompts.is_none() {
            listings.prompts = Vec::new();
            return;
        }
        if let Ok(Ok(listed)) =
            tokio::time::timeout(LISTING_PULL_TIMEOUT, connection.peer().list_all_prompts()).await
        {
            listings.prompts = listed
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
                .collect();
        }
    };
    tokio::join!(resources, prompts);
    listings
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
