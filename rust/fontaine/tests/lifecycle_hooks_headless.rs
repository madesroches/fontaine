//! The lifecycle hooks through `fontaine -p`: what only a whole run shows. A prompt a hook blocks
//! never reaches the model, the context hooks add reaches it without becoming the user's words,
//! a `stop` hook that blocks continues the run, and `session_start` and `session_end` fire with
//! the right source and reason.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire, ledger};
use fontaine_testkit::example_bin;
use serde_json::{Value, json};

/// Adds `[[hooks.<event>]]` to the sandbox's user config: `hook_marker` run with `args`.
fn hook(s: &Sandbox, event: &str, extra: &str, args: &str) {
    let config = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!(
        "{config}\n[[hooks.{event}]]\n{extra}command = '''\n'{}' {args}\n'''\n",
        example_bin("fontaine", "hook_marker")
            .to_string_lossy()
            .into_owned()
    ));
}

fn quoted(path: &std::path::Path) -> String {
    format!("'{}'", path.display())
}

fn recorded(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn user_messages(s: &Sandbox, session: &str) -> Vec<String> {
    ledger(s, session)
        .iter()
        .filter(|r| r["kind"] == "user_message")
        .map(|r| r["text"].as_str().unwrap().to_string())
        .collect()
}

fn session_of(out: &common::Output) -> String {
    out.lines()[0]["session_id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_prompt_never_reaches_the_model_and_the_run_succeeds() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("unused")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let seen = s.work.join("seen.jsonl");
    hook(
        &s,
        "user_prompt_submit",
        "",
        &format!(
            "--append --exit 2 --stderr 'no prompts today' {}",
            quoted(&seen)
        ),
    );
    let out = s.headless(&["--output-format", "json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let result = &out.lines()[0];
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["num_turns"], 0);
    let text = result["result"].as_str().unwrap();
    assert!(
        text.starts_with("UserPromptSubmit operation blocked by hook:\n["),
        "{text}"
    );
    assert!(text.contains("]: no prompts today"), "{text}");
    assert!(text.ends_with("Original prompt: hi"), "{text}");
    assert!(endpoint.requests().await.is_empty());
    let event = &recorded(&seen)[0];
    assert_eq!(event["hook_event_name"], "UserPromptSubmit");
    assert_eq!(event["prompt"], "hi");
    let session = result["session_id"]
        .as_str()
        .map_or_else(String::new, str::to_string);
    if !session.is_empty() {
        assert!(user_messages(&s, &session).is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_context_reaches_the_model_but_is_never_a_user_message() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let marker = s.work.join("m.json");
    let start = json!({"hookSpecificOutput": {
        "hookEventName": "SessionStart", "additionalContext": "the repo is Rust"}})
    .to_string();
    hook(
        &s,
        "session_start",
        "",
        &format!("{} '{start}'", quoted(&marker)),
    );
    let submit = json!({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit", "additionalContext": "use tabs"}})
    .to_string();
    hook(
        &s,
        "user_prompt_submit",
        "",
        &format!("{} '{submit}'", quoted(&s.work.join("p.json"))),
    );
    let out = s
        .headless(&["--output-format", "stream-json", "hello"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let body = endpoint.requests().await[0].to_string();
    assert!(
        body.contains("[Hook context from session_start, not the user's words: the repo is Rust]"),
        "{body}"
    );
    assert!(
        body.contains("[Hook context from user_prompt_submit, not the user's words: use tabs]"),
        "{body}"
    );
    assert_eq!(user_messages(&s, &session_of(&out)), ["hello"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn session_start_names_its_source_and_session_end_fires_at_the_end() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("one"), Turn::text("two"), Turn::text("three")],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let starts = s.work.join("starts.jsonl");
    let ends = s.work.join("ends.jsonl");
    hook(
        &s,
        "session_start",
        "",
        &format!("--append {}", quoted(&starts)),
    );
    hook(
        &s,
        "session_end",
        "",
        &format!("--append {}", quoted(&ends)),
    );
    let first = s.headless(&["first"]).await;
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let second = s.headless(&["--continue", "second"]).await;
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    let third = s.headless(&["--continue", "--fork-session", "third"]).await;
    assert_eq!(third.code, Some(0), "{}", third.stderr);
    let sources: Vec<String> = recorded(&starts)
        .iter()
        .map(|e| e["source"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(sources, ["startup", "resume", "fork"]);
    let reasons: Vec<String> = recorded(&ends)
        .iter()
        .map(|e| e["reason"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(reasons, ["other", "other", "other"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocking_stop_hook_continues_the_run_with_its_reason() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("first answer"), Turn::text("final answer")],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let seen = s.work.join("stops.jsonl");
    hook(
        &s,
        "stop",
        "",
        &format!(
            "--append --block-first 1 --stderr 'run the tests first' {}",
            quoted(&seen)
        ),
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(lines.last().unwrap()["result"], "final answer");
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .to_string()
            .contains("[Hook context from stop, not the user's words: run the tests first]"),
        "{}",
        requests[1]
    );
    let events = recorded(&seen);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["stop_hook_active"], false);
    assert_eq!(events[0]["last_assistant_message"], "first answer");
    assert_eq!(events[1]["stop_hook_active"], true);
    // The continuation is a harness note: the user said one thing.
    assert_eq!(user_messages(&s, &session_of(&out)), ["go"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_hook_that_always_blocks_is_let_go_after_eight() {
    let turns: Vec<Turn> = (0..12)
        .map(|n| Turn::text(&format!("answer {n}")))
        .collect();
    let endpoint = Endpoint::start(Wire::Chat, turns).await;
    let s = Sandbox::compatible(&endpoint.url());
    let seen = s.work.join("stops.jsonl");
    hook(
        &s,
        "stop",
        "",
        &format!("--append --exit 2 --stderr 'keep going' {}", quoted(&seen)),
    );
    let out = s.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(endpoint.requests().await.len(), 9);
    assert_eq!(recorded(&seen).len(), 8);
    assert!(
        out.stderr.contains("8 times in a row"),
        "the cap warns: {}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_hook_answering_allow_changes_nothing() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "shell", json!({"command": "cargo test"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let allow = json!({"hookSpecificOutput": {
        "hookEventName": "Stop", "permissionDecision": "allow"}})
    .to_string();
    hook(
        &s,
        "stop",
        "",
        &format!("{} '{allow}'", quoted(&s.work.join("a"))),
    );
    hook(
        &s,
        "user_prompt_submit",
        "",
        &format!("{} '{allow}'", quoted(&s.work.join("b"))),
    );
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let decision = out
        .lines()
        .into_iter()
        .find(|l| l["type"] == "gate_decision")
        .unwrap();
    assert_eq!(decision["decision"], "deny", "{decision}");
    assert_eq!(decision["tier"], "mode");
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_failure_fires_with_the_error_kind() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![Turn::Fail(500), Turn::Fail(500), Turn::Fail(500)],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let seen = s.work.join("failures.jsonl");
    hook(
        &s,
        "stop_failure",
        "matcher = \"server_error\"\n",
        &format!("--append {}", quoted(&seen)),
    );
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let events = recorded(&seen);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["hook_event_name"], "StopFailure");
    assert_eq!(events[0]["error"], "server_error");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_stop_hook_is_ignored_with_a_warning() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let seen = s.work.join("stops.jsonl");
    s.add_file(
        ".fontaine/settings.toml",
        &format!(
            "[[hooks.stop]]\ncommand = '''\n'{}' --append --exit 2 '{}'\n'''\n",
            example_bin("fontaine", "hook_marker")
                .to_string_lossy()
                .into_owned(),
            seen.display()
        ),
    );
    let out = s.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!seen.exists());
    assert_eq!(endpoint.requests().await.len(), 1);
    assert!(
        out.stderr
            .contains("stop hook(s) from the repository are ignored"),
        "{}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_tool_use_failure_hook_adds_context_to_the_error() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "missing.txt"})),
            Turn::text("noted"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let ok = s.work.join("ok.jsonl");
    let failed = s.work.join("failed.jsonl");
    hook(
        &s,
        "post_tool_use",
        "",
        &format!("--append {}", quoted(&ok)),
    );
    let reply = json!({"hookSpecificOutput": {
        "hookEventName": "PostToolUseFailure",
        "additionalContext": "the file may be generated"}})
    .to_string();
    hook(
        &s,
        "post_tool_use_failure",
        "",
        &format!("--append {} '{reply}'", quoted(&failed)),
    );
    let out = s
        .headless(&["--output-format", "stream-json", "read it"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!ok.exists(), "post_tool_use does not fire on an error");
    assert_eq!(recorded(&failed).len(), 1);
    let body = endpoint.requests().await[1].to_string();
    assert!(body.contains("the file may be generated"), "{body}");
}
