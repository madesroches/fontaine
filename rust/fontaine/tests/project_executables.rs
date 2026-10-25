//! Headless `-p` loads what a repository defines without asking, as Claude Code does, and in
//! both surfaces nothing it defines can decide a permission.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};

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

fn project_settings(s: &Sandbox, text: &str) {
    s.add_file(".fontaine/settings.toml", text);
}

fn of_type<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|l| l["type"] == kind).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_mcp_server_loads_without_a_prompt_and_its_tools_are_gated_like_any_arbitrary_call()
 {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "mcp__repo__lookup", json!({})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    project_settings(
        &s,
        &format!(
            "[mcp.servers.repo]\ncommand = '{}'\n",
            example("repo_mcp_server")
        ),
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    // Loaded: its tools are registered under the Claude Code names.
    let tools: Vec<&str> = lines[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"mcp__repo__echo") && tools.contains(&"mcp__repo__lookup"),
        "{tools:?}"
    );
    // Gated: the one that declares itself read-only still has no fast path, so in a headless
    // run that cannot ask it is denied by the mode.
    let decision = of_type(&lines, "gate_decision")[0];
    assert_eq!(decision["tool"], "mcp__repo__lookup");
    assert_eq!(decision["tier"], "mode");
    assert_eq!(decision["decision"], "deny");
    assert_eq!(of_type(&lines, "tool_result")[0]["success"], json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_user_rule_lets_a_project_mcp_tool_run() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "mcp__repo__echo", json!({"x": 1})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    project_settings(
        &s,
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
            "mcp__repo__echo",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "tool_result")[0];
    assert_eq!(result["success"], json!(true), "{result}");
    assert!(
        result["text"].as_str().unwrap().contains("echo"),
        "{result}"
    );
    assert_eq!(of_type(&lines, "gate_decision")[0]["tier"], "allow_rule");
}

#[tokio::test(flavor = "multi_thread")]
async fn config_env_reaches_an_mcp_server_process() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "mcp__repo__env", json!({"name": "PROBE"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let path = s.config.join("config.toml");
    let base = std::fs::read_to_string(&path).unwrap();
    s.user_config(&format!(
        "{base}\n[env]\nPROBE = \"probe-value-xyz\"\n\n[mcp.servers.repo]\ncommand = '{}'\n",
        example("repo_mcp_server")
    ));
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "mcp__repo__env",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "tool_result")[0];
    assert_eq!(result["success"], json!(true), "{result}");
    assert!(
        result["text"].as_str().unwrap().contains("probe-value-xyz"),
        "{result}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_hook_runs_without_a_prompt_but_its_allow_decides_nothing() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "shell", json!({"command": "cargo test"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let marker = s.work.join("hook-ran.json");
    let allow = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "allow"}})
    .to_string();
    project_settings(
        &s,
        &format!(
            "[[hooks.pre_tool_use]]\ncommand = '''\n'{}' '{}' '{}'\n'''\n",
            example("hook_marker"),
            marker.display(),
            allow
        ),
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    // It ran, and was told about the call.
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
    assert_eq!(event["tool_name"], "shell");
    assert_eq!(event["hook_event_name"], "PreToolUse");
    // Its allow did not approve anything: the call still went to the mode, which denied it.
    let lines = out.lines();
    let decision = of_type(&lines, "gate_decision")[0];
    assert_eq!(decision["tier"], "mode");
    assert_eq!(decision["decision"], "deny");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_hook_in_the_users_own_settings_does_allow() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "shell",
                json!({"command": "echo from-a-hook-allowed-call"}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let marker = s.work.join("hook-ran.json");
    let allow = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "allow"}})
    .to_string();
    let config = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!(
        "{config}\n[[hooks.pre_tool_use]]\nmatcher = \"shell(echo from-a-hook-allowed-call)\"\ncommand = '''\n'{}' '{}' '{}'\n'''\n",
        example("hook_marker"),
        marker.display(),
        allow
    ));
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let decision = of_type(&lines, "gate_decision")[0];
    assert_eq!(decision["tier"], "hook");
    assert_eq!(decision["decision"], "allow");
}
