//! Headless `-p` loads what a repository defines without asking, as Claude Code does, and in
//! both surfaces nothing it defines can decide a permission.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use fontaine_testkit::example_bin;
use serde_json::{Value, json};

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
            example_bin("fontaine", "repo_mcp_server")
                .to_string_lossy()
                .into_owned()
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
            example_bin("fontaine", "repo_mcp_server")
                .to_string_lossy()
                .into_owned()
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
        example_bin("fontaine", "repo_mcp_server")
            .to_string_lossy()
            .into_owned()
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
            example_bin("fontaine", "hook_marker")
                .to_string_lossy()
                .into_owned(),
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
        example_bin("fontaine", "hook_marker").to_string_lossy().into_owned(),
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

#[tokio::test(flavor = "multi_thread")]
async fn an_mcp_tool_that_asks_for_input_in_a_headless_run_gets_cancel_and_the_run_completes() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "mcp__repo__elicit", json!({})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    project_settings(
        &s,
        &format!(
            "[mcp.servers.repo]\ncommand = '{}'\n",
            example_bin("fontaine", "repo_mcp_server")
                .to_string_lossy()
                .into_owned()
        ),
    );
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "mcp__repo__elicit",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "tool_result")[0];
    assert_eq!(result["success"], json!(true), "{result}");
    assert!(
        result["text"]
            .as_str()
            .unwrap()
            .contains(r#""action":"cancel""#),
        "{result}"
    );
}

fn require_trust(s: &Sandbox) {
    let base = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!("{base}\n[headless]\nrequire_trust = true\n"));
}

fn repo_server_settings() -> String {
    format!(
        "[mcp.servers.repo]\ncommand = '{}'\n",
        example_bin("fontaine", "repo_mcp_server")
            .to_string_lossy()
            .into_owned()
    )
}

fn tool_names(init: &Value) -> Vec<&str> {
    init["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn require_trust_skips_project_servers_and_hooks_in_an_untrusted_workspace() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "shell", json!({"command": "cargo test"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    require_trust(&s);
    let marker = s.work.join("hook-ran.json");
    project_settings(
        &s,
        &format!(
            "{}\n[[hooks.pre_tool_use]]\ncommand = '''\n'{}' '{}' '{{}}'\n'''\n",
            repo_server_settings(),
            example_bin("fontaine", "hook_marker")
                .to_string_lossy()
                .into_owned(),
            marker.display(),
        ),
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let init = &lines[0];
    assert!(
        !tool_names(init)
            .iter()
            .any(|t| t.starts_with("mcp__repo__")),
        "{init}"
    );
    assert_eq!(init["project_executables"], json!(false));
    assert!(!marker.exists(), "the project hook ran");
    assert!(
        out.stderr.contains("workspace is not trusted")
            && out.stderr.contains("[headless] require_trust = true"),
        "{}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn require_trust_loads_project_executables_in_a_trusted_workspace() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    require_trust(&s);
    fontaine_core::trust::record(&s.data, &s.work).unwrap();
    project_settings(&s, &repo_server_settings());
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let init = &lines[0];
    assert!(tool_names(init).contains(&"mcp__repo__echo"), "{init}");
    assert_eq!(init["project_executables"], json!(true));
    assert!(
        !out.stderr.contains("workspace is not trusted"),
        "{}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_require_trust_an_untrusted_workspace_still_loads_project_executables() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    project_settings(&s, &repo_server_settings());
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(tool_names(&lines[0]).contains(&"mcp__repo__echo"));
    assert_eq!(lines[0]["project_executables"], json!(true));
    assert!(
        !out.stderr.contains("workspace is not trusted"),
        "{}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_file_cannot_set_require_trust() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    project_settings(&s, "[headless]\nrequire_trust = false\n");
    let out = s.headless(&["go"]).await;
    assert_ne!(out.code, Some(0));
    assert!(out.stderr.contains("require_trust"), "{}", out.stderr);
}
