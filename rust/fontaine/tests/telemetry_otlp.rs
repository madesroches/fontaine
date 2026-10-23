//! The OTLP/HTTP backend against a mock collector, the content canary, and the flush cap.

mod common;

use std::time::{Duration, Instant};

use common::{Endpoint, Route, Sandbox, Turn, Wire};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValue;
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use opentelemetry_proto::tonic::trace::v1::Span;
use prost::Message;
use serde_json::json;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn collector() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

/// Request bodies a collector received on `path`, in order.
async fn bodies(server: &MockServer, path: &str) -> Vec<Vec<u8>> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == path)
        .map(|r| r.body)
        .collect()
}

async fn all_bytes(server: &MockServer) -> Vec<u8> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .flat_map(|r| r.body)
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

async fn spans(server: &MockServer) -> Vec<Span> {
    bodies(server, "/v1/traces")
        .await
        .iter()
        .flat_map(|body| {
            ExportTraceServiceRequest::decode(&body[..])
                .unwrap()
                .resource_spans
        })
        .flat_map(|r| r.scope_spans)
        .flat_map(|s| s.spans)
        .collect()
}

fn config(model_url: &str, collector_url: &str, extra: &str) -> String {
    format!(
        "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{model_url}\"\napi_key_env = \"PAID_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n\n[telemetry]\nenabled = true\nbackend = \"otlp\"\nendpoint = \"{collector_url}\"\napi_key_env = \"INGEST_KEY\"\n{extra}"
    )
}

fn sandbox(model_url: &str, collector_url: &str, extra: &str) -> Sandbox {
    let mut s = Sandbox::new();
    s.user_config(&config(model_url, collector_url, extra));
    s.set_env("PAID_KEY", "sk-test-not-a-real-key");
    s.set_env("INGEST_KEY", "ingest-secret");
    s
}

fn attr<'a>(
    attributes: &'a [opentelemetry_proto::tonic::common::v1::KeyValue],
    key: &str,
) -> Option<&'a AnyValue> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.value.as_ref())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_exports_spans_metrics_and_the_api_request_event() {
    let model = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::tool("call_1", "read", json!({"path": "note.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let collector = collector().await;
    let s = sandbox(&model.url(), &collector.uri(), "");
    s.add_file("note.txt", "x\n");
    let out = s.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "done\n");

    // Every export carries the ingestion key.
    for request in collector.received_requests().await.unwrap() {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .map(|v| v.to_str().unwrap()),
            Some("Bearer ingest-secret")
        );
    }

    // Spans: a tree with its parentage intact.
    let spans = spans(&collector).await;
    let find = |name: &str| spans.iter().find(|s| s.name == name);
    let session = find("fontaine.session").expect("session span");
    let interaction = find("fontaine.interaction").expect("interaction span");
    assert!(session.parent_span_id.is_empty(), "the session is a root");
    assert_eq!(interaction.parent_span_id, session.span_id);
    assert_eq!(interaction.trace_id, session.trace_id);
    let chats: Vec<&Span> = spans.iter().filter(|s| s.name == "chat").collect();
    assert_eq!(chats.len(), 2, "one chat span per model call");
    for chat in &chats {
        assert_eq!(chat.parent_span_id, interaction.span_id);
        assert_eq!(chat.trace_id, session.trace_id);
    }
    let tool = find("execute_tool").expect("tool span");
    assert_eq!(tool.parent_span_id, interaction.span_id);
    assert!(
        chats
            .iter()
            .all(|c| attr(&c.attributes, "gen_ai.request.model").is_some()),
        "model and token fields are exported"
    );
    // rig records its token counts as text.
    assert!(chats.iter().any(|c| matches!(
        attr(&c.attributes, "gen_ai.usage.input_tokens"),
        Some(AnyValue::StringValue(tokens)) if tokens == "1000"
    )));
    assert!(
        spans
            .iter()
            .all(|s| attr(&s.attributes, "code.file.path").is_none()
                && attr(&s.attributes, "thread.name").is_none()),
        "no source locations or thread names are exported"
    );

    // Metrics: tokens and cost.
    let all_metrics: Vec<_> = bodies(&collector, "/v1/metrics")
        .await
        .iter()
        .flat_map(|body| {
            ExportMetricsServiceRequest::decode(&body[..])
                .unwrap()
                .resource_metrics
        })
        .flat_map(|r| r.scope_metrics)
        .flat_map(|s| s.metrics)
        .collect();
    let metrics: Vec<String> = all_metrics.iter().map(|m| m.name.clone()).collect();
    for name in [
        "fontaine.token.usage",
        "fontaine.cost.usage",
        "fontaine.session.count",
        "fontaine.tool_decision.count",
    ] {
        assert!(metrics.iter().any(|m| m == name), "{name} in {metrics:?}");
    }
    let decision = all_metrics
        .iter()
        .find(|m| m.name == "fontaine.tool_decision.count")
        .unwrap();
    let Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(sum)) = &decision.data
    else {
        panic!("a counter: {decision:?}");
    };
    assert!(sum.data_points.iter().any(|p| matches!(
        (attr(&p.attributes, "tier"), attr(&p.attributes, "decision")),
        (Some(AnyValue::StringValue(t)), Some(AnyValue::StringValue(d)))
            if t == "read_only_fast_path" && d == "allow"
    )));

    // Events: api_request with tokens and cost.
    let records: Vec<_> = bodies(&collector, "/v1/logs")
        .await
        .iter()
        .flat_map(|body| {
            ExportLogsServiceRequest::decode(&body[..])
                .unwrap()
                .resource_logs
        })
        .flat_map(|r| r.scope_logs)
        .flat_map(|s| s.log_records)
        .collect();
    let requests: Vec<_> = records
        .iter()
        .filter(|r| {
            matches!(r.body.as_ref().and_then(|b| b.value.as_ref()),
                     Some(AnyValue::StringValue(text)) if text == "api_request")
        })
        .collect();
    assert_eq!(requests.len(), 2, "one api_request per model call");
    for record in requests {
        assert!(matches!(
            attr(&record.attributes, "input_tokens"),
            Some(AnyValue::IntValue(1000))
        ));
        assert!(matches!(
            attr(&record.attributes, "output_tokens"),
            Some(AnyValue::IntValue(500))
        ));
        let Some(AnyValue::DoubleValue(cost)) = attr(&record.attributes, "cost_usd") else {
            panic!("api_request carries a cost: {:?}", record.attributes);
        };
        assert!((cost - 0.00045).abs() < 1e-9, "{cost}");
        assert!(matches!(attr(&record.attributes, "model"),
                         Some(AnyValue::StringValue(m)) if m == "gpt-4o-mini"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn guardian_metrics_and_spans_reach_the_collector_attributed_to_the_guardian() {
    let judge = Endpoint::start(
        Wire::Chat,
        vec![Turn::tool(
            "g1",
            "submit",
            json!({"decision": "allow", "rule": "allow", "explanation": "fine", "confidence": 0.9}),
        )],
    )
    .await;
    let broken = Endpoint::start(Wire::Chat, vec![Turn::Fail(500)]).await;
    let collector = collector().await;
    for guardian in [&judge, &broken] {
        let agent = Endpoint::start(
            Wire::Chat,
            vec![
                Turn::tool("c1", "shell", json!({"command": "echo hi > out.txt"})),
                Turn::text("done"),
            ],
        )
        .await;
        let mut s = Sandbox::new();
        s.user_config(&format!(
            "[accounts.agent]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[accounts.judge]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"agent/scripted\"\n\n[guardian]\nmodel = \"judge/scripted\"\n\n[telemetry]\nenabled = true\nbackend = \"otlp\"\nendpoint = \"{}\"\napi_key_env = \"INGEST_KEY\"\n",
            agent.url(),
            guardian.url(),
            collector.uri()
        ));
        s.set_env("INGEST_KEY", "ingest-secret");
        let out = s.run(&["-p", "--permission-mode", "auto", "go"]).await;
        assert_eq!(out.code, Some(0), "{}", out.stderr);
    }

    let all_metrics: Vec<_> = bodies(&collector, "/v1/metrics")
        .await
        .iter()
        .flat_map(|body| {
            ExportMetricsServiceRequest::decode(&body[..])
                .unwrap()
                .resource_metrics
        })
        .flat_map(|r| r.scope_metrics)
        .flat_map(|s| s.metrics)
        .collect();
    let names: Vec<&str> = all_metrics.iter().map(|m| m.name.as_str()).collect();
    for name in [
        "fontaine.guardian.latency",
        "fontaine.guardian.fail_closed.count",
    ] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
    let failed = all_metrics
        .iter()
        .find(|m| m.name == "fontaine.guardian.fail_closed.count")
        .unwrap();
    let Some(Data::Sum(sum)) = &failed.data else {
        panic!("a counter: {failed:?}");
    };
    assert!(sum.data_points.iter().any(|p| matches!(
        attr(&p.attributes, "reason"),
        Some(AnyValue::StringValue(reason)) if reason == "error"
    )));
    // The guardian's own model calls are told apart from the agent's.
    let tokens = all_metrics
        .iter()
        .find(|m| m.name == "fontaine.token.usage")
        .unwrap();
    let Some(Data::Sum(sum)) = &tokens.data else {
        panic!("a counter: {tokens:?}");
    };
    let sources: Vec<String> = sum
        .data_points
        .iter()
        .filter_map(|p| match attr(&p.attributes, "query_source") {
            Some(AnyValue::StringValue(source)) => Some(source.clone()),
            _ => None,
        })
        .collect();
    assert!(sources.contains(&"guardian".to_string()), "{sources:?}");
    assert!(sources.contains(&"main".to_string()), "{sources:?}");

    // The guardian span sits under the permission span and carries no content.
    let spans = spans(&collector).await;
    let guardian = spans
        .iter()
        .find(|s| s.name == "fontaine.guardian")
        .expect("a guardian span");
    let permission = spans
        .iter()
        .find(|s| s.span_id == guardian.parent_span_id)
        .expect("its parent");
    assert_eq!(permission.name, "fontaine.permission");
}

#[tokio::test(flavor = "multi_thread")]
async fn traces_can_be_switched_off_while_metrics_and_events_stay() {
    let model = Endpoint::start(Wire::Responses, vec![Turn::text("done")]).await;
    let collector = collector().await;
    let s = sandbox(&model.url(), &collector.uri(), "traces = false\n");
    let out = s.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(bodies(&collector, "/v1/traces").await.is_empty());
    assert!(!bodies(&collector, "/v1/metrics").await.is_empty());
    assert!(!bodies(&collector, "/v1/logs").await.is_empty());
}

const PROMPT_CANARY: &str = "CANARY-PROMPT-7f3a9c";
const REPLY_CANARY: &str = "CANARY-REPLY-51d2e8";
const PROMPT_SECRET: &str = "sk-ant-api03-promptsecretabcdefghijklmnop";
const REPLY_SECRET: &str = "ghp_replysecretabcdefghijklmnopqrstuvwxyz1234";

async fn canary_run(extra: &str) -> Vec<u8> {
    let model = Endpoint::start(
        Wire::Responses,
        vec![Turn::text(&format!(
            "reply {REPLY_CANARY} with token {REPLY_SECRET} inside"
        ))],
    )
    .await;
    let collector = collector().await;
    let s = sandbox(&model.url(), &collector.uri(), extra);
    let prompt = format!("question {PROMPT_CANARY} using key {PROMPT_SECRET} please");
    let out = s.headless(&[&prompt]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    all_bytes(&collector).await
}

#[tokio::test(flavor = "multi_thread")]
async fn no_content_leaves_the_process_without_the_opt_in_keys() {
    let exported = canary_run("").await;
    assert!(!exported.is_empty());
    for needle in [
        PROMPT_CANARY,
        REPLY_CANARY,
        PROMPT_SECRET,
        REPLY_SECRET,
        "question",
        "reply ",
    ] {
        assert!(!contains(&exported, needle), "`{needle}` was exported");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn opted_in_content_is_exported_with_credential_shaped_strings_redacted() {
    let exported = canary_run("log_user_prompts = true\nlog_assistant_responses = true\n").await;
    // The prompt leg and the response leg both arrive...
    assert!(contains(&exported, PROMPT_CANARY), "prompt leg");
    assert!(contains(&exported, REPLY_CANARY), "response leg");
    // ...with anything shaped like a credential replaced.
    assert!(!contains(&exported, PROMPT_SECRET), "prompt secret leaked");
    assert!(!contains(&exported, REPLY_SECRET), "reply secret leaked");
    assert!(
        !contains(&exported, "sk-ant-api03"),
        "prompt secret prefix leaked"
    );
    assert!(contains(&exported, "[REDACTED]"));
}

#[tokio::test(flavor = "multi_thread")]
async fn one_opt_in_key_exports_only_its_own_leg() {
    let exported = canary_run("log_user_prompts = true\n").await;
    assert!(contains(&exported, PROMPT_CANARY));
    assert!(!contains(&exported, REPLY_CANARY));
}

#[tokio::test(flavor = "multi_thread")]
async fn content_is_truncated_at_content_max_bytes() {
    let exported = canary_run("log_user_prompts = true\ncontent_max_bytes = 8\n").await;
    assert!(
        !contains(&exported, PROMPT_CANARY),
        "the canary lies past the cap"
    );
}

const TOOL_ARG_CANARY: &str = "CANARY-TOOLARG-2b9e41";
const TOOL_OUT_CANARY: &str = "CANARY-TOOLOUT-8c07d3";

async fn tool_canary_run(extra: &str) -> Vec<u8> {
    let file = format!("{TOOL_ARG_CANARY}.txt");
    let model = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::tool("call_1", "read", json!({ "path": file })),
            Turn::text("done"),
        ],
    )
    .await;
    let collector = collector().await;
    let s = sandbox(&model.url(), &collector.uri(), extra);
    s.add_file(&file, &format!("contents {TOOL_OUT_CANARY}\n"));
    let out = s.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout == "done\n", "{}", out.stdout);
    all_bytes(&collector).await
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_arguments_and_output_stay_out_of_the_export_without_their_opt_in_keys() {
    let exported = tool_canary_run("").await;
    assert!(!contains(&exported, TOOL_ARG_CANARY), "arguments leaked");
    assert!(!contains(&exported, TOOL_OUT_CANARY), "output leaked");
    // The content-free record of the call itself is exported.
    assert!(contains(&exported, "fontaine.tool"));
    assert!(contains(&exported, "tool_result"));
    assert!(contains(&exported, "read_only"));
}

#[tokio::test(flavor = "multi_thread")]
async fn log_tool_details_exports_the_arguments_and_not_the_output() {
    let exported = tool_canary_run("log_tool_details = true\n").await;
    assert!(contains(&exported, TOOL_ARG_CANARY));
    assert!(!contains(&exported, TOOL_OUT_CANARY));
}

#[tokio::test(flavor = "multi_thread")]
async fn log_tool_content_exports_the_output_and_not_the_arguments() {
    let exported = tool_canary_run("log_tool_content = true\n").await;
    assert!(contains(&exported, TOOL_OUT_CANARY));
    assert!(!contains(&exported, TOOL_ARG_CANARY));
}

/// A listener that accepts connections and never answers.
fn black_hole() -> (
    std::net::SocketAddr,
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Sender<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let mut held = Vec::new();
        while stopped.try_recv().is_err() {
            if let Ok((stream, _)) = listener.accept() {
                held.push(stream);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    (addr, handle, stop)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exporter_that_never_answers_cannot_hold_the_exit_past_the_flush_cap() {
    let model = Endpoint::start(Wire::Responses, vec![Turn::text("steady answer")]).await;
    let plain = {
        let mut s = Sandbox::priced(&model.url());
        s.set_env("PAID_KEY", "sk-test-not-a-real-key");
        s.headless(&["go"]).await
    };

    let (addr, handle, stop) = black_hole();
    let s = sandbox(&model.url(), &format!("http://{addr}"), "");
    let started = Instant::now();
    let out = s.headless(&["go"]).await;
    let elapsed = started.elapsed();
    stop.send(()).unwrap();
    handle.join().unwrap();

    assert_eq!(out.code, plain.code, "{}", out.stderr);
    assert_eq!(out.stdout, plain.stdout);
    assert!(
        elapsed < Duration::from_secs(8),
        "the run took {elapsed:?} against an exporter that never answers"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_micromegas_recipe_over_otlp_appends_signal_paths_to_the_endpoint() {
    let model = Endpoint::start(Wire::Responses, vec![Turn::text("done")]).await;
    let collector = collector().await;
    let s = sandbox(
        &model.url(),
        &format!("{}/ingestion/otlp/", collector.uri()),
        "",
    );
    let out = s.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let paths: Vec<String> = collector
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    for path in [
        "/ingestion/otlp/v1/traces",
        "/ingestion/otlp/v1/metrics",
        "/ingestion/otlp/v1/logs",
    ] {
        assert!(paths.iter().any(|p| p == path), "{path} in {paths:?}");
    }
}

async fn metric_points(
    server: &MockServer,
    name: &str,
) -> Vec<Vec<opentelemetry_proto::tonic::common::v1::KeyValue>> {
    bodies(server, "/v1/metrics")
        .await
        .iter()
        .flat_map(|body| {
            ExportMetricsServiceRequest::decode(&body[..])
                .unwrap()
                .resource_metrics
        })
        .flat_map(|r| r.scope_metrics)
        .flat_map(|s| s.metrics)
        .filter(|m| m.name == name)
        .flat_map(|m| match m.data {
            Some(Data::Sum(sum)) => sum.data_points,
            _ => Vec::new(),
        })
        .map(|p| p.attributes)
        .collect()
}

async fn log_records(
    server: &MockServer,
    event: &str,
) -> Vec<opentelemetry_proto::tonic::logs::v1::LogRecord> {
    bodies(server, "/v1/logs")
        .await
        .iter()
        .flat_map(|body| {
            ExportLogsServiceRequest::decode(&body[..])
                .unwrap()
                .resource_logs
        })
        .flat_map(|r| r.scope_logs)
        .flat_map(|s| s.log_records)
        .filter(|r| {
            matches!(r.body.as_ref().and_then(|b| b.value.as_ref()),
                     Some(AnyValue::StringValue(text)) if text == event)
        })
        .collect()
}

fn text(value: Option<&AnyValue>) -> Option<&str> {
    match value {
        Some(AnyValue::StringValue(text)) => Some(text.as_str()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_count_carries_the_start_type_and_whether_the_run_was_headless() {
    let model = Endpoint::start(Wire::Responses, vec![Turn::text("done")]).await;
    let collector = collector().await;
    let s = sandbox(&model.url(), &collector.uri(), "");
    let first = s.headless(&["--output-format", "stream-json", "one"]).await;
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let id = first.lines()[0]["session_id"].as_str().unwrap().to_string();
    let second = s.headless(&["--continue", "two"]).await;
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    let third = s
        .headless(&["--resume", &id, "--fork-session", "three"])
        .await;
    assert_eq!(third.code, Some(0), "{}", third.stderr);

    let points = metric_points(&collector, "fontaine.session.count").await;
    let mut seen: Vec<(String, String)> = points
        .iter()
        .map(|attrs| {
            (
                text(attr(attrs, "start_type")).unwrap_or("").to_string(),
                text(attr(attrs, "headless")).unwrap_or("").to_string(),
            )
        })
        .collect();
    seen.sort();
    // A fork takes the value of the flag it forks from.
    assert_eq!(
        seen,
        [
            ("continue".to_string(), "true".to_string()),
            ("fresh".to_string(), "true".to_string()),
            ("resume".to_string(), "true".to_string()),
        ]
    );
}

fn agent_routes() -> Vec<Route> {
    vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "explore", "prompt": "child-T: look", "background": false}),
                ),
                Turn::text("done"),
            ],
        ),
        Route::new(
            "child-T",
            vec![
                Turn::tool("k1", "read", json!({"path": "note.txt"})),
                Turn::text("saw it"),
            ],
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn subagents_have_spans_under_their_spawning_call_and_decisions_keyed_by_agent() {
    let model = Endpoint::start_routed(Wire::Responses, agent_routes()).await;
    let collector = collector().await;
    let s = sandbox(&model.url(), &collector.uri(), "");
    s.add_file("note.txt", "x\n");
    let out = s.headless(&["--allowed-tools", "agent", "root task"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);

    let spans = spans(&collector).await;
    let subagent = spans
        .iter()
        .find(|s| s.name == "fontaine.subagent")
        .expect("a subagent span");
    assert_eq!(
        text(attr(&subagent.attributes, "agent.id")),
        Some("agent-1")
    );
    assert_eq!(
        text(attr(&subagent.attributes, "agent.type")),
        Some("explore")
    );
    let parent = spans
        .iter()
        .find(|s| s.span_id == subagent.parent_span_id)
        .expect("a parent");
    assert_eq!(parent.name, "fontaine.tool");
    assert_eq!(text(attr(&parent.attributes, "tool_name")), Some("agent"));
    assert_eq!(parent.trace_id, subagent.trace_id);

    // The child's model calls are the subagent's, and its decisions carry its id like the audit
    // log's records do.
    let tokens = metric_points(&collector, "fontaine.token.usage").await;
    let sources: Vec<&str> = tokens
        .iter()
        .filter_map(|a| text(attr(a, "query_source")))
        .collect();
    assert!(
        sources.contains(&"subagent") && sources.contains(&"main"),
        "{sources:?}"
    );
    let decisions = log_records(&collector, "tool_decision").await;
    let by_agent = |agent: &str| {
        decisions
            .iter()
            .filter(|r| text(attr(&r.attributes, "agent.id")) == Some(agent))
            .count()
    };
    assert_eq!(by_agent("root"), 1);
    assert_eq!(by_agent("agent-1"), 1);
    let events = log_records(&collector, "subagent").await;
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|r| text(attr(&r.attributes, "event")))
        .collect();
    assert_eq!(kinds, ["spawn", "complete"]);
}

fn example(name: &str) -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = dir.join(file);
    assert!(path.exists(), "{} is not built", path.display());
    path.to_string_lossy().into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_names_are_exported_only_when_tool_details_are_on() {
    for details in [false, true] {
        let model = Endpoint::start(
            Wire::Responses,
            vec![
                Turn::tool("c1", "mcp__repo__echo", json!({})),
                Turn::text("done"),
            ],
        )
        .await;
        let collector = collector().await;
        let s = sandbox(
            &model.url(),
            &collector.uri(),
            &format!("log_tool_details = {details}\n"),
        );
        s.add_file(
            ".fontaine/settings.toml",
            &format!(
                "[mcp.servers.repo]\ncommand = '{}'\n",
                example("repo_mcp_server")
            ),
        );
        let out = s
            .headless(&["--allowed-tools", "mcp__repo__echo", "go"])
            .await;
        assert_eq!(out.code, Some(0), "{}", out.stderr);

        let everything = all_bytes(&collector).await;
        let shown = |needle: &str| contains(&everything, needle);
        assert_eq!(
            shown("mcp__repo__echo"),
            details,
            "tool name, details = {details}"
        );
        let connection = log_records(&collector, "mcp_server_connection").await;
        assert_eq!(connection.len(), 1);
        let server = text(attr(&connection[0].attributes, "server")).unwrap();
        assert_eq!(server, if details { "repo" } else { "third-party" });
        let decision = &log_records(&collector, "tool_decision").await[0];
        let tool = text(attr(&decision.attributes, "tool_name")).unwrap();
        assert_eq!(
            tool,
            if details {
                "mcp__repo__echo"
            } else {
                "third-party"
            }
        );
        // The call span exists either way.
        assert!(
            spans(&collector)
                .await
                .iter()
                .any(|s| s.name == "fontaine.mcp.call")
        );
    }
}
