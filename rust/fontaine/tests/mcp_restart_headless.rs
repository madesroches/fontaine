//! Headless `fontaine -p` with a stdio MCP server that exits after every call: the next call
//! starts it again through the tool's own registration, until the restart budget is spent.

mod common;

use common::{Endpoint, Route, Sandbox, Turn, Wire};
use fontaine_testkit::example_bin;
use serde_json::{Value, json};
use std::time::Duration;

fn sandbox(url: &str) -> Sandbox {
    let s = Sandbox::compatible(url);
    s.add_file(
        ".fontaine/settings.toml",
        &format!(
            "[mcp.servers.flaky]\ncommand = '{}'\n",
            example_bin("fontaine", "exiting_mcp_server").to_string_lossy()
        ),
    );
    s
}

fn echo_calls(count: usize) -> Vec<Turn> {
    let mut turns: Vec<Turn> = (0..count)
        .map(|n| Turn::tool(&format!("c{n}"), "mcp__flaky__echo", json!({ "n": n })))
        .collect();
    turns.push(Turn::text("done"));
    turns
}

fn results(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|l| l["type"] == "tool_result")
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_after_the_server_exited_starts_it_again_and_succeeds() {
    // The delay gives the watcher time to see the exit before the next call.
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![Route::new("go", echo_calls(2)).slow(Duration::from_millis(600))],
    )
    .await;
    let s = sandbox(&endpoint.url());
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "mcp__flaky__echo",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stderr, out.stdout);
    let lines = out.lines();
    let results = results(&lines);
    assert_eq!(results.len(), 2, "{lines:?}");
    for (n, result) in results.iter().enumerate() {
        assert_eq!(result["success"], json!(true), "{result}");
        assert!(
            result["text"]
                .as_str()
                .unwrap()
                .contains(&format!("\"n\":{n}")),
            "{result}"
        );
    }
    let process = |result: &Value| {
        let text = result["text"].as_str().unwrap();
        text.split("process ")
            .nth(1)
            .unwrap()
            .split(':')
            .next()
            .unwrap()
            .to_string()
    };
    assert_ne!(process(results[0]), process(results[1]), "{results:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn exits_past_the_restart_budget_fail_the_call_with_an_error() {
    // One call per process: the first runs on the initial server, three more on restarts, and
    // the fifth finds the server given up on.
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![Route::new("go", echo_calls(5)).slow(Duration::from_millis(600))],
    )
    .await;
    let s = sandbox(&endpoint.url());
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "mcp__flaky__echo",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}\n{}", out.stderr, out.stdout);
    let lines = out.lines();
    let results = results(&lines);
    assert_eq!(results.len(), 5, "{lines:?}");
    for result in &results[..4] {
        assert_eq!(result["success"], json!(true), "{result}");
    }
    assert_eq!(results[4]["success"], json!(false), "{}", results[4]);
    assert!(
        !results[4]["text"].as_str().unwrap().is_empty(),
        "the failure says something: {}",
        results[4]
    );
}
