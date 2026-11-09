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
