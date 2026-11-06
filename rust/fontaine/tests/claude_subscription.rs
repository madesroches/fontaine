//! `fontaine -p` on a `claude-subscription` account against the scripted CLI double
//! (`fontaine_claude_cli_double`, built as an example of this package; prebuild it when running
//! this test alone): the conversation, Fontaine's own gate around every tool call, usage without
//! cost, and the documented exit codes. The real CLI never runs here.

mod common;

use std::path::PathBuf;
use std::process::Stdio;

use common::{Output, Sandbox, ledger, write};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CONFLICTS: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

fn double() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "fontaine_claude_cli_double.exe"
    } else {
        "fontaine_claude_cli_double"
    };
    let path = dir.join(name);
    assert!(
        path.exists(),
        "{} is not built: run `cargo build -p fontaine --example fontaine_claude_cli_double`",
        path.display()
    );
    path
}

fn toml_path(path: &std::path::Path) -> String {
    format!("{:?}", path.to_string_lossy())
}

/// A sandbox with the `claude` account on the double, and the CLI profile directory that holds
/// the scenario and the log.
struct Claude {
    s: Sandbox,
    dir: PathBuf,
}

impl Claude {
    fn new(scenario: Value) -> Claude {
        let mut s = Sandbox::new();
        let dir = s.work.parent().unwrap().join("claude-profile");
        std::fs::create_dir_all(&dir).unwrap();
        s.user_config(&format!(
            "[accounts.claude]\nkind = \"claude-subscription\"\ncli_path = {}\nclaude_config_dir = {}\n\n[model]\nmodel = \"claude/sonnet\"\n",
            toml_path(&double()),
            toml_path(&dir)
        ));
        for name in CONFLICTS {
            s.remove_env(name);
        }
        for name in fontaine_core::providers::claude_subscription::CONFLICTING_ENV {
            s.remove_env(name);
        }
        let claude = Claude { s, dir };
        claude.scenario(scenario);
        claude
    }

    fn scenario(&self, scenario: Value) {
        write(&self.dir.join("double.json"), &scenario.to_string());
    }

    fn log(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.join("log.jsonl")).unwrap_or_default();
        // A double still alive (a silent run is killed asynchronously) may be mid-append; an
        // entry without its newline is not complete yet.
        let complete = text.rfind('\n').map_or("", |end| &text[..end]);
        complete
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn model_runs(&self) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|e| {
                e["kind"] == "invocation"
                    && e["argv"].as_array().unwrap().iter().any(|a| a == "--model")
            })
            .collect()
    }

    /// The user and assistant frames each model run received, one list per run.
    fn frames_by_run(&self) -> Vec<Vec<Value>> {
        let mut runs: Vec<Vec<Value>> = Vec::new();
        for entry in self.log() {
            if entry["kind"] == "frame"
                && let Some(run) = entry["run"].as_u64()
            {
                while runs.len() < run as usize {
                    runs.push(Vec::new());
                }
                runs[run as usize - 1].push(entry["value"].clone());
            }
        }
        runs
    }

    async fn headless(&self, args: &[&str]) -> Output {
        self.s.headless(args).await
    }

    /// A headless run in `auto` mode, where the guardian judges the call.
    async fn auto(&self, args: &[&str]) -> Output {
        let mut all = vec!["-p", "--permission-mode", "auto"];
        all.extend_from_slice(args);
        self.s.run(&all).await
    }
}

fn of_type<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|l| l["type"] == kind).collect()
}

fn read_call(path: &str) -> Value {
    json!({"name": "mcp__fontaine__read", "id": "toolu_r1", "input": {"path": path}})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_runs_through_fontaines_gate_and_its_result_reaches_the_next_request() {
    let claude = Claude::new(json!({"responses": [
        {"tool_use": read_call("README.md"), "denial_echo": true, "total_cost_usd": 9.99,
         "usage": {"input_tokens": 100, "output_tokens": 20}},
        {"text": "It is a readme.", "total_cost_usd": 9.99,
         "usage": {"input_tokens": 140, "output_tokens": 9}},
    ]}));
    claude.s.add_file("README.md", "UNIQUE-README-CONTENT\n");
    let out = claude
        .headless(&[
            "--output-format",
            "stream-json",
            "Read README.md and summarize it",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let decision = of_type(&lines, "gate_decision")[0];
    assert_eq!(decision["decision"], "allow", "{decision}");
    let tool_result = of_type(&lines, "tool_result")[0];
    assert_eq!(tool_result["success"], json!(true), "{tool_result}");
    let result = of_type(&lines, "result")[0];
    assert_eq!(result["result"], "It is a readme.", "{result}");

    // Tokens are shown; no cost is, and the CLI's list-price figure is nobody's invoice.
    assert_eq!(result["usage"]["main"]["input_tokens"], 240, "{result}");
    assert_eq!(result["usage"]["main"]["output_tokens"], 29, "{result}");
    assert!(result["total_cost_usd"].is_null(), "{result}");
    assert!(result["usage"]["main"]["cost_usd"].is_null(), "{result}");
    assert!(!out.stdout.contains("9.99"), "{}", out.stdout);

    // The second request carries the durable result, and its history is its own.
    let runs = claude.frames_by_run();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let second = runs[1]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second.contains("UNIQUE-README-CONTENT"), "{second}");
    assert!(second.contains("mcp__fontaine__read"), "{second}");
    assert!(
        !runs[0]
            .iter()
            .any(|f| f.to_string().contains("UNIQUE-README-CONTENT"))
    );
    // No native tools were offered and no suggestion machinery was asked for.
    for run in claude.model_runs() {
        let argv: Vec<&str> = run["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        let at = argv
            .iter()
            .position(|a| *a == "--prompt-suggestions")
            .unwrap();
        assert_eq!(argv[at + 1], "false");
        assert!(!out.stdout.contains("prompt_suggestion"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mid_stream_denial_does_not_fail_the_turn() {
    let claude = Claude::new(json!({"responses": [
        {"tool_use": read_call("README.md"), "denial": "mid_stream"},
        {"text": "It is a readme."},
    ]}));
    claude.s.add_file("README.md", "UNIQUE-README-CONTENT\n");
    let out = claude
        .headless(&["--output-format", "stream-json", "Read README.md"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(of_type(&lines, "gate_decision")[0]["decision"], "allow");
    assert_eq!(of_type(&lines, "result")[0]["result"], "It is a readme.");
    let runs = claude.frames_by_run();
    assert_eq!(runs.len(), 2, "{runs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_thinking_turn_with_an_estimate_answers() {
    let claude = Claude::new(json!({"responses": [
        {"thinking": {"thinking": "hmm", "signature": "sig"}, "thinking_tokens": true,
         "text": "done"},
    ]}));
    let out = claude
        .headless(&["--output-format", "stream-json", "think"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(of_type(&out.lines(), "result")[0]["result"], "done");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_event_is_named_and_leaves_a_trail_and_a_ledger_record() {
    let claude = Claude::new(json!({"responses": [
        {"text": "never shown", "after_stop": [
            {"type": "system", "subtype": "notification", "marker": "UNIQUE-RAW"}]},
    ]}));
    let debug = claude.s.work.join("debug.log");
    let session = "5b0d3c3e-6a39-4c4e-9d47-0c5b6f0f7a11";
    let out = claude
        .headless(&[
            "--debug-file",
            debug.to_str().unwrap(),
            "--session-id",
            session,
            "hello",
        ])
        .await;
    assert_ne!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stderr.contains("system/notification"), "{}", out.stderr);
    assert!(!out.stderr.contains("UNIQUE-RAW"), "{}", out.stderr);

    let log = std::fs::read_to_string(&debug).unwrap();
    assert!(log.contains("UNIQUE-RAW"), "{log}");
    assert!(
        log.contains("stream_event/message_stop → assistant"),
        "{log}"
    );

    let records = ledger(&claude.s, session);
    let failed: Vec<&Value> = records
        .iter()
        .filter(|r| r["kind"] == "request_failed")
        .collect();
    assert_eq!(failed.len(), 1, "{records:?}");
    assert_eq!(failed[0]["agent"], "root", "{}", failed[0]);
    assert!(
        failed[0]["message"]
            .as_str()
            .unwrap()
            .contains("system/notification"),
        "{}",
        failed[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_native_write_never_happens_and_fontaines_gate_decides_the_edit() {
    let claude = Claude::new(json!({"responses": [
        {"tool_use": {"name": "mcp__fontaine__write", "id": "toolu_w", "input":
            {"path": "created.txt", "content": "should not exist"}}, "denial_echo": true},
        {"text": "could not write"},
    ]}));
    let out = claude
        .headless(&["--output-format", "stream-json", "write a file"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    // Manual mode with no one to ask: Fontaine's gate denies, and the CLI executed nothing.
    assert!(!claude.s.work.join("created.txt").exists());
    let lines = out.lines();
    let decision = of_type(&lines, "gate_decision")[0];
    assert_ne!(decision["decision"], "allow", "{decision}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guardian_verdict_on_the_account_gates_a_shell_call_in_auto_mode() {
    let verdict = json!({"decision": "allow", "rule": "allow", "explanation": "ordinary", "confidence": 0.97});
    let claude = Claude::new(json!({"responses": [
        {"tool_use": {"name": "mcp__fontaine__shell", "id": "toolu_s", "input": {"command": "echo guarded"}},
         "denial_echo": true},
        {"text": verdict.to_string()},
        {"text": "done"},
    ]}));
    let out = claude
        .auto(&["--output-format", "stream-json", "run echo"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "tool_result")[0];
    assert_eq!(result["success"], json!(true), "{result}");
    assert!(
        result["text"].as_str().unwrap().contains("guarded"),
        "{result}"
    );
    assert_eq!(claude.model_runs().len(), 3, "main, guardian, main");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guardian_that_cannot_be_read_fails_closed() {
    let claude = Claude::new(json!({"responses": [
        {"tool_use": {"name": "mcp__fontaine__shell", "id": "toolu_s", "input": {"command": "echo never-ran"}},
         "denial_echo": true},
        {"events": ["this is not the protocol"]},
        {"events": ["this is not the protocol"]},
        {"events": ["this is not the protocol"]},
        {"text": "gave up"},
    ]}));
    let out = claude
        .auto(&["--output-format", "stream-json", "run echo"])
        .await;
    let lines = out.lines();
    let results = of_type(&lines, "tool_result");
    assert!(
        results.iter().all(|r| !r["text"]
            .as_str()
            .unwrap_or_default()
            .contains("never-ran\n")),
        "the call ran without a verdict: {results:?}"
    );
    let decision = of_type(&lines, "gate_decision")[0];
    assert_ne!(decision["decision"], "allow", "{decision}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_logged_out_cli_fails_before_any_turn_without_retries() {
    let claude = Claude::new(json!({"auth": {"loggedIn": false}}));
    let out = claude.headless(&["hello"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(out.stderr.contains("claude auth login"), "{}", out.stderr);
    assert!(claude.model_runs().is_empty());
    assert!(
        claude.log().iter().all(|e| e["kind"] != "frame"),
        "no inference"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wrong_plan_and_a_conflicting_variable_are_refused_before_any_turn() {
    let claude = Claude::new(
        json!({"auth": {"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "pro"}}),
    );
    let out = claude.headless(&["hello"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(out.stderr.contains("Max or Ultra"), "{}", out.stderr);
    assert!(claude.log().iter().all(|e| e["kind"] != "frame"));

    let mut claude = Claude::new(json!({}));
    claude.s.set_env("ANTHROPIC_API_KEY", "test-placeholder");
    let out = claude.headless(&["hello"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(out.stderr.contains("ANTHROPIC_API_KEY"), "{}", out.stderr);
    assert!(!out.stderr.contains("test-placeholder"), "{}", out.stderr);
    assert!(claude.log().is_empty(), "no process before the refusal");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credit_billed_model_is_refused() {
    let claude = Claude::new(json!({"models": [
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "usesUsageCredits": true},
    ]}));
    let out = claude.headless(&["hello"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("usage credits"), "{}", out.stderr);
    assert!(claude.model_runs().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cost_budget_on_the_subscription_is_a_usage_error() {
    let claude = Claude::new(json!({}));
    let out = claude.headless(&["--max-budget-usd", "1", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("--max-budget-usd"), "{}", out.stderr);
    assert!(claude.model_runs().is_empty(), "no inference request");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_limit_is_not_retried_and_a_silent_process_is_retried_twice() {
    let claude = Claude::new(json!({"responses": [{"events": [
        {"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "tools": [],
         "mcp_servers": [{"name": "fontaine", "status": "connected"}], "plugins": [], "skills": []},
        {"type": "result", "subtype": "error_during_execution", "is_error": true,
         "api_error_status": 429, "result": "RAW-LIMIT-TEXT"},
    ]}]}));
    let out = claude.headless(&["hello"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert_eq!(claude.model_runs().len(), 1, "a limit earns no retry");
    assert!(!format!("{}{}", out.stdout, out.stderr).contains("RAW-LIMIT-TEXT"));

    let mut claude = Claude::new(json!({"responses": [{"hang_before_output": true}]}));
    claude.s.set_env("FONTAINE_CLAUDE_READ_IDLE_MS", "300");
    let out = claude.headless(&["hello"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert_eq!(
        claude.model_runs().len(),
        3,
        "the first attempt and two retries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_replays_its_history_to_the_cli() {
    let claude =
        Claude::new(json!({"responses": [{"text": "first answer"}, {"text": "second answer"}]}));
    let id = "6f1d2c3b-4a5e-4f60-8a7b-9c0d1e2f3a4b";
    let out = claude
        .headless(&["--session-id", id, "remember the number 42"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let out = claude.headless(&["--resume", id, "what number?"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "second answer\n");
    let runs = claude.frames_by_run();
    let second = runs[1]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second.contains("remember the number 42"), "{second}");
    assert!(second.contains("first answer"), "{second}");
    assert!(second.contains("what number?"), "{second}");
    assert!(
        !runs[0]
            .iter()
            .any(|f| f.to_string().contains("what number?"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_inventory_mode_describes_tools_and_denies_every_call() {
    let manifest = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        manifest.path(),
        json!([{"name": "read_file", "description": "d", "inputSchema": {"type": "object"}}])
            .to_string(),
    )
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_fontaine"))
        .arg("--internal-claude-inventory")
        .arg(manifest.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    async fn send(stdin: &mut tokio::process::ChildStdin, line: &str) {
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }
    async fn reply(stdout: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Value {
        serde_json::from_str(&stdout.next_line().await.unwrap().unwrap()).unwrap()
    }
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                            "params": {"protocolVersion": "2025-03-26"}});
    send(&mut stdin, &initialize.to_string()).await;
    assert_eq!(
        reply(&mut stdout).await["result"]["serverInfo"]["name"],
        "fontaine"
    );
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
    )
    .await;
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(),
    )
    .await;
    let listed = reply(&mut stdout).await;
    assert_eq!(listed["result"]["tools"][0]["name"], "read_file");
    assert_eq!(
        listed["result"]["tools"][0]["_meta"]["anthropic/requiresUserInteraction"],
        true
    );
    for call in [
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "read_file", "arguments": {"path": "x"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "unknown"}}),
    ] {
        send(&mut stdin, &call.to_string()).await;
        let denied = reply(&mut stdout).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }
    send(&mut stdin, "{broken").await;
    assert_eq!(reply(&mut stdout).await["error"]["code"], -32700);
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}).to_string(),
    )
    .await;
    assert_eq!(reply(&mut stdout).await["error"]["code"], -32601);
    drop(stdin);
    assert!(child.wait().await.unwrap().success());
}
