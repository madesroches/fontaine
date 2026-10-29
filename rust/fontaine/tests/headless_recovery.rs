//! A model stream cut short mid-body: the turn retries instead of failing, and the recovered
//! conversation keeps the interrupted prompt, so the next turn still sees the task.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_stream_is_retried_and_the_interrupted_turn_survives() {
    let endpoint =
        Endpoint::start(Wire::Chat, vec![Turn::cut("par"), Turn::text("recovered")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&["--output-format", "stream-json", "first task"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout.contains("recovered"), "{}", out.stdout);
    let lines = out.lines();
    let id = lines[0]["session_id"].as_str().unwrap().to_string();
    assert_eq!(
        endpoint.requests().await.len(),
        2,
        "the cut answer was retried once"
    );

    let again = s
        .headless(&["--output-format", "stream-json", "--resume", &id, "retry"])
        .await;
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 3);
    let messages = serde_json::to_string(&requests[2]["messages"]).unwrap();
    assert!(
        messages.contains("first task"),
        "the resumed turn still sees the interrupted prompt: {messages}"
    );
}
