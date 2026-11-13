//! Model roles through `fontaine -p`: a list falling through to its next entry on a quota
//! failure, for the main conversation and for subagents, and what happens when the list is spent.

mod common;

use common::{Endpoint, Route, Sandbox, Turn, Wire, ledger};
use serde_json::{Value, json};

fn accounts(quota: &Endpoint, ok: &Endpoint) -> String {
    format!(
        "[accounts.quota]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"q1\"]\n\n[accounts.ok]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"o1\"]\n\n",
        quota.url(),
        ok.url()
    )
}

fn two_messages() -> String {
    ["one", "two"]
        .iter()
        .map(|m| {
            format!(
                "{}\n",
                json!({"type": "user", "message": {"role": "user", "content": m}})
            )
        })
        .collect()
}

async fn run_two(s: &Sandbox) -> common::Output {
    s.run_with_stdin(
        &[
            "-p",
            "--permission-mode",
            "manual",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ],
        &two_messages(),
    )
    .await
}

fn results(lines: &[Value]) -> Vec<&Value> {
    lines.iter().filter(|l| l["type"] == "result").collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_main_list_falls_through_on_a_quota_failure_and_the_next_message_starts_on_the_entry_that_worked()
 {
    let quota = Endpoint::start(Wire::Chat, vec![Turn::Fail(429)]).await;
    let ok = Endpoint::start(Wire::Chat, vec![Turn::text("served by the second")]).await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"quota/q1\", \"ok/o1\"]\n",
        accounts(&quota, &ok)
    ));
    let out = run_two(&s).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let results = results(&lines);
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|r| r["subtype"] == "success"),
        "{results:?}"
    );

    // The first message tried the first entry through its same-model retries and moved on; the
    // second went straight to the settled entry.
    assert_eq!(quota.requests().await.len(), 3);
    assert_eq!(ok.requests().await.len(), 2);
    assert_eq!(
        out.stderr.matches("out of quota").count(),
        1,
        "warned once: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("main: quota/q1 is out of quota"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("continuing on ok/o1"), "{}", out.stderr);

    let session = lines[0]["session_id"].as_str().unwrap();
    let fell: Vec<Value> = ledger(&s, session)
        .into_iter()
        .filter(|r| r["kind"] == "model_fell_through")
        .collect();
    assert_eq!(fell.len(), 1, "{fell:?}");
    assert_eq!(fell[0]["role"], "main");
    assert_eq!(fell[0]["from"], "quota/q1");
    assert_eq!(fell[0]["to"], "ok/o1");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spent_list_fails_the_turn_naming_the_role() {
    let first = Endpoint::start(Wire::Chat, vec![Turn::Fail(429)]).await;
    let second = Endpoint::start(Wire::Chat, vec![Turn::Fail(429)]).await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"quota/q1\", \"ok/o1\"]\n",
        accounts(&first, &second)
    ));
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let result = results(&lines)[0].clone();
    assert_eq!(result["subtype"], "error_during_execution", "{result}");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|text| text.contains("every model in main failed")),
        "{result}"
    );
    assert_eq!(first.requests().await.len(), 3);
    assert_eq!(second.requests().await.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn subagents_on_a_role_share_the_entry_that_worked() {
    let quota = Endpoint::start(Wire::Chat, vec![Turn::Fail(429)]).await;
    let ok = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new(
                "root task",
                vec![
                    Turn::tool(
                        "c1",
                        "agent",
                        json!({"agent_type": "explore", "prompt": "child-A: look", "model": "coding"}),
                    ),
                    Turn::tool("c2", "job_output", json!({"id": "agent-1"})),
                    Turn::tool(
                        "c3",
                        "agent",
                        json!({"agent_type": "explore", "prompt": "child-B: look", "model": "coding"}),
                    ),
                    Turn::tool("c4", "job_output", json!({"id": "agent-2"})),
                    Turn::text("root finished"),
                ],
            ),
            Route::new("child-A", vec![Turn::text("A answered")]),
            Route::new("child-B", vec![Turn::text("B answered")]),
        ],
    )
    .await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\", \"ok/o1\"]\n",
        accounts(&quota, &ok)
    ));
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "agent",
            "root task",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let text: String = lines
        .iter()
        .filter(|l| l["type"] == "tool_result")
        .filter_map(|r| r["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("A answered") && text.contains("B answered"),
        "{text}"
    );
    // Only the first child paid for the failed entry: the second started on the settled one.
    assert_eq!(quota.requests().await.len(), 3);
    assert_eq!(
        out.stderr.matches("out of quota").count(),
        1,
        "{}",
        out.stderr
    );
    let session = lines[0]["session_id"].as_str().unwrap();
    let fell: Vec<Value> = ledger(&s, session)
        .into_iter()
        .filter(|r| r["kind"] == "model_fell_through")
        .collect();
    assert_eq!(fell.len(), 1, "{fell:?}");
    assert_eq!(fell[0]["role"], "coding");
    assert_eq!(fell[0]["agent"], "agent-1");
}

const WINDOW: &str = r#"{"error":{"message":"You've reached your 5-hour usage limit.","type":"exceeded_current_quota_error"}}"#;

fn spent_window(times: usize) -> Vec<Turn> {
    (0..times)
        .map(|_| Turn::Reply(429, WINDOW.to_string()))
        .collect()
}

/// The `quota` account on a spent usage window and `mid` as given, beside a working `ok`.
fn window_accounts(quota: &Endpoint, mid: &Endpoint, ok: &Endpoint) -> String {
    format!(
        "{}[accounts.mid]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"m1\"]\n\n",
        accounts(quota, ok),
        mid.url()
    )
}

async fn ok_endpoint_with_one_child() -> Endpoint {
    Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new(
                "root task",
                vec![
                    Turn::tool(
                        "c1",
                        "agent",
                        json!({"agent_type": "explore", "prompt": "child-A: look", "model": "coding"}),
                    ),
                    Turn::tool("c2", "job_output", json!({"id": "agent-1"})),
                    Turn::text("root finished"),
                ],
            ),
            Route::new("child-A", vec![Turn::text("A answered")]),
        ],
    )
    .await
}

fn tool_results(lines: &[Value]) -> String {
    lines
        .iter()
        .filter(|l| l["type"] == "tool_result")
        .filter_map(|r| r["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_with_child(s: &Sandbox, extra: &[&str]) -> common::Output {
    let mut args = vec!["--output-format", "stream-json", "--allowed-tools", "agent"];
    args.extend_from_slice(extra);
    args.push("root task");
    s.headless(&args).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_on_a_spent_window_continues_on_the_fallback_role_without_pausing() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, vec![]).await;
    let ok = ok_endpoint_with_one_child().await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\"]\nfallback = [\"ok/o1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(tool_results(&lines).contains("A answered"));
    assert!(lines.iter().all(|l| l["type"] != "usage_paused"));
    assert_eq!(quota.requests().await.len(), 1);
    assert!(
        out.stderr
            .contains("agent-1: quota/q1 usage window spent; continuing on ok/o1"),
        "{}",
        out.stderr
    );
    let session = lines[0]["session_id"].as_str().unwrap();
    let fell: Vec<Value> = ledger(&s, session)
        .into_iter()
        .filter(|r| r["kind"] == "model_fell_through")
        .collect();
    assert_eq!(fell.len(), 1, "{fell:?}");
    assert_eq!(fell[0]["role"], "fallback");
    assert_eq!(fell[0]["agent"], "agent-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_on_a_spent_window_pauses_the_session_when_no_fallback_is_defined() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, vec![]).await;
    let ok = ok_endpoint_with_one_child().await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    let lines = out.lines();
    let paused = lines
        .iter()
        .find(|l| l["type"] == "usage_paused")
        .unwrap_or_else(|| panic!("{lines:?}\n{}", out.stderr));
    assert_eq!(paused["account"], "quota");
    assert!(!out.stderr.contains("continuing on"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fallback_on_the_spent_account_is_skipped_and_the_session_pauses() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, vec![]).await;
    let ok = ok_endpoint_with_one_child().await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\"]\nfallback = [\"quota/q1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    let lines = out.lines();
    assert!(
        lines.iter().any(|l| l["type"] == "usage_paused"),
        "{lines:?}\n{}",
        out.stderr
    );
    assert!(!out.stderr.contains("continuing on"), "{}", out.stderr);
    assert_eq!(
        quota.requests().await.len(),
        1,
        "no request to the spent entry again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fallback_that_is_spent_too_pauses_on_its_own_account() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let ok = ok_endpoint_with_one_child().await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\"]\nfallback = [\"mid/m1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    let lines = out.lines();
    let paused = lines
        .iter()
        .find(|l| l["type"] == "usage_paused")
        .unwrap_or_else(|| panic!("{lines:?}\n{}", out.stderr));
    assert_eq!(paused["account"], "mid");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_window_failure_on_the_fallback_walks_its_list_and_never_back_to_a_spent_entry() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let ok = ok_endpoint_with_one_child().await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\"]\nfallback = [\"mid/m1\", \"quota/q1\", \"ok/o1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(tool_results(&lines).contains("A answered"));
    assert!(lines.iter().all(|l| l["type"] != "usage_paused"));
    assert_eq!(quota.requests().await.len(), 1);
    assert_eq!(mid.requests().await.len(), 1);
    let session = lines[0]["session_id"].as_str().unwrap();
    let fell: Vec<Value> = ledger(&s, session)
        .into_iter()
        .filter(|r| r["kind"] == "model_fell_through")
        .collect();
    assert_eq!(fell.len(), 2, "{fell:?}");
    assert!(fell.iter().all(|r| r["role"] == "fallback"), "{fell:?}");
    assert_eq!(fell[1]["to"], "ok/o1", "{fell:?}");
}

fn spawn_child(call: &str, id: &str, prompt: &str, model: &str) -> Vec<Turn> {
    vec![
        Turn::tool(
            call,
            "agent",
            json!({"agent_type": "explore", "prompt": prompt, "model": model}),
        ),
        Turn::tool(&format!("{call}o"), "job_output", json!({"id": id})),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fallback_move_leaves_the_roles_other_subagents_on_their_own_entry() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, vec![]).await;
    let mut root = spawn_child("c1", "agent-1", "child-A: look", "coding");
    root.extend(spawn_child("c3", "agent-2", "child-B: look", "coding"));
    root.push(Turn::text("root finished"));
    let ok = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new("root task", root),
            Route::new("child-A", vec![Turn::text("A answered")]),
            Route::new("child-B", vec![Turn::text("B answered")]),
        ],
    )
    .await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\ncoding = [\"quota/q1\"]\nfallback = [\"ok/o1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let text = tool_results(&out.lines());
    assert!(
        text.contains("A answered") && text.contains("B answered"),
        "{text}"
    );
    assert_eq!(
        quota.requests().await.len(),
        2,
        "the second child still started on the role's own entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_on_a_direct_model_falls_back_and_skips_its_own_spent_account() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, vec![]).await;
    let mut root = spawn_child("c1", "agent-1", "child-A: look", "quota/q1");
    root.push(Turn::text("root finished"));
    let ok = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new("root task", root),
            Route::new("child-A", vec![Turn::text("A answered")]),
        ],
    )
    .await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\nfallback = [\"quota/q1\", \"ok/o1\"]\n",
        window_accounts(&quota, &mid, &ok)
    ));
    let out = run_with_child(&s, &["--max-usage-wait", "1s"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(tool_results(&lines).contains("A answered"));
    assert!(lines.iter().all(|l| l["type"] != "usage_paused"));
    assert_eq!(quota.requests().await.len(), 1);
    let session = lines[0]["session_id"].as_str().unwrap();
    let fell: Vec<Value> = ledger(&s, session)
        .into_iter()
        .filter(|r| r["kind"] == "model_fell_through")
        .collect();
    assert_eq!(fell.len(), 1, "{fell:?}");
    assert_eq!(fell[0]["to"], "ok/o1");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unpriced_fallback_entry_is_skipped_under_a_cost_cap() {
    let quota = Endpoint::start(Wire::Chat, spent_window(5)).await;
    let mid = Endpoint::start(Wire::Chat, vec![]).await;
    let mut root = spawn_child("c1", "agent-1", "child-A: look", "coding");
    root.push(Turn::text("root finished"));
    let paid = Endpoint::start_routed(
        Wire::Responses,
        vec![
            Route::new("root task", root),
            Route::new("child-A", vec![Turn::text("A answered")]),
        ],
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.quota]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"q1\"]\n\n[accounts.mid]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"m1\"]\n\n[accounts.paid]\nkind = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"PAID_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[models.roles]\nmain = [\"paid/gpt-4o-mini\"]\ncoding = [\"quota/q1\"]\nfallback = [\"mid/m1\", \"paid/gpt-4o-mini\"]\n",
        quota.url(),
        mid.url(),
        paid.url()
    ));
    s.set_env("PAID_KEY", "sk-test-not-a-real-key");
    let out = run_with_child(&s, &["--max-budget-usd", "1", "--max-usage-wait", "1s"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(tool_results(&lines).contains("A answered"));
    assert_eq!(
        mid.requests().await.len(),
        0,
        "the unpriced entry is passed over"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_type_naming_an_undefined_role_is_a_config_error() {
    let quota = Endpoint::start(Wire::Chat, vec![]).await;
    let ok = Endpoint::start(Wire::Chat, vec![]).await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "{}[models.roles]\nmain = [\"ok/o1\"]\n\n[agents.types.lead]\nmodel = \"nosuchrole\"\n",
        accounts(&quota, &ok)
    ));
    let out = s.run(&["-p", "hello"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("nosuchrole"), "{}", out.stderr);
}
