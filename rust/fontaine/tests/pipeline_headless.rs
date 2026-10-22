//! `fontaine -p` with the real permission pipeline, against the scripted endpoint: nothing
//! prompts, every would-prompt outcome is a denial the model reads, and the run goes on.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};

fn of_type<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|l| l["type"] == kind).collect()
}

fn audit(s: &Sandbox, session: &str) -> Vec<Value> {
    std::fs::read_to_string(s.data.join("audit").join(format!("{session}.jsonl")))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn session_of(lines: &[Value]) -> String {
    lines[0]["session_id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_would_prompt_outcome_is_a_denial_the_run_continues_and_exit_is_zero() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "write", json!({"path": "new.txt", "content": "x"})),
            Turn::tool("c2", "shell", json!({"command": "touch made.txt"})),
            Turn::tool("c3", "shell", json!({"command": "rm -rf /"})),
            Turn::tool(
                "c4",
                "write",
                json!({"path": ".git/config", "content": "x"}),
            ),
            Turn::tool("c5", "webfetch", json!({"url": "http://127.0.0.1:9/"})),
            Turn::tool("c6", "read", json!({"path": "ok.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("ok.txt", "fine\n");
    let out = s
        .headless(&["--output-format", "stream-json", "try things"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();

    let results = of_type(&lines, "tool_result");
    assert_eq!(results.len(), 6);
    for result in &results[..5] {
        assert_eq!(result["success"], json!(false), "{result}");
        assert!(
            result["text"].as_str().unwrap().contains("denied"),
            "{result}"
        );
    }
    assert_eq!(results[5]["success"], json!(true), "the read still ran");
    assert!(!s.work.join("new.txt").exists());
    assert!(!s.work.join("made.txt").exists());

    let decisions = of_type(&lines, "gate_decision");
    assert_eq!(decisions.len(), 6);
    let tiers: Vec<_> = decisions
        .iter()
        .map(|d| d["tier"].as_str().unwrap())
        .collect();
    assert_eq!(
        tiers,
        [
            "mode",
            "mode",
            "critical_path",
            "protected_path",
            "mode",
            "read_only_fast_path"
        ]
    );

    let last = of_type(&lines, "result")[0];
    assert_eq!(last["subtype"], "success");
    let denials = last["denials"].as_array().unwrap();
    assert_eq!(denials.len(), 5);
    let tools: Vec<_> = denials
        .iter()
        .map(|d| d["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["write", "shell", "shell", "write", "webfetch"]);
    for denial in denials {
        assert!(denial["tier"].is_string());
        assert!(denial.get("rule").is_some());
        assert!(denial["explanation"].as_str().unwrap().contains("denied"));
    }
    assert_eq!(denials[2]["tier"], "critical_path");

    // The model was told, as the tool result, and kept going.
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 7);
    assert!(requests[1].to_string().contains("headless run cannot ask"));
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_gate_decision_and_tool_call_ids_agree() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_a", "read", json!({"path": "ok.txt"})),
            Turn::tool("call_b", "shell", json!({"command": "touch x"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("ok.txt", "fine\n");
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let records = audit(&s, &session_of(&lines));
    let calls: Vec<_> = of_type(&lines, "tool_call")
        .iter()
        .map(|c| c["tool_use_id"].clone())
        .collect();
    let decisions: Vec<_> = of_type(&lines, "gate_decision")
        .iter()
        .map(|d| {
            (
                d["tool_use_id"].clone(),
                d["tier"].clone(),
                d["decision"].clone(),
            )
        })
        .collect();
    assert_eq!(records.len(), 2);
    for (i, record) in records.iter().enumerate() {
        assert_eq!(record["tool_use_id"], calls[i]);
        assert_eq!(decisions[i].0, calls[i]);
        assert_eq!(decisions[i].1, record["tier"]);
        assert_eq!(decisions[i].2, record["decision"]);
        assert_eq!(record["agent_id"], "root");
        assert!(record["args_digest"].as_str().unwrap().len() == 64);
        assert!(
            record.get("args").is_none(),
            "raw arguments are never logged"
        );
    }
    assert_eq!(calls, [json!("call_a"), json!("call_b")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn dont_ask_and_plan_modes_deny_with_their_own_tiers() {
    for (mode, tiers) in [
        ("dontAsk", ["dont_ask", "dont_ask"]),
        // The edit is blocked outright; the shell call goes to the guardian, which here is the
        // scripted main model and answers in prose, so the call fails closed.
        ("plan", ["plan_block", "guardian"]),
    ] {
        let endpoint = Endpoint::start(
            Wire::Chat,
            vec![
                Turn::tool("c1", "write", json!({"path": "new.txt", "content": "x"})),
                Turn::tool("c2", "shell", json!({"command": "touch x"})),
                Turn::text("done"),
            ],
        )
        .await;
        let s = Sandbox::compatible(&endpoint.url());
        let out = s
            .run(&[
                "-p",
                "--permission-mode",
                mode,
                "--output-format",
                "stream-json",
                "go",
            ])
            .await;
        assert_eq!(out.code, Some(0), "{mode}: {}", out.stderr);
        let lines = out.lines();
        let got: Vec<_> = of_type(&lines, "gate_decision")
            .iter()
            .map(|d| d["tier"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, tiers, "{mode}");
        assert!(!s.work.join("new.txt").exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rule_flags_act_as_cli_scope_rules() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "shell", json!({"command": "touch made.txt"})),
            Turn::tool("c2", "read", json!({"path": "secret.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("secret.txt", "shh\n");
    let out = s
        .headless(&[
            "--allowed-tools",
            "shell(touch made.txt)",
            "--disallowed-tools",
            "read(secret.txt)",
            "--output-format",
            "stream-json",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(
        s.work.join("made.txt").exists(),
        "the allow rule ran the command"
    );
    let decisions = of_type(&lines, "gate_decision");
    assert_eq!(decisions[0]["tier"], "allow_rule");
    assert_eq!(decisions[0]["rule"], "shell(touch made.txt) [cli]");
    assert_eq!(decisions[1]["tier"], "deny_rule");
    assert_eq!(decisions[1]["rule"], "read(secret.txt) [cli]");
    let denials = of_type(&lines, "result")[0]["denials"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0]["rule"], "read(secret.txt) [cli]");
    assert!(!out.stdout.contains("shh"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_rule_flag_is_a_usage_error() {
    let s = Sandbox::new();
    let out = s.run(&["-p", "--allowed-tools", "shell(", "hello"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("--allowed-tools"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_audit_log_is_written_without_telemetry() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "shell", json!({"command": "ls"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let records = audit(&s, &session_of(&out.lines()));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["tier"], "tier05");
    assert_eq!(records[0]["decision"], "allow");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_deny_rule_still_binds_the_run() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "secret.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("secret.txt", "shh\n");
    s.add_file(
        ".fontaine/settings.toml",
        "[permissions]\ndeny = [\"read(secret.txt)\"]\n",
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let decision = of_type(&lines, "gate_decision")[0].clone();
    assert_eq!(decision["tier"], "deny_rule");
    assert_eq!(decision["rule"], "read(secret.txt) [project]");
}
