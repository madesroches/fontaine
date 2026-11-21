//! Anthropic's own deferred tool loading through the whole runtime, against a wiremock Messages
//! endpoint: the deferred tools ride `tools[]` once and a `tool_search` result goes back as
//! `tool_reference` blocks, a host that rejects the fields gets the client-side route, and the
//! setting and the model turn the route off.

#[path = "core_common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::World;
use common::runtime::build_with_approver;
use fontaine_sdk::core::agent::AgentRuntime;
use fontaine_sdk::core::config::PermissionMode;
use fontaine_sdk::core::credentials;
use fontaine_sdk::core::models::{self, CostBasis, ResolveParams, Switcher};
use fontaine_sdk::core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_sdk::core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MODEL: &str = "claude-sonnet-4-6";

struct JsonRpc;

impl Respond for JsonRpc {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let id = body["id"].clone();
        let reply = |result: Value| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        };
        let tool = |name: &str, description: &str| json!({"name": name, "description": description, "inputSchema": {"type": "object"}});
        match body["method"].as_str() {
            Some("server/discover") => ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "Method not found"},
            })),
            Some("initialize") => reply(json!({
                "protocolVersion": body["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "web", "version": "0.1.0"},
            })),
            Some("tools/list") => reply(json!({"tools": [
                tool("ping", "Ping the server"),
                tool("weather", "Get the weather forecast"),
            ]})),
            _ => ResponseTemplate::new(202),
        }
    }
}

fn sse(events: &[Value]) -> ResponseTemplate {
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

fn start() -> Value {
    json!({"type": "message_start", "message": {
        "id": "msg_1", "type": "message", "role": "assistant", "model": MODEL,
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 1, "output_tokens": 1}}})
}

fn end(reason: &str) -> [Value; 2] {
    [
        json!({"type": "message_delta",
               "delta": {"stop_reason": reason, "stop_sequence": null},
               "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ]
}

fn searches() -> ResponseTemplate {
    let [delta, stop] = end("tool_use");
    sse(&[
        start(),
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "tool_use", "id": "toolu_1", "name": "tool_search", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "input_json_delta", "partial_json": "{\"query\":\"select:mcp__web__weather\"}"}}),
        json!({"type": "content_block_stop", "index": 0}),
        delta,
        stop,
    ])
}

fn answers() -> ResponseTemplate {
    let [delta, stop] = end("end_turn");
    sse(&[
        start(),
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": "done"}}),
        json!({"type": "content_block_stop", "index": 0}),
        delta,
        stop,
    ])
}

/// A Messages endpoint that refuses any request whose body contains `reject`, searches for a tool
/// on a first prompt and answers once it has a tool result.
struct Messages {
    reject: Option<&'static str>,
}

impl Respond for Messages {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&request.body);
        if self.reject.is_some_and(|needle| body.contains(needle)) {
            return ResponseTemplate::new(400).set_body_json(json!({
                "type": "error",
                "error": {"type": "invalid_request_error",
                          "message": format!("{}: Extra inputs are not permitted", self.reject.unwrap())},
            }));
        }
        if body.contains("\"tool_result\"") {
            answers()
        } else {
            searches()
        }
    }
}

#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Approver for Warnings {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

struct Rig {
    runtime: AgentRuntime,
    messages: MockServer,
    warnings: Arc<Warnings>,
    _mcp: MockServer,
    _world: World,
}

async fn rig(reject: Option<&'static str>, mcp_extra: &str, model: &str) -> Rig {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(JsonRpc)
        .mount(&mcp)
        .await;
    let messages = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Messages { reject })
        .mount(&messages)
        .await;
    let world = World::new();
    world.user_file(&format!(
        "[mcp]\ntool_search = \"on\"\n{mcp_extra}\n[mcp.servers.web]\nurl = \"{}/mcp\"\n\n\
         [accounts.main]\nkind = \"anthropic\"\nbase_url = \"{}\"\nmodels = [\"{model}\"]\n\n\
         [model]\nmodel = \"main/{model}\"\n",
        mcp.uri(),
        messages.uri()
    ));
    credentials::store(&world.data, "main", "sk-main").unwrap();
    let config = world.load().unwrap();
    let env: fontaine_sdk::core::agent::EnvLookup = Arc::new(|_| None);
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &world.data,
        env: env.clone(),
        model_override: None,
        session_id: None,
    })
    .await
    .unwrap();
    let switcher = Switcher::new(config, world.data.clone(), env, None);
    let warnings = Arc::new(Warnings::default());
    let mut runtime = build_with_approver(
        &world,
        PermissionMode::Manual,
        resolved,
        Some(switcher),
        false,
        Default::default(),
        None,
        warnings.clone(),
    );
    let outcomes = runtime.connect_mcp(&[]).await;
    assert_eq!(outcomes[0].error, None);
    for _ in 0..200 {
        if runtime
            .tool_names()
            .iter()
            .any(|n| n == "mcp__web__weather")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    runtime.refresh_before_turn().expect("the rebuild");
    Rig {
        runtime,
        messages,
        warnings,
        _mcp: mcp,
        _world: world,
    }
}

impl Rig {
    async fn run(&self, model: &str) {
        let runtime = &self.runtime;
        runtime.begin_turn("go", false);
        let outcome = run_turn(
            TurnParams {
                served: Default::default(),
                agent: &runtime.agent,
                state: &runtime.state,
                prompt: "go".to_string(),
                attachments: Vec::new(),
                modalities: rig_core::catalog::Modalities::default(),
                max_turns: Some(8),
                cost: CostBasis::Billed,
                model: model.to_string(),
                provider: "anthropic".to_string(),
                native_search: false,
                deferral: Some(runtime.deferral()),
                generation_options: runtime.turn_generation_options(),
                max_tokens: runtime.turn_max_tokens(),
            },
            &mut |_| Flow::Continue,
        )
        .await;
        assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    }

    async fn requests(&self) -> Vec<Value> {
        self.messages
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/v1/messages")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    fn fallback_warnings(&self) -> usize {
        self.warnings
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.contains("provider-side tool loading"))
            .count()
    }
}

fn tool<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no tool {name} in {body}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_tools_ride_the_request_once_and_a_load_leaves_the_prefix_alone() {
    let rig = rig(None, "", MODEL).await;
    rig.run(MODEL).await;
    let requests = rig.requests().await;
    assert_eq!(requests.len(), 2, "{requests:#?}");
    for body in &requests {
        assert_eq!(tool(body, "mcp__web__weather")["defer_loading"], true);
        assert_eq!(tool(body, "mcp__web__ping")["defer_loading"], true);
        assert!(tool(body, "tool_search").get("defer_loading").is_none());
    }
    assert_eq!(
        requests[0]["tools"], requests[1]["tools"],
        "loading a tool changes nothing in tools[]"
    );
    let result = &requests[1]["messages"].as_array().unwrap().last().unwrap()["content"][0];
    assert_eq!(
        result["content"],
        json!([{"type": "tool_reference", "tool_name": "mcp__web__weather"}]),
        "{result}"
    );
    assert_eq!(rig.fallback_warnings(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_rejects_defer_loading_on_the_first_request_gets_the_client_side_route() {
    let rig = rig(Some("defer_loading"), "", MODEL).await;
    rig.run(MODEL).await;
    let requests = rig.requests().await;
    assert_eq!(requests.len(), 3, "{requests:#?}");
    assert!(requests[0].to_string().contains("defer_loading"));
    for body in &requests[1..] {
        assert!(!body.to_string().contains("defer_loading"), "{body}");
        assert!(!body.to_string().contains("tool_reference"), "{body}");
    }
    let last = requests[2]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["content"][0]["content"][0]["type"], "text", "{last}");
    assert_eq!(rig.fallback_warnings(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejection_on_a_later_request_flips_the_route_too() {
    let rig = rig(Some("tool_reference"), "", MODEL).await;
    rig.run(MODEL).await;
    let requests = rig.requests().await;
    assert_eq!(requests.len(), 3, "{requests:#?}");
    assert!(requests[0].to_string().contains("defer_loading"));
    assert!(requests[1].to_string().contains("tool_reference"));
    for body in &requests[2..] {
        assert!(!body.to_string().contains("defer_loading"), "{body}");
        assert!(!body.to_string().contains("tool_reference"), "{body}");
    }
    assert_eq!(rig.fallback_warnings(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_setting_keeps_the_client_side_route() {
    let rig = rig(None, "native_tool_search = false\n", MODEL).await;
    rig.run(MODEL).await;
    let requests = rig.requests().await;
    assert_eq!(requests.len(), 2);
    for body in &requests {
        assert!(!body.to_string().contains("defer_loading"), "{body}");
        assert!(!body.to_string().contains("tool_reference"), "{body}");
    }
    assert_eq!(rig.fallback_warnings(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_without_the_feature_keeps_the_client_side_route() {
    let model = "claude-opus-4-1";
    let rig = rig(None, "", model).await;
    rig.run(model).await;
    let requests = rig.requests().await;
    assert_eq!(requests.len(), 2);
    for body in &requests {
        assert!(!body.to_string().contains("defer_loading"), "{body}");
        assert!(!body.to_string().contains("tool_reference"), "{body}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_agent_decides_from_its_own_account_and_a_handle_without_a_client_is_never_native() {
    use fontaine_sdk::core::config::AccountKind;
    use fontaine_sdk::core::models::{ModelHandle, ModelRef};
    use fontaine_sdk::tools::mcp::Deferral;

    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "m1", "object": "model", "created": 0, "owned_by": "x"}],
        })))
        .mount(&other)
        .await;
    let world = World::new();
    world.user_file(&format!(
        "[accounts.main]\nkind = \"anthropic\"\nbase_url = \"http://127.0.0.1:1\"\nmodels = [\"{MODEL}\"]\n\n\
         [accounts.other]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"m1\"]\n\n\
         [model]\nmodel = \"main/{MODEL}\"\n",
        other.uri()
    ));
    credentials::store(&world.data, "main", "sk-main").unwrap();
    let config = world.load().unwrap();
    let env: fontaine_sdk::core::agent::EnvLookup = Arc::new(|_| None);
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &world.data,
        env: env.clone(),
        model_override: None,
        session_id: None,
    })
    .await
    .unwrap();
    let switcher = Switcher::new(config, world.data.clone(), env, None);
    assert!(resolved.main.native_deferral(), "the parent stays native");
    let (child, _) = switcher.main("other/m1").await.unwrap();
    assert!(!child.for_agent("c1").native_deferral());

    let deferral = Deferral::default();
    let bare = ModelHandle::new(
        ModelRef::parse(&format!("main/{MODEL}")).unwrap(),
        AccountKind::named("anthropic"),
        resolved.main.completion.clone(),
    );
    assert!(!bare.native_deferral());
    assert!(!bare.deferring(&deferral).native_deferral());
}
