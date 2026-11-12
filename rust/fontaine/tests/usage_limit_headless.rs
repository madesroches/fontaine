//! A spent usage window under `fontaine -p`: the run waits up to a cap or exits at once, ends
//! with exit code 3 and its own `result` subtype, and tells a throttle apart.

mod common;

use std::path::Path;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::Value;

const WINDOW: &str = r#"{"error":{"message":"You've reached your 5-hour usage limit.","type":"exceeded_current_quota_error"}}"#;
const THROTTLE: &str = r#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#;

fn validator() -> jsonschema::Validator {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/headless-v1.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{value}\n{}", errors.join("\n"));
}

fn refusal(body: &str, times: usize) -> Vec<Turn> {
    (0..times)
        .map(|_| Turn::Reply(429, body.to_string()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_ends_the_run_at_once_with_code_3_and_names_the_account() {
    let endpoint = Endpoint::start(Wire::Chat, refusal(WINDOW, 5)).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&["--on-usage-limit=exit", "--output-format", "json", "hi"])
        .await;
    assert_eq!(out.code, Some(3), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(lines.len(), 1);
    assert_valid(&validator(), &lines[0]);
    let result = &lines[0];
    assert_eq!(result["subtype"], "error_usage_limit");
    assert_eq!(result["is_error"], true);
    assert_eq!(result["usage_limit"]["account"], "mock");
    assert_eq!(result["usage_limit"]["role"], "main");
    assert_eq!(result["usage_limit"]["end"], "cap_reached");
    assert!(
        result["error"].as_str().unwrap().contains("usage window"),
        "{result}"
    );
    assert_eq!(
        endpoint.requests().await.len(),
        1,
        "a spent window is not retried with a backoff"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_that_reaches_its_cap_exits_3_and_streams_the_pause_events() {
    let endpoint = Endpoint::start(Wire::Chat, refusal(WINDOW, 5)).await;
    let s = Sandbox::compatible(&endpoint.url());
    let started = std::time::Instant::now();
    let out = s
        .headless(&[
            "--max-usage-wait",
            "1s",
            "--output-format",
            "stream-json",
            "hi",
        ])
        .await;
    assert_eq!(out.code, Some(3), "{}", out.stderr);
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    let lines = out.lines();
    let v = validator();
    for line in &lines {
        assert_valid(&v, line);
    }
    let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    let paused = kinds
        .iter()
        .position(|k| *k == "usage_paused")
        .expect("paused");
    let resumed = kinds
        .iter()
        .position(|k| *k == "usage_resumed")
        .expect("resumed");
    assert!(paused < resumed && resumed < kinds.len() - 1, "{kinds:?}");
    assert_eq!(lines[resumed]["end"], "cap_reached");
    assert_eq!(lines.last().unwrap()["subtype"], "error_usage_limit");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_config_key_sets_exit_and_a_bad_wait_is_a_usage_error() {
    let endpoint = Endpoint::start(Wire::Chat, refusal(WINDOW, 5)).await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_config_file("extra.toml", "[headless]\non_usage_limit = \"exit\"\n");
    let extra = s.config.join("extra.toml");
    let out = s
        .headless(&["--settings", extra.to_str().unwrap(), "hi"])
        .await;
    assert_eq!(out.code, Some(3), "{}", out.stderr);

    let out = s.headless(&["--max-usage-wait", "soon", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    let out = s.headless(&["--max-usage-wait", "0s", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_throttle_is_retried_and_ends_as_an_ordinary_failure() {
    let endpoint = Endpoint::start(Wire::Chat, refusal(THROTTLE, 5)).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--output-format", "json", "hi"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let result = out.lines().remove(0);
    assert_eq!(result["subtype"], "error_during_execution");
    assert!(result.get("usage_limit").is_none());
    assert_eq!(endpoint.requests().await.len(), 3, "the normal retry");
}
