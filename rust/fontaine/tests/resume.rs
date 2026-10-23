//! `--continue`, `--resume` and `--fork-session` through `fontaine -p`: the conversation and the
//! ledger come back, and the flags that make no sense together are usage errors.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::Value;

async fn setup() -> (Endpoint, Sandbox) {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::text("first answer"),
            Turn::text("second answer"),
            Turn::text("third answer"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    (endpoint, s)
}

async fn run(s: &Sandbox, extra: &[&str], prompt: &str) -> (common::Output, String) {
    let mut args = vec!["--output-format", "stream-json"];
    args.extend_from_slice(extra);
    args.push(prompt);
    let out = s.headless(&args).await;
    let id = out
        .lines()
        .first()
        .and_then(|l| l["session_id"].as_str())
        .unwrap_or_default()
        .to_string();
    (out, id)
}

fn ledger(s: &Sandbox, id: &str) -> Vec<Value> {
    std::fs::read_to_string(s.data.join("sessions").join(id).join("ledger/ledger.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn kinds(entries: &[Value], kind: &str) -> usize {
    entries.iter().filter(|e| e["kind"] == kind).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_reopens_the_latest_session_of_this_directory_with_its_conversation() {
    let (endpoint, s) = setup().await;
    let (first, id) = run(&s, &[], "one").await;
    assert_eq!(first.code, Some(0), "{}", first.stderr);

    let (second, again) = run(&s, &["--continue"], "two").await;
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    assert_eq!(again, id, "the same session");
    let requests = endpoint.requests().await;
    let body = requests[1].to_string();
    assert!(
        body.contains("one") && body.contains("first answer") && body.contains("two"),
        "{body}"
    );

    let entries = ledger(&s, &id);
    assert_eq!(kinds(&entries, "user_message"), 2);
    assert_eq!(kinds(&entries, "session_start"), 2);
    let resumed = entries.iter().find(|e| e["kind"] == "resumed").unwrap();
    assert_eq!(resumed["source"], id);
    assert_eq!(resumed["how"], "continue");
    // The audit log continues in the same file.
    assert!(s.data.join("audit").join(format!("{id}.jsonl")).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_reopens_a_session_by_id() {
    let (endpoint, s) = setup().await;
    let (_, id) = run(&s, &[], "one").await;
    let (_, other) = run(&s, &[], "unrelated").await;
    assert_ne!(id, other);
    let (out, resumed) = run(&s, &["--resume", &id], "two").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(resumed, id);
    let body = endpoint.requests().await[2].to_string();
    assert!(
        body.contains("one") && !body.contains("unrelated"),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_picks_the_most_recently_updated_session_of_the_launch_directory_only() {
    let (_endpoint, s) = setup().await;
    let elsewhere = s.work.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (_, here) = run(&s, &[], "here").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    // A newer session, started from another directory.
    let there = s
        .run_in(
            &elsewhere,
            &[
                "-p",
                "--permission-mode",
                "manual",
                "--output-format",
                "stream-json",
                "there",
            ],
        )
        .await;
    let there_id = there.lines()[0]["session_id"].as_str().unwrap().to_string();
    assert_ne!(there_id, here);

    let (_, continued) = run(&s, &["--continue"], "more").await;
    assert_eq!(
        continued, here,
        "the newer session belongs to another directory"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_with_nothing_to_continue_starts_fresh() {
    let (_endpoint, s) = setup().await;
    let (out, id) = run(&s, &["--continue"], "one").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let entries = ledger(&s, &id);
    assert_eq!(kinds(&entries, "resumed"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_session_copies_both_stores_under_a_new_id_and_leaves_the_original_alone() {
    let (endpoint, s) = setup().await;
    let (_, id) = run(&s, &[], "one").await;
    let before = ledger(&s, &id).len();

    let new_id = "11111111-2222-4333-8444-555555555555";
    let (out, forked) = run(
        &s,
        &["--resume", &id, "--fork-session", "--session-id", new_id],
        "two",
    )
    .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(forked, new_id);
    let body = endpoint.requests().await[1].to_string();
    assert!(body.contains("first answer"), "{body}");

    // The original is exactly as it was; the copy carries its history and its own start.
    assert_eq!(ledger(&s, &id).len(), before);
    let copy = ledger(&s, new_id);
    assert!(copy.len() > before);
    assert_eq!(
        copy.iter().find(|e| e["kind"] == "resumed").unwrap()["how"],
        "fork (resume)"
    );
    assert_eq!(kinds(&copy, "user_message"), 2);

    // Without --session-id the fork still gets an id of its own.
    let (_, other) = run(&s, &["--continue", "--fork-session"], "three").await;
    assert_ne!(other, id);
    assert_ne!(other, new_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_starts_in_the_default_mode_not_the_one_it_ended_in() {
    let (_endpoint, s) = setup().await;
    let out = s
        .run(&[
            "-p",
            "--permission-mode",
            "acceptEdits",
            "--output-format",
            "stream-json",
            "one",
        ])
        .await;
    assert_eq!(out.lines()[0]["permission_mode"], "acceptEdits");
    // A headless run without a flag starts in manual, as `-p` does.
    let (resumed, _) = run(&s, &["--continue"], "two").await;
    let _ = resumed;
    let out = s
        .run(&[
            "-p",
            "--continue",
            "--output-format",
            "stream-json",
            "three",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.lines()[0]["permission_mode"], "manual");
}

#[tokio::test(flavor = "multi_thread")]
async fn session_flags_that_do_not_fit_together_are_usage_errors() {
    let (endpoint, s) = setup().await;
    let (_, id) = run(&s, &[], "one").await;
    let uuid = "11111111-2222-4333-8444-555555555555";
    let cases: Vec<Vec<&str>> = vec![
        // The id would name the session, but a resumed one keeps its own.
        vec!["--continue", "--session-id", uuid],
        vec!["--resume", &id, "--session-id", uuid],
        // A fork needs something to fork.
        vec!["--fork-session"],
        // Nothing to resume.
        vec!["--resume", "99999999-2222-4333-8444-555555555555"],
        vec!["--resume", "../escape"],
        // One source only.
        vec!["--continue", "--resume", &id],
    ];
    let before = endpoint.requests().await.len();
    for flags in cases {
        let mut args = vec!["-p", "--permission-mode", "manual"];
        args.extend(flags.iter());
        args.push("go");
        let out = s.run(&args).await;
        assert_eq!(out.code, Some(2), "{flags:?}: {}", out.stderr);
    }
    assert_eq!(
        endpoint.requests().await.len(),
        before,
        "none of them reached the model"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_id_help_says_what_it_does_with_resume() {
    let (_endpoint, s) = setup().await;
    let out = s.run(&["--help"]).await;
    let help = out.stdout;
    assert!(help.contains("--fork-session"), "{help}");
    assert!(
        help.contains("--continue") && help.contains("--resume"),
        "{help}"
    );
    let at = help.find("--session-id").unwrap();
    assert!(
        help[at..].contains("only together with --fork-session"),
        "{help}"
    );
}
