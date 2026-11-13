//! A headless run over a deferred MCP server: the prompt carries only names, the model searches,
//! then calls the loaded tool through the same gate.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct JsonRpc;

impl Respond for JsonRpc {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let id = body["id"].clone();
        let reply = |result: Value| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        };
        match body["method"].as_str() {
            Some("initialize") => reply(json!({
                "protocolVersion": body["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "web", "version": "0.1.0"},
            })),
            Some("tools/list") => reply(json!({"tools": [
                {"name": "ping", "description": "Ping the server",
                 "inputSchema": {"type": "object"}},
                {"name": "weather", "description": "Get the weather forecast",
                 "inputSchema": {"type": "object"}},
            ]})),
            Some("tools/call") => reply(json!({"content": [{"type": "text", "text": "pong"}]})),
            _ => ResponseTemplate::new(202),
        }
    }
}

fn names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .expect("the request carries tools")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_searches_then_calls_a_deferred_tool() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(JsonRpc)
        .mount(&mcp)
        .await;
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "call_1",
                "tool_search",
                json!({"query": "select:mcp__web__ping"}),
            ),
            Turn::tool("call_2", "mcp__web__ping", json!({})),
            Turn::text("the server said pong"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let base = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!(
        "{base}\n[mcp]\ntool_search = \"on\"\n\n[mcp.servers.web]\nurl = \"{}/mcp\"\n\n\
         [permissions]\nallow = [\"mcp__web__ping\"]\n",
        mcp.uri()
    ));
    let out = s
        .headless(&["--output-format", "stream-json", "ping the server"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(lines.last().unwrap()["result"], "the server said pong");

    let calls: Vec<&str> = lines
        .iter()
        .filter(|l| l["type"] == "tool_call")
        .map(|l| l["tool"].as_str().unwrap())
        .collect();
    assert_eq!(calls, ["tool_search", "mcp__web__ping"]);
    // The loaded tool went through the gate like any other; `tool_search` is read-only.
    let decisions: Vec<(&str, &str)> = lines
        .iter()
        .filter(|l| l["type"] == "gate_decision")
        .map(|l| (l["tier"].as_str().unwrap(), l["decision"].as_str().unwrap()))
        .collect();
    assert_eq!(decisions.len(), 2, "{decisions:?}");
    assert_eq!(decisions[0], ("read_only_fast_path", "allow"));
    assert_eq!(decisions[1].1, "allow");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 3);
    let first = names(&requests[0]);
    assert!(first.contains(&"tool_search".to_string()), "{first:?}");
    assert!(!first.iter().any(|n| n.starts_with("mcp__")), "{first:?}");
    let prompt = requests[0].to_string();
    assert!(prompt.contains("mcp__web__ping") && prompt.contains("mcp__web__weather"));
    assert!(!prompt.contains("Get the weather forecast"));
    let after = names(&requests[1]);
    assert_eq!(
        after
            .iter()
            .filter(|n| n.starts_with("mcp__"))
            .collect::<Vec<_>>(),
        ["mcp__web__ping"]
    );
    assert!(requests[1].to_string().contains("Ping the server"));
}
