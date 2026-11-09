//! A remote MCP server that drops mid-session is reconnected, over rmcp's real streamable-HTTP
//! transport: the one concurrency path a scripted transport cannot reproduce, because it needs
//! rmcp's own session teardown and re-initialize.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::build;
use fontaine_core::agent::{AgentRuntime, McpServerState, ReconnectTarget};
use fontaine_core::config::PermissionMode;
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use rig_core::tool::ToolContext;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A streamable-HTTP MCP endpoint that answers each POST with one JSON reply.
struct JsonRpc;

impl Respond for JsonRpc {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let id = body["id"].clone();
        let reply = |result: serde_json::Value| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        };
        match body["method"].as_str() {
            Some("initialize") => reply(json!({
                "protocolVersion": body["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "web", "version": "0.1.0"},
            })),
            Some("tools/list") => reply(json!({"tools": [{
                "name": "ping",
                "description": "Ping",
                "inputSchema": {"type": "object"},
            }]})),
            Some("tools/call") => reply(json!({"content": [{"type": "text", "text": "pong"}]})),
            _ => ResponseTemplate::new(202),
        }
    }
}

/// A server on `listener`'s port, which a later server can take over once this one is dropped.
async fn serve_on(listener: std::net::TcpListener) -> MockServer {
    let server = MockServer::builder().listener(listener).start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(JsonRpc)
        .mount(&server)
        .await;
    server
}

async fn rebind(port: u16) -> std::net::TcpListener {
    for _ in 0..100 {
        if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            return listener;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("port {port} did not come free");
}

async fn call(runtime: &AgentRuntime) -> Result<String, String> {
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    match runtime
        .tool_server()
        .toolset()
        .execute("mcp__web__ping", "{}", &mut context)
        .await
        .into_result()
    {
        Ok(output) => Ok(output.render()),
        Err(e) => Err(e.model_output().render()),
    }
}

fn state(runtime: &AgentRuntime) -> McpServerState {
    runtime
        .mcp_states()
        .into_iter()
        .find(|(name, _)| name == "web")
        .map(|(_, state)| state)
        .expect("the server has a slot")
}

async fn wait_for(runtime: &AgentRuntime, wanted: McpServerState) {
    for _ in 0..300 {
        if state(runtime) == wanted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the state is {:?}, not {wanted:?}", state(runtime));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_http_server_is_reconnected_when_it_comes_back_and_a_call_then_succeeds() {
    let first_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = first_listener.local_addr().unwrap().port();
    let first = serve_on(first_listener).await;

    let world = World::new();
    world.user_file(&format!(
        "[mcp.servers.web]\nurl = \"http://127.0.0.1:{port}/mcp\"\n\n[permissions]\nallow = [\"mcp__web__ping\"]\n"
    ));
    let mut runtime = build(&world, PermissionMode::Manual, vec![], false).runtime;
    let outcomes = runtime.connect_mcp(&[]).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].error, None);
    assert_eq!(state(&runtime), McpServerState::Connected);
    assert_eq!(call(&runtime).await.unwrap(), "pong");

    drop(first);
    // The failed POST is what shows the transport is dead: rmcp's worker never ends the
    // connection of a server that goes away.
    assert!(call(&runtime).await.is_err());
    wait_for(&runtime, McpServerState::Reconnecting { attempt: 1 }).await;

    let second = serve_on(rebind(port).await).await;
    wait_for(&runtime, McpServerState::Connected).await;
    assert_eq!(call(&runtime).await.unwrap(), "pong");

    // A manual reconnect restarts a live server, and refuses a name that has no slot.
    let outcomes = runtime
        .reconnect_mcp(ReconnectTarget::One("web".to_string()))
        .await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].error, None);
    assert_eq!(state(&runtime), McpServerState::Connected);
    assert_eq!(call(&runtime).await.unwrap(), "pong");

    let outcomes = runtime
        .reconnect_mcp(ReconnectTarget::One("nope".to_string()))
        .await;
    assert!(outcomes[0].error.as_deref().unwrap().contains("`nope`"));
    assert!(
        runtime
            .reconnect_mcp(ReconnectTarget::AllFailed)
            .await
            .is_empty()
    );
    drop(second);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_failed_at_startup_keeps_a_slot_and_reconnects_by_hand() {
    // Nothing listens on the port at first, so startup fails after its retries.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let world = World::new();
    world.user_file(&format!(
        "[mcp.servers.web]\nurl = \"http://127.0.0.1:{port}/mcp\"\n\n[permissions]\nallow = [\"mcp__web__ping\"]\n"
    ));
    let mut runtime = build(&world, PermissionMode::Manual, vec![], false).runtime;
    let outcomes = runtime.connect_mcp(&[]).await;
    assert!(outcomes[0].error.is_some());
    assert!(matches!(state(&runtime), McpServerState::Failed(_)));

    let server = serve_on(rebind(port).await).await;
    let outcomes = runtime.reconnect_mcp(ReconnectTarget::AllFailed).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].error, None);
    assert_eq!(state(&runtime), McpServerState::Connected);
    assert_eq!(call(&runtime).await.unwrap(), "pong");
    drop(server);
}
