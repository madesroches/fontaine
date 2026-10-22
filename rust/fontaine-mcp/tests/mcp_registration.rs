//! MCP servers register wrapped and `Arbitrary`, whatever they declare, and keep doing so when
//! their tool list changes.

mod common;

use std::time::Duration;

use common::server::{Double, echo_tool, lookup_tool};
use common::{Gate, call, guard, names, tool_server};
use fontaine_mcp::{ServerSpec, Transport, connect, connect_transport};
use fontaine_tools::risk::RiskClass;
use rmcp::ServiceExt;
use serde_json::json;

/// Connects `double` over an in-process pipe as the server `name`.
async fn serve(
    name: &str,
    double: Double,
    gate: std::sync::Arc<Gate>,
    handle: rig_agent::tool::server::ToolServerHandle,
) -> (
    fontaine_mcp::McpConnectionHandle,
    tokio::task::JoinHandle<()>,
) {
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        let running = double.serve((sfc, s2c)).await.expect("server start");
        let _ = running.waiting().await;
    });
    let connection = connect_transport(name, (cfs, c2s), handle, guard(gate), true)
        .await
        .expect("connect");
    (connection, task)
}

#[tokio::test]
async fn tools_register_under_the_claude_code_names_and_run_only_after_the_gate() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let double = Double::with_tools(vec![echo_tool(), lookup_tool()]);
    let calls = double.calls.clone();
    let (connection, _task) = serve("double", double, gate.clone(), handle.clone()).await;

    assert_eq!(names(&handle), ["mcp__double__echo", "mcp__double__lookup"]);
    let out = call(&handle, "mcp__double__echo", r#"{"a":1}"#)
        .await
        .unwrap();
    assert!(out.contains("echo"), "{out}");
    assert_eq!(gate.calls().len(), 1);
    assert_eq!(gate.calls()[0].tool, "mcp__double__echo");
    assert_eq!(calls.lock().unwrap().len(), 1);
    connection.close().await;
}

#[tokio::test]
async fn a_denied_call_never_reaches_the_server() {
    let gate = Gate::new(false);
    let handle = tool_server();
    let double = Double::with_tools(vec![echo_tool()]);
    let calls = double.calls.clone();
    let (connection, _task) = serve("double", double, gate.clone(), handle.clone()).await;

    let denied = call(&handle, "mcp__double__echo", "{}").await.unwrap_err();
    assert!(denied.contains("denied by the test gate"), "{denied}");
    assert!(calls.lock().unwrap().is_empty());
    connection.close().await;
}

#[tokio::test]
async fn a_declared_read_only_hint_is_text_for_the_guardian_and_never_a_risk_class() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let double = Double::with_tools(vec![lookup_tool()]);
    let (connection, _task) = serve("double", double, gate.clone(), handle.clone()).await;

    call(&handle, "mcp__double__lookup", "{}").await.unwrap();
    let seen = gate.calls();
    assert_eq!(seen[0].risk, RiskClass::Arbitrary);
    let declared = seen[0].annotations.as_deref().unwrap();
    assert!(declared.contains("readOnlyHint"), "{declared}");
    connection.close().await;
}

#[tokio::test]
async fn a_list_changed_refresh_registers_the_new_tools_wrapped_and_drops_the_old() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let double = Double::with_tools(vec![echo_tool()]);
    let (connection, _task) = serve("double", double.clone(), gate.clone(), handle.clone()).await;
    assert_eq!(names(&handle), ["mcp__double__echo"]);

    double.replace_tools(vec![lookup_tool()]).await;
    for _ in 0..200 {
        if names(&handle) == ["mcp__double__lookup"] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(names(&handle), ["mcp__double__lookup"]);

    call(&handle, "mcp__double__lookup", "{}").await.unwrap();
    let seen = gate.calls();
    assert_eq!(seen.last().unwrap().tool, "mcp__double__lookup");
    assert_eq!(seen.last().unwrap().risk, RiskClass::Arbitrary);
    connection.close().await;
}

#[tokio::test]
async fn two_servers_keep_their_tools_apart() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let (a, _ta) = serve(
        "one",
        Double::with_tools(vec![echo_tool()]),
        gate.clone(),
        handle.clone(),
    )
    .await;
    let (b, _tb) = serve(
        "two",
        Double::with_tools(vec![echo_tool()]),
        gate.clone(),
        handle.clone(),
    )
    .await;
    assert_eq!(names(&handle), ["mcp__one__echo", "mcp__two__echo"]);
    a.close().await;
    b.close().await;
}

/// The example binary Cargo builds beside the tests.
fn double_binary() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "mcp_double.exe"
    } else {
        "mcp_double"
    };
    dir.join(name)
}

#[tokio::test]
async fn a_stdio_server_gets_only_the_variables_it_is_given_by_name() {
    let binary = double_binary();
    assert!(binary.exists(), "{} is not built", binary.display());
    let gate = Gate::new(true);
    let handle = tool_server();
    let spec = ServerSpec {
        name: "stdio".to_string(),
        transport: Transport::Stdio {
            command: binary.to_string_lossy().into_owned(),
            args: Vec::new(),
            env_vars: vec!["GRANTED_VAR".to_string()],
            cwd: std::env::temp_dir(),
        },
    };
    // The parent has two secrets; the config names only one of them.
    let env = |name: &str| match name {
        "GRANTED_VAR" => Some("granted".into()),
        "WITHHELD_KEY" => Some("withheld".into()),
        "PATH" => std::env::var_os("PATH"),
        _ => None,
    };
    let connected = connect(&spec, handle.clone(), guard(gate.clone()), true, &env)
        .await
        .expect("the stdio server starts");
    assert_eq!(
        names(&handle),
        ["mcp__stdio__echo", "mcp__stdio__env", "mcp__stdio__lookup"]
    );
    let granted = call(
        &handle,
        "mcp__stdio__env",
        &json!({"name": "GRANTED_VAR"}).to_string(),
    )
    .await
    .unwrap();
    assert!(granted.contains("granted"), "{granted}");
    let withheld = call(
        &handle,
        "mcp__stdio__env",
        &json!({"name": "WITHHELD_KEY"}).to_string(),
    )
    .await
    .unwrap();
    assert!(withheld.contains("<unset>"), "{withheld}");
    connected.connection.close().await;
}

#[tokio::test]
async fn a_server_that_cannot_start_is_an_error_naming_it() {
    let gate = Gate::new(true);
    let spec = ServerSpec {
        name: "ghost".to_string(),
        transport: Transport::Stdio {
            command: "fontaine-no-such-program".to_string(),
            args: Vec::new(),
            env_vars: Vec::new(),
            cwd: std::env::temp_dir(),
        },
    };
    let error = connect(&spec, tool_server(), guard(gate), true, &|_| None)
        .await
        .err()
        .expect("it cannot start");
    assert!(error.to_string().contains("ghost"), "{error}");
}

/// A streamable-HTTP MCP endpoint that answers each POST with one JSON reply.
struct JsonRpc;

impl wiremock::Respond for JsonRpc {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        use wiremock::ResponseTemplate;
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
                "annotations": {"readOnlyHint": true},
            }]})),
            Some("tools/call") => reply(json!({"content": [{"type": "text", "text": "pong"}]})),
            // Notifications get an acknowledgement and nothing else.
            _ => ResponseTemplate::new(202),
        }
    }
}

#[tokio::test]
async fn a_streamable_http_server_registers_and_runs_behind_the_gate() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(JsonRpc)
        .mount(&server)
        .await;
    let gate = Gate::new(true);
    let handle = tool_server();
    let spec = ServerSpec {
        name: "web".to_string(),
        transport: Transport::Http {
            url: format!("{}/mcp", server.uri()),
        },
    };
    let connected = connect(&spec, handle.clone(), guard(gate.clone()), true, &|_| None)
        .await
        .expect("the endpoint answers");
    assert_eq!(names(&handle), ["mcp__web__ping"]);
    assert_eq!(call(&handle, "mcp__web__ping", "{}").await.unwrap(), "pong");
    assert_eq!(gate.calls()[0].risk, RiskClass::Arbitrary);
    connected.connection.close().await;
}
