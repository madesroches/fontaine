//! MCP resources and prompts end to end at the core: listings pulled after connect, `/mcp`
//! counts, the `@` mention slot, the slash-prompt surface, `run_mcp_prompt` and the gated
//! `list_mcp_resources`/`read_mcp_resource` tools — over a scripted streamable-HTTP server.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::build;
use fontaine_core::agent::{AgentRuntime, McpServerState};
use fontaine_core::config::PermissionMode;
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use rig_core::tool::ToolContext;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A streamable-HTTP MCP endpoint with one tool, one resource and two prompts.
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
                "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                "serverInfo": {"name": "web", "version": "0.1.0"},
            })),
            Some("tools/list") => reply(json!({"tools": [{
                "name": "ping",
                "description": "Ping",
                "inputSchema": {"type": "object"},
            }]})),
            Some("resources/list") => reply(json!({"resources": [{
                "uri": "web://info",
                "name": "web_info",
                "description": "Web server information",
                "mimeType": "text/plain",
            }]})),
            Some("resources/read") => {
                let uri = body["params"]["uri"].as_str().unwrap_or_default();
                if uri == "web://info" {
                    reply(json!({"contents": [{
                        "uri": uri,
                        "mimeType": "text/plain",
                        "text": "the web server info",
                    }]}))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": format!("unknown resource {uri}")},
                    }))
                }
            }
            Some("prompts/list") => reply(json!({"prompts": [
                {
                    "name": "greet",
                    "description": "Greet someone",
                    "arguments": [
                        {"name": "name", "required": true},
                        {"name": "mood"},
                    ],
                },
                {
                    "name": "assistant-note",
                    "description": "A note from the assistant",
                },
            ]})),
            Some("prompts/get") => {
                let name = body["params"]["name"].as_str().unwrap_or_default();
                match name {
                    "greet" => {
                        let arguments = body["params"]["arguments"].clone();
                        let who = arguments["name"].as_str().unwrap_or_default();
                        let mood = arguments["mood"].as_str().unwrap_or("neutral");
                        reply(json!({"messages": [
                            {"role": "user", "content": {"type": "text", "text": format!("Hello, {who}!")}},
                            {"role": "user", "content": {"type": "text", "text": format!("Mood: {mood}.")}},
                        ]}))
                    }
                    "assistant-note" => reply(json!({"messages": [
                        {"role": "assistant", "content": {"type": "text", "text": "an assistant line"}},
                    ]})),
                    other => ResponseTemplate::new(200).set_body_json(json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": format!("unknown prompt {other}")},
                    })),
                }
            }
            Some("tools/call") => reply(json!({"content": [{"type": "text", "text": "pong"}]})),
            _ => ResponseTemplate::new(202),
        }
    }
}

async fn endpoint() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(JsonRpc)
        .mount(&server)
        .await;
    server
}

fn world(server: &MockServer) -> World {
    let world = World::new();
    world.user_file(&format!(
        "[mcp.servers.web]\nurl = \"{}/mcp\"\n\n\
         [permissions]\nallow = [\"mcp__web__ping\", \"read_mcp_resource\", \"list_mcp_resources\"]\n",
        server.uri()
    ));
    world
}

async fn connect(world: &World) -> AgentRuntime {
    let mut runtime = build(world, PermissionMode::Manual, vec![], false).runtime;
    let outcomes = runtime.connect_mcp(&[]).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].error, None);
    runtime
}

async fn wait_for(mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the condition did not arrive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_lists_resources_and_prompts_and_the_runtime_surfaces_them() {
    let server = endpoint().await;
    let world = world(&server);
    let mut runtime = connect(&world).await;

    // The watcher pulls the listings after install; the counts and surfaces follow.
    wait_for(|| {
        runtime.mcp_states()
            == vec![(
                "web".to_string(),
                McpServerState::Connected {
                    tools: 1,
                    resources: 1,
                    prompts: 2,
                },
            )]
    })
    .await;

    let mentions = fontaine_tools::lock::read(&runtime.mcp_mentions, "test").clone();
    assert_eq!(
        mentions.as_slice(),
        [fontaine_core::agent::McpResourceSummary {
            server: "web".to_string(),
            uri: "web://info".to_string(),
            name: "web_info".to_string(),
            description: Some("Web server information".to_string()),
        }]
    );

    let prompts = runtime.slash_prompts();
    assert_eq!(prompts.len(), 2);
    let greet = prompts
        .iter()
        .find(|(name, _, _)| name == "mcp__web__greet")
        .expect("the greet command");
    assert_eq!(
        greet.1.as_deref(),
        Some("name=<value> [mood=<value>]"),
        "{prompts:?}"
    );
    assert_eq!(greet.2, "Greet someone");
    assert!(
        prompts
            .iter()
            .any(|(name, hint, _)| name == "mcp__web__assistant-note" && hint.as_deref().is_none()),
        "{prompts:?}"
    );

    // The built-in tools reach the same server through the registry's live view.
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    let toolset = runtime.tool_server().toolset().clone();
    let listed = toolset
        .execute("list_mcp_resources", "{}", &mut context)
        .await;
    let listed = match listed.into_result() {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    };
    assert!(
        listed.contains("web: web://info — web_info · Web server information (text/plain)"),
        "{listed}"
    );
    let read = toolset
        .execute(
            "read_mcp_resource",
            json!({"server": "web", "uri": "web://info"})
                .to_string()
                .as_str(),
            &mut context,
        )
        .await;
    match read.into_result() {
        Ok(output) => assert_eq!(output.render(), "the web server info\n"),
        Err(e) => panic!("the read failed: {}", e.model_output().render()),
    }
    let unknown = toolset
        .execute(
            "read_mcp_resource",
            json!({"server": "web", "uri": "web://nope"})
                .to_string()
                .as_str(),
            &mut context,
        )
        .await;
    let unknown = match unknown.into_result() {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    };
    assert!(unknown.contains("no resource `web://nope`"), "{unknown}");
    let unknown = toolset
        .execute(
            "read_mcp_resource",
            json!({"server": "ghost", "uri": "web://info"})
                .to_string()
                .as_str(),
            &mut context,
        )
        .await;
    let unknown = match unknown.into_result() {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    };
    assert!(unknown.contains("no MCP server named `ghost`"), "{unknown}");

    // A prompt fetches and flattens; a missing required argument and an assistant-role answer
    // fail with a notice and no turn.
    let prompt = runtime
        .run_mcp_prompt("mcp__web__greet", "name=Ada")
        .await
        .expect("the prompt");
    assert_eq!(prompt, "Hello, Ada!\n\nMood: neutral.");
    let error = runtime
        .run_mcp_prompt("mcp__web__greet", "mood=calm")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing: name"), "{error}");
    assert!(error.contains("For example"), "{error}");
    let error = runtime
        .run_mcp_prompt("mcp__web__greet", "name=Ada grumpy")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("`grumpy` is not a `key=value` pair"),
        "{error}"
    );
    let error = runtime
        .run_mcp_prompt("mcp__web__assistant-note", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Assistant"), "{error}");
    let error = runtime
        .run_mcp_prompt("mcp__web__nope", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no MCP prompt named `mcp__web__nope`"),
        "{error}"
    );

    // A server going away drops the mentions, the prompts and the counts with its slot.
    drop(server);
    runtime.stop_mcp_server("web").await.expect("the slot");
    assert!(runtime.mcp_states().is_empty());
    assert!(fontaine_tools::lock::read(&runtime.mcp_mentions, "test").is_empty());
    assert!(runtime.slash_prompts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_driven_loss_bumps_the_listing_revision_the_menu_caches_on() {
    let server = endpoint().await;
    let world = world(&server);
    let runtime = connect(&world).await;
    wait_for(|| {
        matches!(
            runtime.mcp_states().first().map(|(_, state)| state),
            Some(McpServerState::Connected { .. })
        )
    })
    .await;
    let before = runtime.slash_prompts_revision();
    // The overlay answers every request with a 500: rmcp's HTTP worker never ends the
    // connection of a dead server, so the failed request is what marks the transport dead.
    // Dropping the listener alone is not enough — its keep-alive connections keep serving.
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&server)
        .await;
    // The watcher's refresh callback must move the revision surfaces cache prompt rows on,
    // not only rebuild the mention snapshot.
    let mut failed = false;
    for _ in 0..10 {
        if runtime
            .run_mcp_prompt("mcp__web__greet", "name=Ada")
            .await
            .is_err()
        {
            failed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(failed, "the dead transport never surfaced");
    wait_for(|| {
        matches!(
            runtime.mcp_states().first().map(|(_, state)| state),
            Some(McpServerState::Reconnecting { .. })
        )
    })
    .await;
    assert!(runtime.slash_prompts_revision() > before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_server_read_names_only_the_live_connections() {
    let server = endpoint().await;
    let world = World::new();
    world.user_file(&format!(
        "[mcp.servers.web]\nurl = \"{}/mcp\"\n\n\
         [mcp.servers.dead]\ncommand = \"fontaine-no-such-mcp-binary\"\n\n\
         [permissions]\nallow = [\"read_mcp_resource\", \"list_mcp_resources\"]\n",
        server.uri()
    ));
    let mut runtime = build(&world, PermissionMode::Manual, vec![], false).runtime;
    let outcomes = runtime.connect_mcp(&[]).await;
    assert_eq!(outcomes.len(), 2);
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.server == "dead" && outcome.error.is_some()),
        "{outcomes:?}"
    );
    // The failed slot stays for `/mcp reconnect`.
    assert!(
        matches!(
            runtime
                .mcp_states()
                .iter()
                .find(|(name, _)| name == "dead")
                .map(|(_, state)| state),
            Some(McpServerState::Failed(_))
        ),
        "{:?}",
        runtime.mcp_states()
    );
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    let unknown = runtime
        .tool_server()
        .toolset()
        .execute(
            "read_mcp_resource",
            json!({"server": "ghost", "uri": "web://info"})
                .to_string()
                .as_str(),
            &mut context,
        )
        .await;
    let unknown = match unknown.into_result() {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    };
    assert!(unknown.contains("no MCP server named `ghost`"), "{unknown}");
    assert!(
        unknown.contains("(connected: web)"),
        "the failed server must not be labelled connected: {unknown}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tools_only_server_keeps_the_counts_zero_and_every_surface_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(|request: &Request| {
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
                    "serverInfo": {"name": "plain", "version": "0.1.0"},
                })),
                Some("tools/list") => reply(json!({"tools": [{
                    "name": "ping",
                    "description": "Ping",
                    "inputSchema": {"type": "object"},
                }]})),
                Some("tools/call") => reply(json!({"content": [{"type": "text", "text": "pong"}]})),
                _ => ResponseTemplate::new(202),
            }
        })
        .mount(&server)
        .await;
    let world = world(&server);
    let runtime = connect(&world).await;
    wait_for(|| {
        runtime.mcp_states()
            == vec![(
                "web".to_string(),
                McpServerState::Connected {
                    tools: 1,
                    resources: 0,
                    prompts: 0,
                },
            )]
    })
    .await;
    assert!(runtime.slash_prompts().is_empty());
    assert!(fontaine_tools::lock::read(&runtime.mcp_mentions, "test").is_empty());
    let error = runtime
        .run_mcp_prompt("mcp__web__greet", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no MCP prompt named"), "{error}");
    let mut context = ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    let listed = runtime
        .tool_server()
        .toolset()
        .execute("list_mcp_resources", "{}", &mut context)
        .await;
    let listed = match listed.into_result() {
        Ok(output) => output.render(),
        Err(e) => e.model_output().render(),
    };
    assert_eq!(listed, "no connected MCP server declared resources");
    drop(server);
}
