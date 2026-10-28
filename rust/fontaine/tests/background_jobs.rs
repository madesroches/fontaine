//! Background shell jobs through `fontaine -p`: a job that ended before the answer gets a
//! notification turn, and a run that ends with a job still running terminates it.
#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use common::{Endpoint, Route, Sandbox, Turn, Wire};
use serde_json::{Value, json};

fn sandbox(endpoint: &Endpoint) -> Sandbox {
    let s = Sandbox::new();
    s.point_at(&endpoint.url());
    s
}

async fn run(s: &Sandbox, task: &str) -> common::Output {
    s.headless(&[
        "--allowed-tools",
        "shell",
        "--output-format",
        "stream-json",
        task,
    ])
    .await
}

fn of_type<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|l| l["type"] == kind).collect()
}

fn gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z')),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_ended_before_the_answer_gets_a_notification_turn() {
    // The slow route holds each answer back long enough for the job to end between them.
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new(
                "root task",
                vec![
                    Turn::tool(
                        "c1",
                        "shell",
                        json!({"command": "sleep 0.3; echo job-says-hi", "run_in_background": true}),
                    ),
                    Turn::text("started it"),
                    Turn::text("the job said hi"),
                ],
            )
            .slow(Duration::from_millis(1000)),
        ],
    )
    .await;
    let s = sandbox(&endpoint);
    let out = run(&s, "root task").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();

    let shell = of_type(&lines, "tool_result")
        .into_iter()
        .find(|r| r["tool"] == "shell")
        .expect("a shell result");
    assert!(
        shell["text"]
            .as_str()
            .unwrap()
            .contains("started in the background as job-"),
        "{shell}"
    );
    // The run waited for the note's turn: the last result is the answer after it.
    let results = of_type(&lines, "result");
    assert_eq!(results.last().unwrap()["result"], "the job said hi");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 3, "a third request carried the note");
    let third = requests[2].to_string();
    assert!(
        third.contains("agents or background jobs you started have news"),
        "{third}"
    );
    assert!(third.contains("ended (exit code 0)"), "{third}");
    assert!(third.contains("job-says-hi"), "{third}");
    assert!(third.contains("untrusted data"), "{third}");
    for earlier in &requests[..2] {
        assert!(!earlier.to_string().contains("ended (exit code 0)"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_ends_with_a_job_still_running_terminates_it() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "shell",
                json!({"command": "sleep 120 & echo $! > child.pid; wait", "run_in_background": true}),
            ),
            Turn::text("it runs"),
        ],
    )
    .await;
    let s = sandbox(&endpoint);
    let out = run(&s, "start a server").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let pid: u32 = std::fs::read_to_string(s.work.join("child.pid"))
        .expect("the job started")
        .trim()
        .parse()
        .unwrap();
    // The run does not wait for the job, and nothing is left behind.
    let started = std::time::Instant::now();
    while !gone(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "pid {pid} survives"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(endpoint.requests().await.len(), 2);
}
