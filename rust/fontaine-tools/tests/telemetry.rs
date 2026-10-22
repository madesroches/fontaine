//! Lives in a binary of its own: a thread-local subscriber races with callsite registration in
//! tests that run beside it.

mod common;

use std::sync::{Arc, Mutex};

use common::{AllowAll, Dirs, call, env_with, registry, scope};
use serde_json::json;

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
}
