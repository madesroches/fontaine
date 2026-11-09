//! MCP servers register wrapped and `Arbitrary`, whatever they declare, and keep doing so when
//! their tool list changes.

mod mcp_common;

use std::time::Duration;

use fontaine_tools::mcp::{ServerSpec, Transport, connect, connect_transport};
use fontaine_tools::risk::RiskClass;
use mcp_common::server::{
    Double, echo_tool, grant_access_tool, lookup_tool, marked, unmarked_grant_access_tool,
};
use mcp_common::{Gate, call, names, options, tool_server};
use rmcp::ServiceExt;
use serde_json::json;

/// Connects `double` over an in-process pipe as the server `name`.
async fn serve(
    name: &str,
    double: Double,
    gate: std::sync::Arc<Gate>,
    handle: rig_agent::tool::server::ToolServerHandle,
) -> (
    fontaine_tools::mcp::McpConnectionHandle,
    tokio::task::JoinHandle<()>,
) {
    serve_with(name, double, handle, &options(gate)).await
}

async fn serve_with(
    name: &str,
    double: Double,
    handle: rig_agent::tool::server::ToolServerHandle,
    options: &fontaine_tools::mcp::McpOptions,
) -> (
    fontaine_tools::mcp::McpConnectionHandle,
    tokio::task::JoinHandle<()>,
) {
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        let running = double.serve((sfc, s2c)).await.expect("server start");
        let _ = running.waiting().await;
    });
    let connected = connect_transport(name, (cfs, c2s), handle, options)
        .await
        .expect("connect");
    (connected.connection, task)
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
async fn any_tools_list_order_registers_the_same_definition_sequence() {
    let orders = [
        vec![echo_tool(), lookup_tool(), grant_access_tool()],
        vec![grant_access_tool(), lookup_tool(), echo_tool()],
        vec![lookup_tool(), grant_access_tool(), echo_tool()],
    ];
    let mut sequences = Vec::new();
    for tools in orders {
        let handle = tool_server();
        let (connection, _task) = serve(
            "double",
            Double::with_tools(tools),
            Gate::new(true),
            handle.clone(),
        )
        .await;
        let sequence: Vec<String> = handle
            .snapshot()
            .definitions()
            .iter()
            .map(|d| d.name.to_string())
            .collect();
        sequences.push(sequence);
        connection.close().await;
    }
    assert_eq!(
        sequences[0],
        [
            "mcp__double__echo",
            "mcp__double__grant_access",
            "mcp__double__lookup"
        ]
    );
    assert!(
        sequences.iter().all(|s| s == &sequences[0]),
        "{sequences:?}"
    );
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
async fn a_marked_tool_reaches_the_gate_marked_and_an_unmarked_one_does_not() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let double = Double::with_tools(vec![grant_access_tool(), echo_tool()]);
    let (connection, _task) = serve("double", double, gate.clone(), handle.clone()).await;

    call(&handle, "mcp__double__grant_access", "{}")
        .await
        .unwrap();
    call(&handle, "mcp__double__echo", "{}").await.unwrap();
    let seen = gate.calls();
    assert!(seen[0].requires_user_interaction);
    assert!(!seen[1].requires_user_interaction);
    connection.close().await;
}

#[tokio::test]
async fn a_marker_added_by_a_refresh_takes_effect_and_a_removed_one_stays() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let double = Double::with_tools(vec![grant_access_tool(), lookup_tool()]);
    let (connection, _task) = serve("double", double.clone(), gate.clone(), handle.clone()).await;

    double
        .replace_tools(vec![unmarked_grant_access_tool(), marked(echo_tool())])
        .await;
    for _ in 0..200 {
        if names(&handle).contains(&"mcp__double__echo".to_string()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for tool in ["mcp__double__grant_access", "mcp__double__echo"] {
        call(&handle, tool, "{}").await.unwrap();
    }
    let seen = gate.calls();
    assert!(
        seen[0].requires_user_interaction,
        "removal waits for a new connection"
    );
    assert!(seen[1].requires_user_interaction, "an added marker applies");
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
            env: Vec::new(),
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
    let connected = connect(
        &spec,
        handle.clone(),
        &options(gate.clone()),
        &Default::default(),
        &env,
    )
    .await
    .expect("the stdio server starts");
    assert_eq!(
        names(&handle),
        [
            "mcp__stdio__echo",
            "mcp__stdio__elicit",
            "mcp__stdio__env",
            "mcp__stdio__lookup"
        ]
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
async fn configured_env_reaches_a_stdio_server_and_overrides_a_copied_value() {
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
            env: vec![
                ("CONFIGURED_VAR".into(), "from-env-table".into()),
                ("GRANTED_VAR".into(), "overridden".into()),
            ],
            cwd: std::env::temp_dir(),
        },
    };
    let env = |name: &str| match name {
        "GRANTED_VAR" => Some("granted".into()),
        "PATH" => std::env::var_os("PATH"),
        _ => None,
    };
    let connected = connect(
        &spec,
        handle.clone(),
        &options(gate.clone()),
        &fontaine_tools::tls::LoadedNetwork::default(),
        &env,
    )
    .await
    .expect("the stdio server starts");
    for (name, expected) in [
        ("CONFIGURED_VAR", "from-env-table"),
        ("GRANTED_VAR", "overridden"),
    ] {
        let seen = call(
            &handle,
            "mcp__stdio__env",
            &json!({"name": name}).to_string(),
        )
        .await
        .unwrap();
        assert!(seen.contains(expected), "{name}: {seen}");
    }
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
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        },
    };
    let error = connect(
        &spec,
        tool_server(),
        &options(gate),
        &Default::default(),
        &|_| None,
    )
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
                "_meta": {"anthropic/requiresUserInteraction": true},
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
    let connected = connect(
        &spec,
        handle.clone(),
        &options(gate.clone()),
        &Default::default(),
        &|_| None,
    )
    .await
    .expect("the endpoint answers");
    assert_eq!(names(&handle), ["mcp__web__ping"]);
    assert_eq!(call(&handle, "mcp__web__ping", "{}").await.unwrap(), "pong");
    assert_eq!(gate.calls()[0].risk, RiskClass::Arbitrary);
    assert!(gate.calls()[0].requires_user_interaction);
    connected.connection.close().await;
}

#[tokio::test]
async fn an_http_server_is_reached_through_the_configured_proxy() {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let proxy = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .mount(&proxy)
        .await;
    let network = fontaine_tools::tls::NetworkSettings {
        http_proxy: Some(proxy.uri()),
        ..Default::default()
    }
    .load()
    .unwrap();
    let spec = ServerSpec {
        name: "remote".to_string(),
        transport: Transport::Http {
            url: "http://mcp.test/mcp".to_string(),
        },
    };
    let gate = Gate::new(true);
    let result = connect(&spec, tool_server(), &options(gate), &network, &|_| None).await;
    assert!(result.is_err());
    let seen = proxy.received_requests().await.unwrap();
    assert!(!seen.is_empty());
    assert_eq!(seen[0].url.host_str(), Some("mcp.test"));
}

/// Answers every elicitation with `reply` and keeps the forms it was shown.
struct Answers {
    reply: fontaine_tools::mcp::ElicitReply,
    forms: std::sync::Mutex<Vec<fontaine_tools::mcp::ElicitForm>>,
}

impl fontaine_tools::mcp::Elicitor for Answers {
    fn elicit<'a>(
        &'a self,
        form: fontaine_tools::mcp::ElicitForm,
    ) -> futures::future::BoxFuture<'a, fontaine_tools::mcp::ElicitReply> {
        self.forms.lock().unwrap().push(form);
        let reply = self.reply.clone();
        Box::pin(async move { reply })
    }
}

#[tokio::test]
async fn a_tool_that_asks_for_input_gets_cancel_when_nobody_can_answer() {
    let gate = Gate::new(true);
    let handle = tool_server();
    let (connection, _task) = serve(
        "double",
        Double::with_tools(vec![mcp_common::server::elicit_tool()]),
        gate,
        handle.clone(),
    )
    .await;
    let out = call(&handle, "mcp__double__elicit", "{}").await.unwrap();
    assert!(out.contains(r#""action":"cancel""#), "{out}");
    connection.close().await;
}

#[tokio::test]
async fn a_tool_that_asks_for_input_gets_the_form_answer() {
    use fontaine_tools::mcp::ElicitReply;
    let answers = std::sync::Arc::new(Answers {
        reply: ElicitReply::Accept(serde_json::Map::from_iter([
            ("name".to_string(), json!("Ada")),
            ("age".to_string(), json!(36)),
        ])),
        forms: Default::default(),
    });
    let mut options = options(Gate::new(true));
    options.elicitor = answers.clone();
    let handle = tool_server();
    let (connection, _task) = serve_with(
        "double",
        Double::with_tools(vec![mcp_common::server::elicit_tool()]),
        handle.clone(),
        &options,
    )
    .await;
    let out = call(&handle, "mcp__double__elicit", "{}").await.unwrap();
    assert!(out.contains(r#""action":"accept""#), "{out}");
    assert!(
        out.contains(r#""name":"Ada""#) && out.contains(r#""age":36"#),
        "{out}"
    );
    {
        let forms = answers.forms.lock().unwrap();
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].server, "double");
        assert_eq!(forms[0].message, "Who are you?");
        let keys: Vec<_> = forms[0]
            .fields()
            .into_iter()
            .map(|f| (f.key, f.required))
            .collect();
        assert_eq!(
            keys,
            [("age".to_string(), false), ("name".to_string(), true)]
        );
    }
    connection.close().await;
}

fn spilling(dir: &std::path::Path, gate: std::sync::Arc<Gate>) -> fontaine_tools::mcp::McpOptions {
    let mut options = options(gate);
    options.limits = std::sync::Arc::new(fontaine_tools::mcp::OutputLimits::new(
        25_000,
        Some(rig_tools::Spill(dir.join("mcp-results"))),
    ));
    options
}

#[tokio::test]
async fn a_result_over_the_limit_is_saved_to_a_file_and_the_model_gets_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let handle = tool_server();
    let (connection, _task) = serve_with(
        "double",
        Double::with_tools(vec![mcp_common::server::big_tool()]),
        handle.clone(),
        &spilling(dir.path(), Gate::new(true)),
    )
    .await;
    let out = call(&handle, "mcp__double__big", r#"{"chars":60000}"#)
        .await
        .unwrap();
    assert!(
        out.starts_with("Output too large (60000 chars, ~15000 tokens), saved to "),
        "{out}"
    );
    let saved: Vec<_> = std::fs::read_dir(dir.path().join("mcp-results"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(saved.len(), 1);
    assert!(out.contains(&saved[0].display().to_string()), "{out}");
    assert_eq!(
        std::fs::read_to_string(&saved[0]).unwrap(),
        "x".repeat(60_000)
    );

    let small = call(&handle, "mcp__double__big", r#"{"chars":100}"#)
        .await
        .unwrap();
    assert_eq!(small, "x".repeat(100));
    connection.close().await;
}

#[tokio::test]
async fn a_declared_max_result_size_raises_the_limit_for_that_tool() {
    let dir = tempfile::tempdir().unwrap();
    let mut big = mcp_common::server::big_tool();
    let mut meta = rmcp::model::MetaObject::new();
    meta.insert("anthropic/maxResultSizeChars".to_string(), json!(100_000));
    big.meta = Some(meta);
    let handle = tool_server();
    let (connection, _task) = serve_with(
        "double",
        Double::with_tools(vec![big]),
        handle.clone(),
        &spilling(dir.path(), Gate::new(true)),
    )
    .await;
    let out = call(&handle, "mcp__double__big", r#"{"chars":60000}"#)
        .await
        .unwrap();
    assert_eq!(out.len(), 60_000);
    assert!(!dir.path().join("mcp-results").exists());
    connection.close().await;
}

#[tokio::test]
async fn a_long_error_result_keeps_its_head_and_tail() {
    let dir = tempfile::tempdir().unwrap();
    let handle = tool_server();
    let (connection, _task) = serve_with(
        "double",
        Double::with_tools(vec![mcp_common::server::failing_tool()]),
        handle.clone(),
        &spilling(dir.path(), Gate::new(true)),
    )
    .await;
    let out = call(&handle, "mcp__double__fail", r#"{"chars":20000}"#)
        .await
        .unwrap_err();
    assert_eq!(
        out.len(),
        5_000 + 5_000 + "\n[… 10000 chars cut …]\n".len(),
        "{}",
        out.len()
    );
    connection.close().await;
}
