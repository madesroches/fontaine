//! The micromegas backend: the span bridge's async span events in an in-memory sink, and the
//! shipped binary against a mock ingestion endpoint. The tests share process-global micromegas
//! state, so they run one at a time.

mod common;

use fontaine_tools::lock::lock_recovering;
use std::sync::{Arc, Mutex};

use common::{Endpoint, Sandbox, Turn, Wire};
use fontaine::telemetry::content::ContentPolicy;
use fontaine::telemetry::micromegas::MicromegasExport;
use fontaine::telemetry::span_bridge::{SpanBridge, is_exported_span};
use micromegas_tracing::dispatch::flush_thread_buffer;
use micromegas_tracing::runtime::TracingRuntimeExt;
use micromegas_tracing::spans::{ThreadBlock, ThreadEventQueueAny};
use micromegas_tracing::test_utils::init_in_memory_tracing;
use micromegas_transit::HeterogeneousQueue;
use tracing::{Dispatch, info_span};
use tracing_subscriber::layer::SubscriberExt;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    // The mutex only serialises tests; one failing test must not fail the rest.
    lock_recovering(&SERIAL)
}

const TARGET: &str = "fontaine::telemetry";

#[derive(Debug)]
struct Seen {
    begin: Option<(u64, u32)>,
    end: Option<(u64, u32)>,
    name: String,
    properties: Vec<(String, String)>,
}

/// Every async span event of every thread block, paired by span id.
fn spans(blocks: &[Arc<ThreadBlock>]) -> std::collections::BTreeMap<u64, Seen> {
    let mut out = std::collections::BTreeMap::<u64, Seen>::new();
    for block in blocks {
        for event in block.events.iter() {
            match event {
                ThreadEventQueueAny::BeginAsyncDynNamedSpanEvent(e) => {
                    let seen = out.entry(e.span_id).or_insert_with(|| Seen {
                        begin: None,
                        end: None,
                        name: block.dyn_deps.str(e.name.id).to_string(),
                        properties: Vec::new(),
                    });
                    seen.begin = Some((e.parent_span_id, e.depth));
                }
                ThreadEventQueueAny::EndAsyncDynNamedSpanEvent(e) => {
                    let seen = out.entry(e.span_id).or_insert_with(|| Seen {
                        begin: None,
                        end: None,
                        name: block.dyn_deps.str(e.name.id).to_string(),
                        properties: Vec::new(),
                    });
                    seen.end = Some((e.parent_span_id, e.depth));
                }
                ThreadEventQueueAny::TaggedEndAsyncNamedSpanEvent(e) => {
                    let seen = out.entry(e.span_id).or_insert_with(|| Seen {
                        begin: None,
                        end: None,
                        name: block.dyn_deps.str(e.name.id).to_string(),
                        properties: Vec::new(),
                    });
                    seen.end = Some((e.parent_span_id, e.depth));
                    seen.properties = block
                        .dyn_deps
                        .set_properties(e.properties)
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect();
                }
                _ => {}
            }
        }
    }
    out
}

fn bridge_dispatch() -> Dispatch {
    bridge_dispatch_with(default_telemetry())
}

fn bridge_dispatch_with(telemetry: fontaine_core::config::Telemetry) -> Dispatch {
    let layer = fontaine::telemetry::filtered(
        SpanBridge::new(
            MicromegasExport::new(true),
            ContentPolicy::from_config(&telemetry),
        ),
        is_exported_span,
    );
    Dispatch::new(tracing_subscriber::registry().with(layer))
}

fn default_telemetry() -> fontaine_core::config::Telemetry {
    let w = tempfile::tempdir().unwrap();
    let dirs = tempfile::tempdir().unwrap();
    fontaine_core::config::load(&fontaine_core::config::LoadParams {
        launch_dir: w.path(),
        config_dir: dirs.path(),
        data_dir: dirs.path(),
        settings_file: None,
        cli_rules: Default::default(),
        env: &|_| None,
    })
    .unwrap()
    .telemetry
}

/// A thread that emits span events registers its stream with micromegas and must unregister
/// it before exiting.
fn with_stream<T>(f: impl FnOnce() -> T) -> T {
    micromegas_tracing::dispatch::init_thread_stream();
    let out = f();
    flush_thread_buffer();
    micromegas_tracing::dispatch::unregister_thread_stream();
    out
}

#[test]
fn a_span_tree_round_trips_into_async_span_events() {
    let _serial = serial();
    let guard = init_in_memory_tracing();
    let dispatch = bridge_dispatch();

    // The tree opens on this thread, a task on a tokio worker adds a sibling, and every span
    // closes on yet another thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .with_tracing_callbacks()
        .build()
        .unwrap();

    let (root, child, leaf) = with_stream(|| {
        tracing::dispatcher::with_default(&dispatch, || {
            let root = info_span!(target: TARGET, "root", phase = "outer");
            let child = info_span!(target: TARGET, parent: &root, "child");
            let leaf = info_span!(target: TARGET, parent: &child, "leaf", answer = 42u64);
            (root, child, leaf)
        })
    });

    runtime.block_on(async {
        let parent = root.clone();
        let dispatch = dispatch.clone();
        tokio::spawn(async move {
            let span = tracing::dispatcher::with_default(
                &dispatch,
                || info_span!(target: TARGET, parent: &parent, "worker"),
            );
            tokio::task::yield_now().await;
            tracing::dispatcher::with_default(&dispatch, || drop(span));
        })
        .await
        .unwrap();
    });

    let closing = dispatch.clone();
    std::thread::spawn(move || {
        // Closing a span releases its parent through the thread's default subscriber.
        let _default = tracing::dispatcher::set_default(&closing);
        with_stream(|| {
            drop(leaf);
            drop(child);
            drop(root);
        });
    })
    .join()
    .unwrap();
    drop(runtime);

    let blocks = guard.sink.state.lock().unwrap().thread_blocks.clone();
    let seen = spans(&blocks);
    let by_name = |name: &str| {
        seen.iter()
            .find(|(_, s)| s.name == name)
            .unwrap_or_else(|| panic!("no span named {name}: {seen:#?}"))
    };
    let (root_id, root) = by_name("root");
    let (child_id, child) = by_name("child");
    let (_, leaf) = by_name("leaf");
    let (_, worker) = by_name("worker");

    for (name, span) in [
        ("root", root),
        ("child", child),
        ("leaf", leaf),
        ("worker", worker),
    ] {
        assert!(
            span.begin.is_some() && span.end.is_some(),
            "{name} has a begin and an end: {seen:#?}"
        );
        assert_eq!(
            span.begin, span.end,
            "{name}: the end event repeats the begin's parent and depth"
        );
    }
    assert_eq!(root.begin, Some((0, 0)), "a root has parent 0 and depth 0");
    assert_eq!(child.begin, Some((*root_id, 1)));
    assert_eq!(leaf.begin, Some((*child_id, 2)));
    assert_eq!(worker.begin, Some((*root_id, 1)));
    assert!(
        leaf.properties
            .contains(&("answer".to_string(), "42".to_string())),
        "{:?}",
        leaf.properties
    );
    assert!(
        root.properties
            .contains(&("phase".to_string(), "outer".to_string())),
        "{:?}",
        root.properties
    );
    assert_eq!(seen.len(), 4, "one span per tracing span: {seen:#?}");
}

#[test]
fn spans_outside_the_allowlist_are_not_exported() {
    let _serial = serial();
    let guard = init_in_memory_tracing();
    let dispatch = bridge_dispatch();
    with_stream(|| {
        tracing::dispatcher::with_default(&dispatch, || {
            let kept = info_span!(target: TARGET, "kept");
            let other = info_span!(target: "hyper::client", "dependency");
            let kept_child = info_span!(target: TARGET, parent: &other, "under-dependency");
            drop(kept_child);
            drop(other);
            drop(kept);
        });
    });
    let blocks = guard.sink.state.lock().unwrap().thread_blocks.clone();
    let seen = spans(&blocks);
    let names: Vec<&str> = seen.values().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"kept") && names.contains(&"under-dependency"));
    assert!(!names.contains(&"dependency"), "{names:?}");
    let child = seen
        .values()
        .find(|s| s.name == "under-dependency")
        .unwrap();
    assert_eq!(
        child.begin,
        Some((0, 0)),
        "a span under an unexported one attaches to the nearest exported ancestor"
    );
}

fn content_span_properties(telemetry: fontaine_core::config::Telemetry) -> Vec<(String, String)> {
    let guard = init_in_memory_tracing();
    let dispatch = bridge_dispatch_with(telemetry);
    with_stream(|| {
        tracing::dispatcher::with_default(&dispatch, || {
            let span = info_span!(
                target: "rig::completions",
                "chat",
                gen_ai.request.model = "m",
                gen_ai.prompt = "CANARY-PROMPT key sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
                gen_ai.completion = "CANARY-REPLY",
            );
            drop(span);
        });
    });
    let blocks = guard.sink.state.lock().unwrap().thread_blocks.clone();
    spans(&blocks)
        .into_values()
        .find(|s| s.name == "chat")
        .map(|s| s.properties)
        .unwrap()
}

#[test]
fn span_content_stays_out_of_end_events_without_the_opt_in_keys() {
    let _serial = serial();
    let properties = content_span_properties(default_telemetry());
    assert!(
        properties.iter().any(|(k, _)| k == "gen_ai.request.model"),
        "{properties:?}"
    );
    assert!(
        !properties
            .iter()
            .any(|(k, v)| k.contains("prompt") || k.contains("completion") || v.contains("CANARY")),
        "{properties:?}"
    );
}

#[test]
fn opted_in_span_content_is_redacted_in_end_events() {
    let _serial = serial();
    let mut telemetry = default_telemetry();
    telemetry.log_user_prompts = true;
    let properties = content_span_properties(telemetry);
    let prompt = &properties
        .iter()
        .find(|(k, _)| k == "gen_ai.prompt")
        .unwrap()
        .1;
    assert!(prompt.contains("CANARY-PROMPT") && prompt.contains("[REDACTED]"));
    assert!(!prompt.contains("sk-ant"));
    assert!(
        !properties.iter().any(|(k, _)| k == "gen_ai.completion"),
        "the response leg has its own key"
    );
}

async fn ingestion_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_binary_authenticates_and_posts_to_the_ingestion_endpoint() {
    let ingestion = ingestion_server().await;
    let model = Endpoint::start(Wire::Chat, vec![Turn::text("hello")]).await;
    let mut s = Sandbox::compatible(&model.url());
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n\n[telemetry]\nenabled = true\nbackend = \"micromegas\"\nendpoint = \"{}\"\napi_key_env = \"INGEST_KEY\"\n",
        model.url(),
        ingestion.uri()
    ));
    s.set_env("INGEST_KEY", "ingest-secret");
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "hello\n");

    let requests = ingestion.received_requests().await.unwrap();
    let paths: Vec<&str> = requests.iter().map(|r| r.url.path()).collect();
    assert!(paths.contains(&"/ingestion/insert_process"), "{paths:?}");
    assert!(paths.contains(&"/ingestion/insert_block"), "{paths:?}");
    // The span events ride a per-thread `cpu` stream, which exists only because the sink builder
    // turned CPU tracing on; nothing sets the environment variable here.
    let streams: Vec<&wiremock::Request> = requests
        .iter()
        .filter(|r| r.url.path() == "/ingestion/insert_stream")
        .collect();
    assert!(
        streams
            .iter()
            .any(|r| r.body.windows(3).any(|w| w == b"cpu")),
        "no cpu stream was announced"
    );
    for request in &requests {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .map(|v| v.to_str().unwrap()),
            Some("Bearer ingest-secret"),
            "{}",
            request.url
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_telemetry_builds_nothing_even_with_endpoints_in_the_environment() {
    let ingestion = ingestion_server().await;
    let collector = ingestion_server().await;
    let model = Endpoint::start(Wire::Chat, vec![Turn::text("hello")]).await;
    let mut s = Sandbox::compatible(&model.url());
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n\n[telemetry]\nenabled = false\n",
        model.url()
    ));
    s.set_env("MICROMEGAS_TELEMETRY_URL", &ingestion.uri());
    s.set_env("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.uri());
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(ingestion.received_requests().await.unwrap().is_empty());
    assert!(collector.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exporter_that_cannot_start_prints_one_notice_and_the_run_continues() {
    let ingestion = ingestion_server().await;
    let model = Endpoint::start(Wire::Chat, vec![Turn::text("hello")]).await;
    let mut s = Sandbox::compatible(&model.url());
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n\n[telemetry]\nenabled = true\nendpoint = \"{}\"\napi_key_env = \"INGEST_KEY\"\n",
        model.url(),
        ingestion.uri()
    ));
    s.set_env("INGEST_KEY", "ingest-secret");
    // micromegas refuses a level it cannot parse when the sink is built.
    s.set_env("MICROMEGAS_TELEMETRY_SINK_MAX_LEVEL", "loud");
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "hello\n");
    let notices: Vec<&str> = out
        .stderr
        .lines()
        .filter(|l| l.contains("telemetry export is off"))
        .collect();
    assert_eq!(notices.len(), 1, "{}", out.stderr);
    assert!(ingestion.received_requests().await.unwrap().is_empty());
}

#[test]
fn exported_span_locations_carry_no_file_path() {
    let _serial = serial();
    let guard = init_in_memory_tracing();
    let dispatch = bridge_dispatch();
    with_stream(|| {
        tracing::dispatcher::with_default(&dispatch, || {
            drop(info_span!(target: TARGET, "located"));
        });
    });
    let blocks = guard.sink.state.lock().unwrap().thread_blocks.clone();
    let mut checked = 0;
    for block in &blocks {
        for event in block.events.iter() {
            let location = match event {
                ThreadEventQueueAny::BeginAsyncDynNamedSpanEvent(e) => e.span_location,
                ThreadEventQueueAny::EndAsyncDynNamedSpanEvent(e) => e.span_location,
                ThreadEventQueueAny::TaggedEndAsyncNamedSpanEvent(e) => e.span_location,
                _ => continue,
            };
            assert_eq!((location.file, location.line), ("", 0));
            checked += 1;
        }
    }
    assert!(checked > 0);
}

/// `(name, parent name, depth)` of every span, sorted so the comparison ignores event order.
fn tree(blocks: &[Arc<ThreadBlock>]) -> Vec<(String, String, u32)> {
    let seen = spans(blocks);
    let mut out: Vec<_> = seen
        .values()
        .map(|s| {
            let (parent, depth) = s.begin.expect("every span has a begin");
            let parent_name = seen.get(&parent).map_or("", |p| p.name.as_str());
            (s.name.clone(), parent_name.to_string(), depth)
        })
        .collect();
    out.sort();
    out
}

/// The Fontaine span shapes, plus a `rig` span whose direct parent is not allowlisted. Parents
/// are the entered spans: an explicit parent that is itself disabled would make a root.
fn span_program() {
    let session = info_span!(target: TARGET, "fontaine.session").entered();
    let interaction = info_span!(target: TARGET, "fontaine.interaction").entered();
    drop(info_span!(target: "rig::completions", "chat"));
    let tool = info_span!(target: TARGET, "fontaine.tool").entered();
    let permission = info_span!(target: TARGET, "fontaine.permission").entered();
    let guardian = info_span!(target: TARGET, "fontaine.guardian").entered();
    let chat = info_span!(target: "rig::completions", "chat").entered();
    let hyper = info_span!(target: "hyper::client", "connect").entered();
    drop(info_span!(target: "rig::agent_chat", "invoke_agent"));
    drop(hyper);
    drop(chat);
    drop(guardian);
    drop(permission);
    drop(tool);
    drop(interaction);
    drop(session);
}

fn tree_through(
    layer: fontaine::telemetry::BoxLayer<tracing_subscriber::Registry>,
) -> Vec<(String, String, u32)> {
    let guard = init_in_memory_tracing();
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));
    with_stream(|| tracing::dispatcher::with_default(&dispatch, span_program));
    let blocks = guard.sink.state.lock().unwrap().thread_blocks.clone();
    tree(&blocks)
}

#[test]
fn the_upstream_span_capture_builds_the_same_tree_as_the_bridge() {
    let _serial = serial();
    let telemetry = default_telemetry();
    let bridge = tree_through(fontaine::telemetry::filtered(
        SpanBridge::new(
            MicromegasExport::new(true),
            ContentPolicy::from_config(&telemetry),
        ),
        is_exported_span,
    ));
    let upstream = tree_through(fontaine::telemetry::filtered(
        micromegas_telemetry_sink::tracing_interop::TracingCaptureLayer {
            max_level: micromegas_tracing::levels::LevelFilter::Info,
            capture_spans: true,
        },
        is_exported_span,
    ));
    assert!(
        bridge
            .iter()
            .any(|(name, parent, depth)| name == "invoke_agent" && parent == "chat" && *depth == 6),
        "a rig span under a non-allowlisted span lands under its nearest exported ancestor: {bridge:#?}"
    );
    assert_eq!(bridge, upstream);
}
