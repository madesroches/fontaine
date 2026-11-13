//! Deferred MCP tool loading through the whole runtime: a scripted model over a streamable-HTTP
//! MCP server sees only names until it searches, loads definitions in registry order, and a
//! call to a deferred tool never gets past the gate unchecked.

mod common;

use std::time::Duration;

use common::World;
use common::runtime::{Built, build};
use fontaine_core::config::PermissionMode;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::test_utils::{MockStreamEvent, mock_final};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct JsonRpc;

impl Respond for JsonRpc {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let id = body["id"].clone();
        let reply = |result: serde_json::Value| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        };
        let tool = |name: &str, description: &str| json!({"name": name, "description": description, "inputSchema": {"type": "object"}});
        match body["method"].as_str() {
            Some("initialize") => reply(json!({
                "protocolVersion": body["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "web", "version": "0.1.0"},
            })),
            Some("tools/list") => reply(json!({"tools": [
                tool("ping", "Ping the server"),
                tool("weather", "Get the weather forecast"),
                tool("echo", "Echo text back"),
            ]})),
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

fn world(server: &MockServer, setting: &str) -> World {
    let world = World::new();
    world.user_file(&format!(
        "[mcp]\ntool_search = \"{setting}\"\n\n[mcp.servers.web]\nurl = \"{}/mcp\"\n\n\
         [permissions]\nallow = [\"mcp__web__ping\", \"mcp__web__echo\"]\n",
        server.uri()
    ));
    world
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
    ]
}

fn text(answer: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(answer.to_string()),
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
    ]
}

async fn connected(world: &World, turns: Vec<Vec<MockStreamEvent>>) -> Built {
    let mut built = build(world, PermissionMode::Manual, turns, false);
    let outcomes = built.runtime.connect_mcp(&[]).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].error, None);
    for _ in 0..200 {
        if built
            .runtime
            .tool_names()
            .iter()
            .any(|n| n == "mcp__web__ping")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    built.runtime.refresh_before_turn().expect("the rebuild");
    built
}

async fn run(built: &Built) {
    built.runtime.begin_turn("go", false);
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "go".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(8),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: None,
            max_tokens: None,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
}

fn advertised(built: &Built, request: usize) -> Vec<String> {
    built.main.requests()[request]
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

fn mcp(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .filter(|n| n.starts_with("mcp__"))
        .collect()
}

/// The MCP tools the audit log recorded a decision for.
fn audited_mcp_tools(world: &World) -> Vec<String> {
    let mut tools = Vec::new();
    let Ok(files) = std::fs::read_dir(world.data.join("audit")) else {
        return tools;
    };
    for file in files.flatten() {
        for line in std::fs::read_to_string(file.path())
            .unwrap_or_default()
            .lines()
        {
            let record: serde_json::Value = serde_json::from_str(line).expect("an audit record");
            if let Some(tool) = record["tool"].as_str().filter(|t| t.starts_with("mcp__")) {
                tools.push(tool.to_string());
            }
        }
    }
    tools
}

async fn tool_calls_received(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .is_ok_and(|b| b["method"] == "tools/call")
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unloaded_deferred_tool_is_absent_until_tool_search_loads_it_in_registry_order() {
    let server = endpoint().await;
    let world = world(&server, "on");
    let built = connected(
        &world,
        vec![
            call("c1", "mcp__web__ping", json!({})),
            call(
                "c2",
                "tool_search",
                json!({"query": "select:mcp__web__weather,mcp__web__echo"}),
            ),
            call("c3", "mcp__web__echo", json!({})),
            text("done"),
        ],
    )
    .await;
    run(&built).await;

    let requests = built.main.requests();
    assert_eq!(requests.len(), 4);
    // Before the search: only the built-ins and `tool_search`, whatever the model called.
    for before in [0, 1] {
        let names = advertised(&built, before);
        assert!(names.contains(&"tool_search".to_string()), "{names:?}");
        assert!(mcp(names).is_empty(), "request {before}");
    }
    // After it: the two loaded tools at their registry positions, not in load order, and the
    // third tool still deferred.
    for after in [2, 3] {
        assert_eq!(
            mcp(advertised(&built, after)),
            ["mcp__web__echo", "mcp__web__weather"],
            "request {after}"
        );
    }
    // The prompt names every deferred tool and nothing of their definitions.
    let first = format!("{:?}", requests[0]);
    for name in ["mcp__web__ping", "mcp__web__weather", "mcp__web__echo"] {
        assert!(first.contains(name), "{name} is missing from the prompt");
    }
    assert!(first.contains("Deferred MCP tools"));
    assert!(!first.contains("Get the weather forecast"));

    // The unloaded call was refused with the search-first reason, reached neither the gate nor
    // the server; the loaded one took the same gate as any MCP call and ran once.
    let refused = format!("{:?}", requests[1]);
    assert!(refused.contains("tool_search"), "{refused}");
    assert!(
        refused.contains("whose definition is not loaded"),
        "{refused}"
    );
    let loaded = format!("{:?}", requests[2]);
    assert!(loaded.contains("Get the weather forecast"), "{loaded}");
    assert_eq!(tool_calls_received(&server).await, 1);
    // `/context` separates the names still deferred from the definitions that were loaded.
    let usage = built.runtime.context_usage().await.expect("the usage");
    assert!(usage.mcp_tools.is_empty(), "{usage:?}");
    assert_eq!(
        usage
            .loaded_tools
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["mcp__web__echo", "mcp__web__weather"]
    );
    assert_eq!(usage.deferred_tools, ["mcp__web__ping"]);
    assert!(usage.tools.iter().any(|(n, _)| n == "tool_search"));

    let audited = audited_mcp_tools(&world);
    assert_eq!(audited, ["mcp__web__echo"], "{audited:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_session_starts_with_nothing_loaded() {
    let server = endpoint().await;
    let world = world(&server, "on");
    let first = connected(
        &world,
        vec![
            call("c1", "tool_search", json!({"query": "ping"})),
            text("done"),
        ],
    )
    .await;
    run(&first).await;
    assert_eq!(mcp(advertised(&first, 1)), ["mcp__web__ping"]);

    let second = connected(&world, vec![text("again")]).await;
    run(&second).await;
    assert!(mcp(advertised(&second, 0)).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_deferral_off_every_mcp_tool_is_advertised_and_tool_search_is_not() {
    let server = endpoint().await;
    let world = world(&server, "off");
    let built = connected(&world, vec![text("done")]).await;
    run(&built).await;
    let names = advertised(&built, 0);
    assert!(!names.contains(&"tool_search".to_string()), "{names:?}");
    assert_eq!(
        mcp(names),
        ["mcp__web__echo", "mcp__web__ping", "mcp__web__weather"]
    );
    assert!(!format!("{:?}", built.main.requests()[0]).contains("Deferred MCP tools"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_stays_off_below_the_threshold() {
    let server = endpoint().await;
    let world = world(&server, "auto");
    let built = connected(&world, vec![text("done")]).await;
    run(&built).await;
    let names = advertised(&built, 0);
    assert!(!names.contains(&"tool_search".to_string()), "{names:?}");
    assert_eq!(mcp(names).len(), 3);
}
