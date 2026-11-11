//! Headless `fontaine -p` against the in-repo example server: the resource tools are in the
//! toolset, and a `read_mcp_resource` result reaches the model through the gate.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::json;

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
async fn the_resource_tools_list_and_read_through_the_gate() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "list_mcp_resources", json!({})),
            Turn::tool(
                "c2",
                "read_mcp_resource",
                json!({"server": "repo", "uri": "repo://info"}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file(
        ".fontaine/settings.toml",
        &format!(
            "[mcp.servers.repo]\ncommand = '{}'\n",
            example("repo_mcp_server")
        ),
    );
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "list_mcp_resources",
            "--allowed-tools",
            "read_mcp_resource",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();

    // The pair is always registered, next to the server's own tools.
    let tools: Vec<&str> = lines[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    for tool in ["list_mcp_resources", "read_mcp_resource", "mcp__repo__echo"] {
        assert!(tools.contains(&tool), "{tool} missing from {tools:?}");
    }

    // The listing names the server and the uri; the read's result reaches the model.
    let results: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|l| l["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 2, "{lines:?}");
    assert_eq!(results[0]["success"], json!(true));
    let listed = results[0]["text"].as_str().unwrap();
    assert!(listed.contains("repo: repo://info"), "{listed}");
    assert_eq!(results[1]["success"], json!(true), "{results:?}");
    assert!(
        results[1]["text"]
            .as_str()
            .unwrap()
            .contains("contents of repo://info"),
        "{results:?}"
    );
    for decision in lines.iter().filter(|l| l["type"] == "gate_decision") {
        assert_eq!(decision["tier"], "allow_rule", "{decision}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_allow_rule_the_read_is_denied_like_any_arbitrary_call() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "read_mcp_resource",
                json!({"server": "repo", "uri": "repo://info"}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file(
        ".fontaine/settings.toml",
        &format!(
            "[mcp.servers.repo]\ncommand = '{}'\n",
            example("repo_mcp_server")
        ),
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let decision = lines
        .iter()
        .find(|l| l["type"] == "gate_decision")
        .expect("a decision");
    assert_eq!(decision["tool"], "read_mcp_resource");
    assert_eq!(decision["tier"], "mode");
    assert_eq!(decision["decision"], "deny");
    let result = lines
        .iter()
        .find(|l| l["type"] == "tool_result")
        .expect("a result");
    assert_eq!(result["success"], json!(false));
    assert!(result["text"].as_str().unwrap().contains("denied"));
}
