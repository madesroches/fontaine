//! Lives in a binary of its own: a thread-local subscriber races with callsite registration in
//! tests that run beside it.

mod mcp_common;

use std::sync::{Arc, Mutex};

use fontaine_tools::mcp::{ElicitForm, ElicitReply, Elicitor, connect_transport};
use futures::future::BoxFuture;
use mcp_common::server::{Double, elicit_tool};
use mcp_common::{Gate, call, options, tool_server};
use rmcp::ServiceExt;
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

/// Answers with each queued reply in turn.
struct Queue(Mutex<Vec<ElicitReply>>);

impl Elicitor for Queue {
    fn elicit<'a>(&'a self, _form: ElicitForm) -> BoxFuture<'a, ElicitReply> {
        let reply = self.0.lock().unwrap().remove(0);
        Box::pin(async move { reply })
    }
}

#[tokio::test]
async fn each_elicitation_answer_emits_an_event_with_its_action_and_nothing_typed() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut options = options(Gate::new(true));
    options.elicitor = Arc::new(Queue(Mutex::new(vec![
        ElicitReply::Accept(serde_json::Map::from_iter([(
            "name".to_string(),
            json!("hunter2-CANARY"),
        )])),
        ElicitReply::Decline,
        ElicitReply::Cancel,
    ])));
    let handle = tool_server();
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    let double = Double::with_tools(vec![elicit_tool()]);
    tokio::spawn(async move {
        let running = double.serve((sfc, s2c)).await.expect("server start");
        let _ = running.waiting().await;
    });
    let connected = connect_transport("double", (cfs, c2s), handle.clone(), &options)
        .await
        .expect("connect");
    for _ in 0..3 {
        call(&handle, "mcp__double__elicit", "{}").await.unwrap();
    }
    connected.connection.close().await;

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let events: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("mcp_elicitation"))
        .collect();
    assert_eq!(events.len(), 3, "{log}");
    for (event, action) in events.iter().zip(["accept", "decline", "cancel"]) {
        assert!(event.contains(&format!("action=\"{action}\"")), "{event}");
        assert!(event.contains("server=\"double\""), "{event}");
    }
    assert!(
        !log.contains("CANARY") && !log.contains("Who are you"),
        "{log}"
    );
}
