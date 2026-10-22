//! The OTLP/HTTP backend against a mock collector, the content canary, and the flush cap.

mod common;

use std::time::{Duration, Instant};

use common::{Endpoint, Sandbox, Turn, Wire};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValue;
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
