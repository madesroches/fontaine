//! `fontaine -p "/goal ..."`: the loop to a verdict in one invocation, the `goal` events, the
//! result's `goal` object, and the exit code.

mod common;

use std::path::Path;

use common::{Endpoint, Output, Sandbox, Turn, Wire};
use serde_json::{Value, json};

fn validator() -> jsonschema::Validator {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/headless-v1.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_all_valid(out: &Output) {
    let validator = validator();
    for line in out.lines() {
        let errors: Vec<String> = validator
            .iter_errors(&line)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{line}\n{}", errors.join("\n"));
    }
}

fn judged(verdict: &str, reason: &str) -> Turn {
    Turn::tool(
        "g1",
        "submit",
        json!({"verdict": verdict, "reason": reason}),
    )
}

fn stream(prompt: &str) -> Vec<String> {
    ["--output-format", "stream-json", prompt]
        .map(String::from)
        .to_vec()
}

async fn headless(s: &Sandbox, args: &[String]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    s.headless(&args).await
}

fn goal_events(out: &Output) -> Vec<Value> {
    out.lines()
        .into_iter()
        .filter(|l| l["type"] == "goal")
        .collect()
}

fn result_of(out: &Output) -> Value {
    out.lines()
        .into_iter()
        .rev()
        .find(|l| l["type"] == "result")
        .unwrap()
}

fn with_goal_config(s: &Sandbox, url: &str, goal: &str) {
    s.point_at(url);
    let base = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!("{base}\n[goal]\n{goal}\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_runs_turns_until_the_evaluator_says_met() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::text("first pass"),
            judged("not_met", "the file is missing"),
            Turn::text("second pass"),
            judged("met", "the file exists"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = headless(&s, &stream("/goal the file exists")).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_all_valid(&out);

    let goals = goal_events(&out);
    assert_eq!(goals.len(), 2, "{goals:?}");
    assert_eq!(goals[0]["verdict"], "not_met");
    assert_eq!(goals[0]["reason"], "the file is missing");
    assert_eq!(goals[0]["evaluations"], 1);
    assert_eq!(goals[1]["verdict"], "met");
    assert_eq!(goals[1]["evaluations"], 2);

    let result = result_of(&out);
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["result"], "second pass");
    assert_eq!(result["num_turns"], 2);
    assert_eq!(result["goal"]["status"], "met");
    assert_eq!(result["goal"]["condition"], "the file exists");
    assert_eq!(result["goal"]["evaluations"], 2);
    assert_eq!(result["goal"]["reason"], "the file exists");
    assert_eq!(result["usage"]["goal"]["input_tokens"], 2000);
    assert_eq!(result["usage"]["goal"]["output_tokens"], 1000);
    assert_eq!(result["usage"]["main"]["input_tokens"], 2000);

    let sent = endpoint.requests().await;
    assert_eq!(sent.len(), 4);
    assert!(
        sent[0]
            .to_string()
            .contains("The user set a goal; an evaluator will check it"),
        "{}",
        sent[0]
    );
    assert!(
        sent[2]
            .to_string()
            .contains("Harness note, not the user's words")
    );
    assert!(sent[2].to_string().contains("the file is missing"));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_typed_goal_line_is_a_user_message() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::text("one"),
            judged("not_met", "more"),
            Turn::text("two"),
            judged("met", "ok"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["/goal the build is green"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout.trim(), "two");
    let sessions = s.data.join("sessions");
    let dir = std::fs::read_dir(&sessions)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let ledger = std::fs::read_to_string(dir.join("ledger/ledger.jsonl")).unwrap();
    let said: Vec<&str> = ledger
        .lines()
        .filter(|l| l.contains("\"kind\":\"user_message\""))
        .collect();
    assert_eq!(said.len(), 1, "{ledger}");
    assert!(said[0].contains("/goal the build is green"));
    assert!(!dir.join("goal.json").exists(), "a met goal leaves no file");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_impossible_goal_exits_1_and_says_why() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::text("tried"),
            judged("impossible", "the mount is read-only"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = headless(&s, &stream("/goal write to the read-only mount")).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert_all_valid(&out);
    let result = result_of(&out);
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["goal"]["status"], "impossible");
    assert_eq!(result["goal"]["reason"], "the mount is read-only");

    let again = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::text("tried"),
            judged("impossible", "the mount is read-only"),
        ],
    )
    .await;
    s.point_at(&again.url());
    let text = s.headless(&["/goal write to the read-only mount"]).await;
    assert_eq!(text.code, Some(1));
    assert!(text.stdout.is_empty(), "{}", text.stdout);
    assert!(
        text.stderr
            .contains("fontaine: goal impossible: the mount is read-only"),
        "{}",
        text.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_cap_pauses_the_goal_and_exits_1() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::text("pass"),
            judged("not_met", "still not there"),
            Turn::text("pass"),
            judged("not_met", "still not there"),
        ],
    )
    .await;
    let s = Sandbox::new();
    with_goal_config(&s, &endpoint.url(), "max_turns = 1");
    let out = headless(&s, &stream("/goal be done")).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert_all_valid(&out);
    assert_eq!(goal_events(&out).len(), 2);
    let result = result_of(&out);
    assert_eq!(result["goal"]["status"], "paused");
    assert!(
        result["goal"]["reason"]
            .as_str()
            .unwrap()
            .contains("1 goal turn ran"),
        "{result}"
    );
    assert_eq!(
        endpoint.requests().await.len(),
        4,
        "no turn after the pause"
    );
    let dir = std::fs::read_dir(s.data.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(dir.join("goal.json").is_file(), "a paused goal stays set");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_evaluator_that_gives_no_verdict_pauses_the_goal() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("worked"), Turn::text("I think so")],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = headless(&s, &stream("/goal be done")).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert_all_valid(&out);
    let goals = goal_events(&out);
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0]["verdict"], "failed");
    let result = result_of(&out);
    assert_eq!(result["goal"]["status"], "paused");
    assert!(
        result["goal"]["reason"]
            .as_str()
            .unwrap()
            .contains("the evaluator gave no verdict"),
        "{result}"
    );
    // The spend of the failed attempts is still counted.
    assert!(result["usage"]["goal"]["input_tokens"].as_u64().unwrap() >= 1000);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_and_clear_make_no_model_call() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("unused")]).await;
    let s = Sandbox::compatible(&endpoint.url());

    let out = headless(&s, &stream("/goal")).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_all_valid(&out);
    let result = result_of(&out);
    assert_eq!(result["result"], "No goal set");
    assert_eq!(result["num_turns"], 0);
    assert!(result.get("goal").is_none());

    let out = headless(&s, &stream("/goal clear")).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(result_of(&out)["result"], "No goal set");

    for alias in ["stop", "off", "reset", "none", "cancel"] {
        let out = s.headless(&[&format!("/goal {alias}")]).await;
        assert_eq!(out.code, Some(0), "{alias}: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "No goal set");
    }
    assert!(endpoint.requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_goal_is_resumed_and_evaluated_by_the_next_run_of_the_session() {
    let first = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("worked"), Turn::text("not a verdict")],
    )
    .await;
    let s = Sandbox::compatible(&first.url());
    let out = headless(&s, &stream("/goal the file exists")).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);

    let second = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("hello back"), judged("met", "the file exists")],
    )
    .await;
    s.point_at(&second.url());
    let out = headless(
        &s,
        &["--output-format", "stream-json", "--continue", "carry on"].map(String::from),
    )
    .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_all_valid(&out);
    let result = result_of(&out);
    assert_eq!(result["goal"]["status"], "met");
    assert_eq!(result["goal"]["condition"], "the file exists");
    assert_eq!(
        result["goal"]["evaluations"], 1,
        "counters start again on resume"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_goal_followed_by_a_space_or_the_end_is_the_command() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("an answer")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["/goalpost"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout.trim(), "an answer");
    assert_eq!(endpoint.requests().await.len(), 1);
}
