//! A forked subagent through `fontaine -p`. A headless run is a single turn, so the stored
//! conversation is empty while the fork is made: the fork must still start with the turn's
//! user message and what the turn did before forking.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};

fn sandbox(endpoint: &Endpoint) -> Sandbox {
    let s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n",
        endpoint.url()
    ));
    s.add_file("a.txt", "a file\n");
    s
}

fn fork_call() -> Turn {
    Turn::tool(
        "c2",
        "agent",
        json!({"agent_type": "fork", "prompt": "fork-task: report", "background": false}),
    )
}

fn roles(request: &Value) -> Vec<&str> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_starts_with_the_turns_user_message_and_then_its_task() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            fork_call(),
            Turn::text("fork done"),
            Turn::text("root done"),
        ],
    )
    .await;
    let s = sandbox(&endpoint);
    let out = s
        .headless(&["--allowed-tools", "agent", "root-task: fork yourself"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 3);
    let fork = &requests[1];
    let text = fork.to_string();
    assert!(text.contains("root-task: fork yourself"), "{text}");
    assert!(text.contains("forked subagent"), "{text}");
    assert!(text.contains("fork-task: report"), "{text}");
    // The task follows an assistant turn, so no two user turns meet.
    let roles = roles(fork);
    assert_eq!(roles, ["system", "user", "assistant", "user"], "{roles:?}");
    assert!(
        !requests[0]
            .to_string()
            .contains("forked subagent of the conversation")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_also_sees_what_the_turn_did_before_forking() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "a.txt"})),
            fork_call(),
            Turn::text("fork done"),
            Turn::text("root done"),
        ],
    )
    .await;
    let s = sandbox(&endpoint);
    let out = s
        .headless(&[
            "--allowed-tools",
            "agent",
            "--allowed-tools",
            "read",
            "root-task: read then fork",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 4);
    let fork = &requests[2];
    let text = fork.to_string();
    assert!(text.contains("root-task: read then fork"), "{text}");
    assert!(text.contains("a.txt"), "{text}");
    assert!(text.contains("a file"), "{text}");
    let roles = roles(fork);
    assert_eq!(
        roles,
        ["system", "user", "assistant", "tool", "assistant", "user"],
        "{roles:?}"
    );
}
