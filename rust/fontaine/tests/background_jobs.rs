//! Background shell jobs through `fontaine -p`: a job that ended before the answer gets a
//! notification turn, a run that ends with a job still running terminates it, and a goal run waits
//! for the jobs it started.
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
    // The job polls for a marker the endpoint releases from request 2. Two ordering edges
    // shape the test: `NotificationStage` drains an already queued job note into the next
    // tool result, so an instant job would deliver its note in request 2 instead of a turn of
    // its own; and `wait_for_news` returns `Done` after an answer when no note is queued, so
    // the run never waits for a background job. Releasing the job at request 2 — which cannot
    // arrive before the shell result passed through `NotificationStage` — excludes the first
    // edge, while the held-back answer covers reaping and posting the note before `wait_for_news`.
    let s = Sandbox::new();
    let release = s.runtime.join("job-release");
    let marker = release.clone();
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new(
                "root task",
                vec![
                    Turn::tool(
                        "c1",
                        "shell",
                        json!({"command": format!("while ! test -e '{}'; do sleep 0.05; done; echo job-says-hi", release.display()), "run_in_background": true}),
                    ),
                    Turn::text("started it"),
                    Turn::text("the job said hi"),
                ],
            )
            .on_serve(move |at| {
                if at == 1 {
                    std::fs::write(&marker, "go").expect("release the job");
                }
            })
            .slow(Duration::from_millis(1000)),
        ],
    )
    .await;
    s.point_at(&endpoint.url());
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

fn verdict(verdict: &str, reason: &str) -> Turn {
    Turn::tool(
        "g1",
        "submit",
        json!({"verdict": verdict, "reason": reason}),
    )
}

fn is_evaluation(request: &Value) -> bool {
    request.to_string().contains("Goal condition:")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_run_waits_for_a_background_job_to_end_before_evaluating() {
    // The job outlives the answer: it sleeps well past the shell result and the model's reply,
    // so the evaluation can only come after the end note's turn.
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new("Goal condition:", vec![verdict("met", "the job reported")]),
            Route::new(
                "the job reported",
                vec![
                    Turn::tool(
                        "c1",
                        "shell",
                        json!({"command": "sleep 2; echo job-says-hi", "run_in_background": true}),
                    ),
                    Turn::text("started it"),
                    Turn::text("the job said hi"),
                ],
            ),
        ],
    )
    .await;
    let s = sandbox(&endpoint);
    let out = run(&s, "/goal the job reported").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "result").last().copied().unwrap().clone();
    assert_eq!(result["goal"]["status"], "met", "{result}");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 4, "{requests:#?}");
    assert!(
        is_evaluation(&requests[3]),
        "the evaluation came last, after the note's turn"
    );
    assert!(
        requests[2].to_string().contains("ended (exit code 0)"),
        "{}",
        requests[2]
    );
    assert!(
        requests[..3].iter().all(|r| !is_evaluation(r)),
        "nothing was evaluated while the job ran"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn with_check_ins_off_a_goal_run_does_not_wait_and_pauses_on_the_background_wait() {
    let endpoint = Endpoint::start_routed(
        Wire::Chat,
        vec![
            Route::new("Goal condition:", vec![verdict("met", "unreachable")]),
            Route::new(
                "the job reported",
                vec![
                    Turn::tool(
                        "c1",
                        "shell",
                        json!({"command": "sleep 120", "run_in_background": true}),
                    ),
                    Turn::text("started it"),
                ],
            ),
        ],
    )
    .await;
    let s = Sandbox::new();
    s.point_at(&endpoint.url());
    let base = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!("{base}\n[goal]\ncheckin_minutes = 0\n"));
    let out = run(&s, "/goal the job reported").await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "result").last().copied().unwrap().clone();
    assert_eq!(result["goal"]["status"], "paused", "{result}");
    assert!(
        result["goal"]["reason"]
            .as_str()
            .unwrap()
            .contains("background work is still running"),
        "{result}"
    );
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2, "no evaluation of an unfinished wait");
    assert!(requests.iter().all(|r| !is_evaluation(r)));
}
