//! A stdio MCP server for tests and manual verification: `echo`, `lookup` (which declares
//! itself read-only), `env` (reports a variable of its own environment), one resource
//! (`repo://info`) and one prompt (`greet`, with a required `name` and an optional `mood`).
//! Set `REPO_MCP_POKE=1` to replace the resource and the prompt list after two seconds, so a
//! `list_changed` refresh can be watched by hand.

#[path = "../../fontaine-tools/tests/mcp_common/server.rs"]
mod server;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() {
    let double = server::Double::with_resources(
        vec![
            server::echo_tool(),
            server::lookup_tool(),
            server::elicit_tool(),
            rmcp::model::Tool::new(
                "env",
                "Report an environment variable",
                std::sync::Arc::new(serde_json::Map::from_iter([(
                    "type".to_string(),
                    serde_json::json!("object"),
                )])),
            ),
        ],
        vec![server::info_resource()],
        vec![server::greet_prompt(), server::assistant_prompt()],
    );
    if std::env::var_os("REPO_MCP_POKE").is_some() {
        let poking = double.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            poking.replace_resources(Vec::new()).await;
            poking.replace_prompts(vec![server::greet_prompt()]).await;
        });
    }
    let running = double
        .serve(rmcp::transport::io::stdio())
        .await
        .expect("serve");
    let _ = running.waiting().await;
}
