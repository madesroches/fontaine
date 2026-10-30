//! The job-backed `shell`, `job_output`, `job_cancel` and `monitor` tools through the registry.
#![cfg(unix)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{AllowAll, Dirs, call, env_with, registry, scope, text, tool};
use fontaine_tools::builtins::Registered;
use fontaine_tools::gate::AgentId;
use fontaine_tools::jobs::{
    AgentDelivery, AgentState, AgentWaitError, AgentWaits, JobNote, JobSink, Jobs, WaitWake,
};
use futures::future::BoxFuture;
use rig_core::tool::ToolContext;
use serde_json::json;

#[derive(Default)]
struct Notes(Mutex<Vec<JobNote>>);

impl JobSink for Notes {
    fn post(&self, note: JobNote) {
        self.0.lock().unwrap().push(note);
    }
}

fn session() -> (Dirs, Vec<Registered>, Arc<Jobs>, Arc<Notes>) {
    let d = Dirs::new();
    let env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    let notes = Arc::new(Notes::default());
    env.jobs.set_sink(notes.clone());
    let jobs = env.jobs.clone();
    let tools = registry(&env);
    (d, tools, jobs, notes)
}

fn as_child() -> ToolContext {
    let mut context = ToolContext::new();
    context.insert(AgentId("child-1".to_string())).unwrap();
    context
}

async fn run_as_child(
    tools: &[Registered],
    name: &str,
    args: serde_json::Value,
) -> Result<String, String> {
    tool(tools, name)
        .tool
        .run_with(&mut as_child(), args)
        .await
        .map(|out| out.render())
        .map_err(|e| e.model_output().render())
}

fn job_id(text: &str) -> String {
    let at = text.find("job-").expect("a job id in the text");
    text[at..at + 8].to_string()
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let give_up = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < give_up, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_command_that_ends_in_time_reads_as_it_always_did_and_leaves_no_job() {
    let (_d, tools, _, _) = session();
    let out = call(&tools, "shell", json!({"command": "echo hi; exit 2"}))
        .await
        .unwrap();
    assert_eq!(text(&out), "hi\n[exit code 2]");
    let listed = call(&tools, "job_output", json!({})).await.unwrap();
    assert_eq!(text(&listed), "No jobs.");
}

#[tokio::test]
async fn a_command_past_its_timeout_is_handed_back_with_its_output_and_reports_its_end() {
    let (_d, tools, jobs, notes) = session();
    let out = call(
        &tools,
        "shell",
        json!({"command": "echo start; sleep 2; echo done", "timeout_secs": 1}),
    )
    .await
    .unwrap();
    let shown = text(&out);
    assert!(
        shown.starts_with("start\n[still running after 1s; moved to background job job-"),
        "{shown}"
    );
    assert!(
        shown.contains("job_output") && shown.contains("job_cancel"),
        "{shown}"
    );
    let id = job_id(&shown);
    until("the completion note", || {
        !notes.0.lock().unwrap().is_empty()
    })
    .await;
    let notes = notes.0.lock().unwrap().clone();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].job, id);
    assert!(notes[0].text.contains("ended (exit code 0)") && notes[0].text.contains("done"));
    jobs.terminate_all().await;
}

#[tokio::test]
async fn a_background_job_is_listed_read_and_cancelled_through_the_tools() {
    let (_d, tools, _, notes) = session();
    let out = call(
        &tools,
        "shell",
        json!({"command": "echo up; sleep 30", "run_in_background": true}),
    )
    .await
    .unwrap();
    assert!(
        text(&out).contains("started in the background as job-"),
        "{}",
        text(&out)
    );
    assert!(
        text(&out).contains("use job_output to read its output"),
        "{}",
        text(&out)
    );
    let id = job_id(&text(&out));

    let listed = text(&call(&tools, "job_output", json!({})).await.unwrap());
    assert!(
        listed.contains(&id) && listed.contains("running") && listed.contains("sleep 30"),
        "{listed}"
    );

    let read = text(
        &call(&tools, "job_output", json!({"id": id, "wait_secs": 10}))
            .await
            .unwrap(),
    );
    assert!(read.starts_with("up\n"), "{read}");
    assert!(read.contains(&format!("[{id} is running")), "{read}");

    let again = text(&call(&tools, "job_output", json!({"id": id})).await.unwrap());
    assert!(again.starts_with("(no new output)"), "{again}");

    let cancelled = text(&call(&tools, "job_cancel", json!({"id": id})).await.unwrap());
    assert!(cancelled.contains("ended: cancelled"), "{cancelled}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        notes.0.lock().unwrap().is_empty(),
        "the model cancelled it itself"
    );
}

#[tokio::test]
async fn a_subagent_gets_no_jobs_and_keeps_kill_at_timeout() {
    let (_d, tools, jobs, _) = session();
    let refused = run_as_child(
        &tools,
        "shell",
        json!({"command": "sleep 30", "run_in_background": true}),
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("only the main agent runs background jobs"),
        "{refused}"
    );

    let out = run_as_child(
        &tools,
        "shell",
        json!({"command": "sleep 30", "timeout_secs": 1}),
    )
    .await
    .unwrap();
    assert!(out.contains("timed out after 1s and stopped"), "{out}");
    assert!(jobs.snapshot().is_empty(), "a subagent's call is no job");

    let refused = run_as_child(&tools, "job_output", json!({}))
        .await
        .unwrap_err();
    assert!(
        refused.contains("only with the id of a subagent it started"),
        "{refused}"
    );
    for (name, args) in [
        ("job_cancel", json!({"id": "job-0000"})),
        ("monitor", json!({"command": "true", "description": "x"})),
    ] {
        let refused = run_as_child(&tools, name, args).await.unwrap_err();
        assert!(
            refused.contains("only available to the main agent"),
            "{name}: {refused}"
        );
    }
}

#[tokio::test]
async fn the_ninth_background_job_is_refused_naming_job_cancel() {
    let (_d, tools, jobs, _) = session();
    for _ in 0..8 {
        call(
            &tools,
            "shell",
            json!({"command": "sleep 30", "run_in_background": true}),
        )
        .await
        .unwrap();
    }
    let refused = call(
        &tools,
        "shell",
        json!({"command": "sleep 30", "run_in_background": true}),
    )
    .await
    .unwrap_err();
    assert!(refused.model_output().render().contains("job_cancel"));
    let refused = call(
        &tools,
        "monitor",
        json!({"command": "sleep 30", "description": "x"}),
    )
    .await
    .unwrap_err();
    assert!(refused.model_output().render().contains("job_cancel"));
    jobs.terminate_all().await;
}

#[tokio::test]
async fn a_monitor_delivers_its_lines_as_notifications() {
    let (_d, tools, jobs, notes) = session();
    let out = call(
        &tools,
        "monitor",
        json!({"command": "echo ping; sleep 30", "description": "pings", "timeout_secs": 60}),
    )
    .await
    .unwrap();
    let id = job_id(&text(&out));
    until("the notification", || !notes.0.lock().unwrap().is_empty()).await;
    let first = notes.0.lock().unwrap()[0].clone();
    assert!(
        first.text.contains("ping") && first.text.contains("pings"),
        "{}",
        first.text
    );
    // A wait reports status instead of output; the note already posted is not repeated.
    let waited = text(&call(&tools, "job_output", json!({"id": id})).await.unwrap());
    assert!(waited.contains(&format!("[{id} is running")), "{waited}");
    jobs.terminate_all().await;
}

#[tokio::test]
async fn job_output_waits_on_a_monitor_and_reports_each_status() {
    let (_d, tools, jobs, notes) = session();
    let out = call(
        &tools,
        "monitor",
        json!({
            "command": "echo ping; sleep 1; echo pong; sleep 30",
            "description": "pings",
            "timeout_secs": 60,
        }),
    )
    .await
    .unwrap();
    let id = job_id(&text(&out));

    // Still running: no note since the wait began.
    let started = Instant::now();
    let waited = text(
        &call(&tools, "job_output", json!({"id": id, "wait_secs": 10}))
            .await
            .unwrap(),
    );
    assert!(started.elapsed() < Duration::from_secs(9), "{waited}");
    assert!(
        waited.contains(&format!(
            "[{id} has new output; its lines arrive as notifications"
        )),
        "{waited}"
    );

    // Ended while the wait is parked: how, after how long, and where the final output
    // arrives. The end note is held for the wait and posted as its guard leaves.
    let out = call(
        &tools,
        "monitor",
        json!({"command": "sleep 0.5; echo quick", "description": "short", "timeout_secs": 60}),
    )
    .await
    .unwrap();
    let short = job_id(&text(&out));
    let ended = text(
        &call(&tools, "job_output", json!({"id": short, "wait_secs": 10}))
            .await
            .unwrap(),
    );
    assert!(
        ended.contains(&format!("[{short} ended: exit code 0"))
            && ended.contains("final output arrives as a notification"),
        "{ended}"
    );
    until("the end note", || {
        notes
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|note| note.text.contains(&format!("Monitor {short} ended")))
    })
    .await;
    let end_note = notes
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|note| note.text.contains(&format!("Monitor {short} ended")))
        .cloned()
        .unwrap();
    assert!(
        end_note.text.contains("exit code 0") && end_note.text.contains("quick"),
        "{}",
        end_note.text
    );
    let again = text(
        &call(&tools, "job_output", json!({"id": short}))
            .await
            .unwrap(),
    );
    assert!(
        again.contains("ended: exit code 0") && again.contains("was already delivered"),
        "{again}"
    );

    // An unknown id is still an error naming the job.
    let missing = call(&tools, "job_output", json!({"id": "job-nope"}))
        .await
        .unwrap_err();
    assert!(
        missing.model_output().render().contains("there is no job"),
        "{missing:?}"
    );
    jobs.terminate_all().await;
}

#[test]
fn the_shell_definition_offers_jobs_and_no_longer_forbids_servers() {
    let d = Dirs::new();
    let tool = fontaine_tools::builtins::shell_for(&d.dir("w"), &Default::default());
    use rig_core::tool::Tool;
    let description = tool.description();
    assert!(description.contains("run_in_background") && description.contains("wait_secs"));
    assert!(!description.contains("servers in the foreground"));
    let schema = tool.parameters();
    assert_eq!(
        schema["properties"]["run_in_background"]["type"],
        json!(["boolean", "null"])
    );
    assert!(schema["properties"]["command"].is_object());
}

type Asked = Arc<Mutex<Vec<(String, String, Option<Duration>)>>>;

/// A subagent that is still running on a zero wait, and ends after a long one.
struct FakeAgents {
    asked: Asked,
    answer: Option<AgentWaitError>,
    /// When set, the subagent has already ended with this delivery.
    ended: Option<AgentDelivery>,
}

impl AgentWaits for FakeAgents {
    fn wait<'a>(
        &'a self,
        caller: &'a str,
        id: &'a str,
        wait: Option<Duration>,
    ) -> BoxFuture<'a, Result<AgentState, AgentWaitError>> {
        self.asked
            .lock()
            .unwrap()
            .push((caller.to_string(), id.to_string(), wait));
        Box::pin(async move {
            if let Some(error) = self.answer {
                return Err(error);
            }
            if let Some(delivery) = self.ended {
                return Ok(AgentState::Ended {
                    status: "done".into(),
                    delivery,
                });
            }
            if wait == Some(Duration::ZERO) {
                return Ok(AgentState::Running {
                    age: Duration::from_secs(180),
                    on_permission: false,
                });
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
            Ok(AgentState::Ended {
                status: "done".into(),
                delivery: AgentDelivery::Queued,
            })
        })
    }
}

struct FakeWake {
    after: Duration,
    since: Instant,
}

impl WaitWake for FakeWake {
    fn pending(&self, _agent: &str) -> bool {
        self.since.elapsed() >= self.after
    }
}

fn waiting_session(
    answer: Option<AgentWaitError>,
    wake_after: Duration,
) -> (Dirs, Vec<Registered>, Asked) {
    waiting_session_with(answer, None, wake_after)
}

fn waiting_session_with(
    answer: Option<AgentWaitError>,
    ended: Option<AgentDelivery>,
    wake_after: Duration,
) -> (Dirs, Vec<Registered>, Asked) {
    let d = Dirs::new();
    let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    let asked = Asked::default();
    env.agent_waits = Some(Arc::new(FakeAgents {
        asked: asked.clone(),
        answer,
        ended,
    }));
    env.wait_wake = Some(Arc::new(FakeWake {
        after: wake_after,
        since: Instant::now(),
    }));
    let tools = registry(&env);
    (d, tools, asked)
}

#[tokio::test]
async fn an_agent_id_reaches_the_agent_seam_from_a_subagent_and_other_ids_do_not() {
    let (_d, tools, asked) = waiting_session(None, Duration::ZERO);
    let out = run_as_child(
        &tools,
        "job_output",
        json!({"id": "agent-2", "wait_secs": 0}),
    )
    .await
    .unwrap();
    assert_eq!(out, "[agent-2 is running, 3m00s old]");
    assert_eq!(
        asked.lock().unwrap().clone(),
        vec![("child-1".into(), "agent-2".into(), Some(Duration::ZERO))]
    );
    for args in [json!({"id": "job-0000"}), json!({})] {
        let refused = run_as_child(&tools, "job_output", args).await.unwrap_err();
        assert!(
            refused.contains("only with the id of a subagent it started"),
            "{refused}"
        );
    }
}

#[tokio::test]
async fn an_ended_subagent_is_described_by_how_its_report_was_delivered() {
    for (delivery, tail) in [
        (
            AgentDelivery::Queued,
            "Its report follows as a notification.",
        ),
        (
            AgentDelivery::Killed,
            "It was killed and delivers no report.",
        ),
        (
            AgentDelivery::AlreadyDelivered,
            "Its report was already delivered to you.",
        ),
    ] {
        let (_d, tools, _asked) = waiting_session_with(None, Some(delivery), Duration::ZERO);
        let out = run_as_child(&tools, "job_output", json!({"id": "agent-1"}))
            .await
            .unwrap();
        assert_eq!(out, format!("[agent-1 ended: done]\n{tail}"));
    }
}

#[tokio::test]
async fn without_an_agent_seam_an_agent_id_is_refused() {
    let (_d, tools, _, _) = session();
    let err = call(&tools, "job_output", json!({"id": "agent-1"}))
        .await
        .unwrap_err()
        .model_output()
        .render();
    assert!(err.contains("no subagents"), "{err}");
}

#[tokio::test]
async fn the_seams_refusals_read_the_same_whatever_the_relation() {
    let (_d, tools, _) = waiting_session(Some(AgentWaitError::Unknown), Duration::MAX);
    let err = call(&tools, "job_output", json!({"id": "agent-9"}))
        .await
        .unwrap_err()
        .model_output()
        .render();
    assert!(err.contains("no subagent `agent-9`"), "{err}");
    let (_d, tools, _) = waiting_session(Some(AgentWaitError::NotYours), Duration::MAX);
    let err = call(&tools, "job_output", json!({"id": "agent-3"}))
        .await
        .unwrap_err()
        .model_output()
        .render();
    assert!(
        err.contains("`agent-3` is not a subagent you started"),
        "{err}"
    );
}

#[tokio::test]
async fn an_omitted_wait_means_until_it_ends_for_a_subagent_and_now_for_a_job() {
    let (_d, tools, asked) = waiting_session(None, Duration::from_millis(300));
    let started = Instant::now();
    call(&tools, "job_output", json!({"id": "agent-1"}))
        .await
        .unwrap();
    call(
        &tools,
        "job_output",
        json!({"id": "agent-1", "wait_secs": 7}),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let waits: Vec<_> = asked.lock().unwrap().iter().map(|a| a.2).collect();
    assert_eq!(
        waits,
        vec![
            None,
            Some(Duration::ZERO),
            Some(Duration::from_secs(7)),
            Some(Duration::ZERO)
        ],
        "each wake re-asks with a zero wait"
    );

    let started = Instant::now();
    let out = call(
        &tools,
        "shell",
        json!({"command": "sleep 30", "run_in_background": true}),
    )
    .await
    .unwrap();
    let id = job_id(&text(&out));
    let out = call(&tools, "job_output", json!({"id": id})).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "now, not later");
    assert!(text(&out).contains("is running"), "{}", text(&out));
}

#[tokio::test]
async fn a_message_ends_any_wait_early_with_the_zero_wait_answer() {
    let (_d, tools, _) = waiting_session(None, Duration::from_millis(300));
    let started = Instant::now();
    let out = text(
        &call(&tools, "job_output", json!({"id": "agent-1"}))
            .await
            .unwrap(),
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        out,
        "[agent-1 is running, 3m00s old]\n[Stopped waiting: a message arrived.]"
    );

    let shown = text(
        &call(
            &tools,
            "shell",
            json!({"command": "sleep 30", "run_in_background": true}),
        )
        .await
        .unwrap(),
    );
    let id = job_id(&shown);
    // The wake is already pending by now: the call returns at once.
    let started = Instant::now();
    let out = text(
        &call(&tools, "job_output", json!({"id": id, "wait_secs": 600}))
            .await
            .unwrap(),
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        out.starts_with("(no new output)\n[job-")
            && out.ends_with("[Stopped waiting: a message arrived.]"),
        "{out}"
    );
}
