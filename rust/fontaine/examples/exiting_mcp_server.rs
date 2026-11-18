//! A stdio MCP server for tests of restarting a server that exits: its one tool, `echo`, answers
//! and then the process ends, so every call after the first meets a server that was started again.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};

#[derive(Clone)]
struct Exiting;

impl ServerHandler for Exiting {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(serde_json::Map::from_iter([(
            "type".to_string(),
            serde_json::json!("object"),
        )]));
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "echo",
            "Echo, then exit",
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let text = format!(
            "echo from process {}: {}",
            std::process::id(),
            serde_json::Value::Object(request.arguments.unwrap_or_default())
        );
        // After the answer has gone out.
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            std::process::exit(0);
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

#[tokio::main]
async fn main() {
    let running = Exiting
        .serve(rmcp::transport::io::stdio())
        .await
        .expect("serve");
    let _ = running.waiting().await;
}
