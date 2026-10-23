//! A stdio MCP server for tests: `echo`, `lookup` (declared read-only) and `env` (reports a
//! variable of its own environment).

#[path = "../tests/common/server.rs"]
mod server;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() {
    let double = server::Double::with_tools(vec![
        server::echo_tool(),
        server::lookup_tool(),
        rmcp::model::Tool::new(
            "env",
            "Report an environment variable",
            std::sync::Arc::new(serde_json::Map::from_iter([(
                "type".to_string(),
                serde_json::json!("object"),
            )])),
        ),
    ]);
    let running = double
        .serve(rmcp::transport::io::stdio())
        .await
        .expect("serve");
    let _ = running.waiting().await;
}
