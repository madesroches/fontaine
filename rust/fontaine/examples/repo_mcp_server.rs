//! A stdio MCP server for tests: `echo` and `lookup` (which declares itself read-only).

#[path = "../../fontaine-mcp/tests/common/server.rs"]
mod server;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() {
    let double = server::Double::with_tools(vec![server::echo_tool(), server::lookup_tool()]);
    let running = double
        .serve(rmcp::transport::io::stdio())
        .await
        .expect("serve");
    let _ = running.waiting().await;
}
