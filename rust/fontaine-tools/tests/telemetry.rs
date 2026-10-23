//! Lives in a binary of its own: a thread-local subscriber races with callsite registration in
//! tests that run beside it.

mod common;

use std::sync::{Arc, Mutex};

use common::{AllowAll, Dirs, call, env_with, registry, scope};
use fontaine_tools::gate::CallView;
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::{
    ResultPipeline, ResultStage, ScanRecord, ScanVerdict, StageKind, ToolOutcome,
};
use futures::future::BoxFuture;
use serde_json::json;

/// A scanner stage that reports a flagged scan without touching the result.
struct Reporting;

impl ResultStage for Reporting {
    fn process<'a>(
        &'a self,
        _call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { outcome })
    }

    fn process_reported<'a>(
        &'a self,
        _call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, (ToolOutcome, Option<ScanRecord>)> {
        Box::pin(async move {
            (
                outcome,
                Some(ScanRecord {
                    verdict: ScanVerdict::Flagged,
                    latency_ms: 7,
                }),
            )
        })
    }
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

#[tokio::test]
async fn each_call_emits_a_span_and_a_content_free_tool_result_event() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let d = Dirs::new();
    let root = d.dir("work");
    std::fs::write(root.join("secret-name-CANARY.txt"), "body-CANARY").unwrap();
    let tools = registry(&env_with(scope(&root, &[]), Arc::new(AllowAll::default())));
    call(&tools, "read", json!({"path": "secret-name-CANARY.txt"}))
        .await
        .unwrap();

    let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("fontaine.tool"), "{logged}");
    assert!(logged.contains("fontaine::telemetry"), "{logged}");
    for field in [
        "tool_name=\"read\"",
        "risk_class=\"read_only\"",
        "tool_source=\"builtin\"",
        "success=true",
        "duration_ms=",
        "args_bytes=",
        "result_bytes=",
    ] {
        assert!(logged.contains(field), "missing {field} in {logged}");
    }
    assert!(
        !logged.contains("CANARY"),
        "no content in tool telemetry: {logged}"
    );
    assert!(
        !logged.contains("scanner_verdict") && !logged.contains("scanner_latency_ms"),
        "no scan ran: {logged}"
    );
}

#[tokio::test]
async fn a_scan_adds_its_verdict_and_latency_to_the_tool_result_event_and_nothing_else() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let d = Dirs::new();
    let root = d.dir("work");
    std::fs::write(root.join("a.txt"), "body-CANARY").unwrap();
    let mut env = env_with(scope(&root, &[]), Arc::new(AllowAll::default()));
    env.guard = Guard {
        gate: env.guard.gate.clone(),
        results: Arc::new(
            ResultPipeline::new().with_stage(StageKind::Scanner, Arc::new(Reporting)),
        ),
        hide_mcp_names: false,
    };
    let tools = registry(&env);
    call(&tools, "read", json!({"path": "a.txt"}))
        .await
        .unwrap();

    let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("scanner_verdict=\"flagged\""), "{logged}");
    assert!(logged.contains("scanner_latency_ms=7"), "{logged}");
    assert!(!logged.contains("CANARY"), "{logged}");
}
