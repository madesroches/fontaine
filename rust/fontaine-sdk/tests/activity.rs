//! The session activity snapshot: what the runtime derives from its turns, agents, jobs,
//! notifications and human waits, the `activity_changed` hook that carries it, and the status-line
//! object that shares its shape. Deterministic: no live APIs, real processes only for jobs and
//! the hook command.
#![cfg(unix)]

#[path = "core_common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::World;
use common::runtime::build;
use fontaine_sdk::agents::mode::Ladder;
use fontaine_sdk::agents::runner::{ChildReport, ChildStatus};
use fontaine_sdk::agents::tree::NotificationKind;
use fontaine_sdk::core::activity::{
    ActivityReason, ActivityState, SessionActivity, Sources, compute,
};
use fontaine_sdk::core::agent::{AgentRuntime, TRANSITION_CAP};
use fontaine_sdk::core::config::PermissionMode::Manual;
use fontaine_sdk::core::permissions::HumanChannel;
use fontaine_sdk::core::steering::Queued;
use fontaine_sdk::tools::gate::ROOT_AGENT;
use fontaine_sdk::tools::jobs::Start;
use fontaine_testkit::example_bin;
use serde_json::Value;
use tokio::sync::watch;

fn runtime(world: &World) -> AgentRuntime {
    build(world, Manual, Vec::new(), false).runtime
}

fn now(runtime: &AgentRuntime) -> SessionActivity {
    compute(&Sources {
        state: &runtime.state,
        jobs: Some(runtime.jobs()),
        steering: &runtime.steering,
    })
}

fn start_job(runtime: &AgentRuntime, command: &str, background: bool) -> String {
    let process = rig_tools::process::Launcher::default()
        .spawn("sh", ["-c", command], rig_tools::process::Spawn::default())
        .expect("sh starts");
    runtime
        .jobs()
        .start(
            Start {
                command: command.to_owned(),
                background,
                monitor: None,
            },
            process,
        )
        .expect("the job starts")
}

async fn reaches(
    rx: &mut watch::Receiver<SessionActivity>,
    what: &str,
    ready: impl FnMut(&SessionActivity) -> bool,
) -> SessionActivity {
    tokio::time::timeout(Duration::from_secs(20), rx.wait_for(ready))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("the monitor is running")
        .clone()
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let give_up = std::time::Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(
            std::time::Instant::now() < give_up,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn hook_command(args: &[&str]) -> String {
    let mut line = word(&example_bin("fontaine-sdk", "hook_double").to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&word(arg));
    }
    line
}

fn hook_toml(event: &str, extra: &str, args: &[&str]) -> String {
    format!(
        "[[hooks.{event}]]\ntimeout_secs = 10\n{extra}command = '''\n{}\n'''\n",
        hook_command(args)
    )
}

fn recorded(path: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn activity_lines(path: &PathBuf) -> Vec<Value> {
    recorded(path)
        .into_iter()
        .filter(|event| event["hook_event_name"] == "ActivityChanged")
        .collect()
}

fn with_hook(extra: &str) -> (World, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let world = World::new();
    world.user_file(&hook_toml(
        "activity_changed",
        extra,
        &["--append", &log.to_string_lossy()],
    ));
    (world, log, dir)
}

// ---- the headline cases ----

#[tokio::test]
async fn a_background_job_keeps_the_session_active_past_the_turn_and_idle_follows_its_end() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    assert_eq!(rx.borrow().state, ActivityState::Idle);

    let id = start_job(&runtime, "sleep 30", true);
    let active = reaches(&mut rx, "the job to count", |a| {
        a.running_background_jobs == 1
    })
    .await;
    assert_eq!(active.state, ActivityState::Active);

    // The turn ends (its scope drops) and the answer is out: the job still holds the session.
    let scope = runtime.state.activity().turn_scope();
    reaches(&mut rx, "the turn to start", |a| a.turn_running).await;
    drop(scope);
    let after_stop = reaches(&mut rx, "the turn to end", |a| !a.turn_running).await;
    assert_eq!(after_stop.state, ActivityState::Active);

    assert!(runtime.jobs().kill_by_user(&id));
    let idle = reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
    assert_eq!(idle.running_background_jobs, 0);
}

#[tokio::test]
async fn a_timeout_promoted_foreground_command_counts_like_a_background_job() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let id = start_job(&runtime, "sleep 30", false);
    // A foreground command is the call's own work, not a background job.
    assert_eq!(now(&runtime).running_background_jobs, 0);
    let promoted = runtime
        .jobs()
        .foreground(&id, Duration::from_millis(100), None, true)
        .await;
    assert!(promoted.end.is_none());
    let active = reaches(&mut rx, "the promoted job", |a| {
        a.running_background_jobs == 1
    })
    .await;
    assert_eq!(active.state, ActivityState::Active);
    runtime.jobs().terminate_all().await;
    reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
}

#[tokio::test]
async fn a_job_that_ends_on_its_own_leaves_a_host_without_notification_turns_idle() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    start_job(&runtime, "sleep 0.3", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    // Headless, Claude-compat and SDK hosts drain the note with the next message, not on their own.
    let idle = reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
    assert_eq!(idle.pending_notifications, 0);
    assert!(runtime.state.agents().has_job_notifications());
}

#[tokio::test]
async fn in_the_terminal_a_finished_job_keeps_the_session_active_until_its_turn_ends() {
    let world = World::new();
    let runtime = runtime(&world);
    runtime.state.activity().set_delivers_between_messages(true);
    let mut rx = runtime.activity();
    start_job(&runtime, "sleep 0.3", true);
    let queued = reaches(&mut rx, "the note", |a| {
        a.running_background_jobs == 0 && a.pending_notifications == 1
    })
    .await;
    assert_eq!(queued.state, ActivityState::Active);

    // Sampled at every step of the hand-over from the inbox to the turn.
    let turn = runtime.notification_prompt().await.expect("a note waits");
    let claimed = now(&runtime);
    assert!(claimed.turn_running && claimed.state == ActivityState::Active);
    assert_eq!(claimed.pending_notifications, 0);
    let scope = runtime.state.activity().turn_scope();
    drop(turn);
    assert!(now(&runtime).turn_running, "the scope outlives the claim");
    drop(scope);
    reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
}

#[tokio::test]
async fn an_empty_inbox_poll_takes_no_claim() {
    let world = World::new();
    let runtime = runtime(&world);
    assert!(runtime.notification_prompt().await.is_none());
    assert_eq!(runtime.state.activity().turns(), 0);
}

// ---- read order ----

#[tokio::test]
async fn a_subagent_report_is_never_between_the_running_count_and_the_inbox() {
    let world = World::new();
    let runtime = runtime(&world);
    runtime.state.activity().set_delivers_between_messages(true);
    let tree = runtime.state.agents();
    let a = tree
        .reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
        .unwrap();
    assert_eq!(now(&runtime).state, ActivityState::Active);
    // The report is queued before the node leaves running, as the runner does.
    tree.notify_report(ROOT_AGENT, &a, "done".into());
    let queued = now(&runtime);
    assert!(queued.running_agents == 1 && queued.pending_notifications >= 1);
    tree.finish(
        &a,
        ChildReport {
            status: ChildStatus::Done,
            text: "done".into(),
            reason: None,
        },
    );
    assert_eq!(
        now(&runtime).state,
        ActivityState::Active,
        "end not yet settled"
    );
    tree.end_settled(&a);
    let left = now(&runtime);
    assert_eq!(left.running_agents, 0);
    assert_eq!(left.pending_notifications, 1);
    assert_eq!(left.state, ActivityState::Active);
}

#[tokio::test]
async fn a_held_waiters_note_posted_under_a_scope_is_read_before_the_scope_drops() {
    // A note posted while a scope is held and the scope dropped after: the counter is read
    // before the inbox, so the note is seen or the scope still is.
    let world = World::new();
    let runtime = runtime(&world);
    runtime.state.activity().set_delivers_between_messages(true);
    let scope = runtime.state.activity().turn_scope();
    runtime
        .state
        .agents()
        .post_to_root("job-1", NotificationKind::Job, "ended".into());
    drop(scope);
    let after = now(&runtime);
    assert_eq!(after.pending_notifications, 1);
    assert_eq!(after.state, ActivityState::Active);
}

// ---- held deliveries ----

#[tokio::test]
async fn notes_no_surface_will_deliver_are_not_pending() {
    let world = World::new();
    let runtime = runtime(&world);
    runtime.state.activity().set_delivers_between_messages(true);
    runtime
        .state
        .agents()
        .post_to_root("job-1", NotificationKind::Job, "killed".into());
    assert_eq!(now(&runtime).state, ActivityState::Active);
    assert!(runtime.state.afk_stop(5.0, 3.0, Vec::new()));
    let held = now(&runtime);
    assert_eq!(held.pending_notifications, 0);
    assert_eq!(held.state, ActivityState::Idle);
    assert!(runtime.state.afk_review());
    let resumed = now(&runtime);
    assert_eq!(resumed.pending_notifications, 1);
    assert_eq!(resumed.state, ActivityState::Active);
}

// ---- wake sources ----

#[tokio::test]
async fn every_source_of_the_snapshot_republishes_when_it_changes() {
    let world = World::new();
    let runtime = runtime(&world);
    runtime.state.activity().set_delivers_between_messages(true);
    let mut rx = runtime.activity();
    let tree = runtime.state.agents().clone();

    tree.post_to_root("job-1", NotificationKind::Job, "ended".into());
    reaches(&mut rx, "the note", |a| a.pending_notifications == 1).await;
    tree.take_notifications(ROOT_AGENT);
    reaches(&mut rx, "the drain", |a| a.pending_notifications == 0).await;

    tree.post_to_root("job-1", NotificationKind::Job, "ended".into());
    reaches(&mut rx, "the note", |a| a.pending_notifications == 1).await;
    tree.discard_job_notifications();
    reaches(&mut rx, "the discard", |a| a.pending_notifications == 0).await;

    tree.post_to_root(ROOT_AGENT, NotificationKind::Reminder, "board".into());
    reaches(&mut rx, "the reminder", |a| a.pending_notifications == 1).await;
    tree.discard_root_reminders();
    reaches(&mut rx, "the discard", |a| a.pending_notifications == 0).await;

    // Every mutation of the steering queue.
    let steering = runtime.steering.clone();
    steering.queue(Queued::Message("one".into()));
    let queued = reaches(&mut rx, "the queue", |a| a.queued_prompts == 1).await;
    assert_eq!(queued.state, ActivityState::Active);
    let taken = steering.retract();
    reaches(&mut rx, "the retract", |a| a.queued_prompts == 0).await;
    for entry in taken {
        steering.requeue(entry);
    }
    reaches(&mut rx, "the requeue", |a| a.queued_prompts == 1).await;
    steering.retract_typed();
    reaches(&mut rx, "the typed retract", |a| a.queued_prompts == 0).await;
    steering.drain_remote();
    steering
        .queue_remote(
            fontaine_sdk::core::ledger::RemoteSender {
                node: "node-1".into(),
                message_id: "m-1".into(),
            },
            Queued::Message("from a device".into()),
        )
        .unwrap();
    reaches(&mut rx, "the remote queue", |a| a.queued_prompts == 1).await;
    steering.drain_remote();
    reaches(&mut rx, "the remote drain", |a| a.queued_prompts == 0).await;

    // A host's own queue.
    runtime.state.activity().set_queued_inputs(2);
    reaches(&mut rx, "the host queue", |a| a.queued_prompts == 2).await;
    runtime.state.activity().set_queued_inputs(0);
    reaches(&mut rx, "the host dequeue", |a| a.queued_prompts == 0).await;
}

#[tokio::test]
async fn a_burst_that_returns_to_the_same_state_publishes_nothing() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let seq = rx.borrow().seq;
    // The current-thread runtime does not run the monitor between these, so it only ever sees
    // the state it started from.
    drop(runtime.state.activity().turn_scope());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rx.borrow_and_update().seq, seq);
}

#[tokio::test]
async fn seq_only_ever_grows() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let mut last = rx.borrow().seq;
    for _ in 0..3 {
        let id = start_job(&runtime, "sleep 30", true);
        let up = reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
        assert!(up.seq > last);
        last = up.seq;
        runtime.jobs().kill_by_user(&id);
        let down = reaches(&mut rx, "the end", |a| a.running_background_jobs == 0).await;
        assert!(down.seq > last);
        last = down.seq;
    }
}

// ---- cancellation paths ----

#[tokio::test]
async fn every_way_a_job_ends_updates_the_snapshot() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();

    // The panel's kill.
    let id = start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    runtime.jobs().kill_by_user(&id);
    reaches(&mut rx, "the panel kill", |a| {
        a.running_background_jobs == 0
    })
    .await;

    // The model's cancel posts no note, so nothing is left pending either.
    let id = start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    runtime
        .jobs()
        .cancel(&id)
        .await
        .expect("the job is cancelled");
    let cancelled = reaches(&mut rx, "the cancel", |a| a.running_background_jobs == 0).await;
    assert_eq!(cancelled.state, ActivityState::Idle);

    // `/clear`: terminate and discard.
    start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    runtime.clear_conversation().await.unwrap();
    reaches(&mut rx, "the clear", |a| a.state == ActivityState::Idle).await;
}

// ---- waiting ----

#[tokio::test]
async fn a_human_wait_is_its_own_state_and_ends_with_its_guard() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    let guard = runtime.state.wait_on_human(ROOT_AGENT);
    let waiting = reaches(&mut rx, "waiting", |a| a.waiting_on_human).await;
    assert_eq!(
        waiting.state,
        ActivityState::Waiting,
        "waiting beats the job"
    );
    drop(guard);
    let back = reaches(&mut rx, "the answer", |a| !a.waiting_on_human).await;
    assert_eq!(back.state, ActivityState::Active);
    runtime.jobs().terminate_all().await;
}

#[tokio::test]
async fn a_subagent_on_a_prompt_makes_the_session_wait() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let a = runtime
        .state
        .agents()
        .reserve(ROOT_AGENT, "worker", "m", vec![], Ladder::Manual)
        .unwrap();
    let guard = runtime.state.wait_on_human(&a);
    let waiting = reaches(&mut rx, "waiting", |a| a.waiting_on_human).await;
    assert_eq!(
        (waiting.state, waiting.running_agents),
        (ActivityState::Waiting, 1)
    );
    drop(guard);
    reaches(&mut rx, "the answer", |a| !a.waiting_on_human).await;
    runtime.state.agents().kill_all();
}

#[tokio::test]
async fn a_question_waits_only_where_a_human_can_answer() {
    let world = World::new();
    let runtime = runtime(&world);
    let state = &runtime.state;
    assert!(
        state
            .wait_on_answer(ROOT_AGENT, HumanChannel::Absent)
            .is_none()
    );
    let present = state.wait_on_answer(ROOT_AGENT, HumanChannel::Present);
    assert!(present.is_some());
    assert!(now(&runtime).waiting_on_human);
    drop(present);
    assert!(!now(&runtime).waiting_on_human);
    assert!(
        state
            .wait_on_answer("agent-1", HumanChannel::Delegate)
            .is_some()
    );
    state
        .set_mode(
            fontaine_sdk::core::config::PermissionMode::Afk,
            fontaine_sdk::core::afk_report::AfkSource::Cli,
        )
        .expect("AFK starts");
    assert!(
        state
            .wait_on_answer(ROOT_AGENT, HumanChannel::Present)
            .is_none(),
        "AFK answers for the person"
    );
}

// ---- the hook ----

#[tokio::test]
async fn the_hook_carries_the_snapshot_the_reason_and_the_previous_state() {
    let (world, log, _dir) = with_hook("");
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let id = start_job(&runtime, "sleep 30", true);
    until("the first delivery", || !activity_lines(&log).is_empty()).await;
    runtime.jobs().kill_by_user(&id);
    reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
    until("the second delivery", || activity_lines(&log).len() >= 2).await;

    let lines = activity_lines(&log);
    let (first, last) = (&lines[0], lines.last().unwrap());
    assert_eq!(first["reason"], "job_started");
    assert_eq!(first["previous_state"], "idle");
    assert_eq!(first["activity"]["state"], "active");
    assert_eq!(first["activity"]["running_background_jobs"], 1);
    assert_eq!(first["activity"]["session_id"], first["session_id"]);
    assert_eq!(last["reason"], "job_ended");
    assert_eq!(last["previous_state"], "active");
    assert_eq!(last["activity"]["state"], "idle");
    assert!(last["activity"]["seq"].as_u64() > first["activity"]["seq"].as_u64());
}

#[tokio::test]
async fn a_matcher_hooks_only_the_state_it_names() {
    let (world, log, _dir) = with_hook("matcher = \"idle\"\n");
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let id = start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    runtime.jobs().kill_by_user(&id);
    reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
    until("the idle delivery", || !activity_lines(&log).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let lines = activity_lines(&log);
    assert!(
        lines.iter().all(|l| l["activity"]["state"] == "idle"),
        "{lines:?}"
    );
}

#[tokio::test]
async fn a_slow_hook_never_reorders_and_the_final_state_is_always_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let world = World::new();
    world.user_file(&hook_toml(
        "activity_changed",
        "",
        &["--append", &log.to_string_lossy(), "--sleep", "1"],
    ));
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    // The first delivery is slow; everything after it queues behind it.
    let id = start_job(&runtime, "sleep 30", true);
    until("the first delivery to start", || {
        !activity_lines(&log).is_empty()
    })
    .await;
    for _ in 0..2 {
        let scope = runtime.state.activity().turn_scope();
        reaches(&mut rx, "the turn", |a| a.turn_running).await;
        drop(scope);
        reaches(&mut rx, "the turn to end", |a| !a.turn_running).await;
    }
    runtime.jobs().kill_by_user(&id);
    let published = reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
    until("the final delivery", || {
        activity_lines(&log)
            .last()
            .is_some_and(|l| l["activity"]["seq"].as_u64() == Some(published.seq))
    })
    .await;
    let lines = activity_lines(&log);
    assert!(
        (lines.len() as u64) < published.seq,
        "intermediate snapshots were skipped: {} delivered of {}",
        lines.len(),
        published.seq
    );
    let seqs: Vec<u64> = lines
        .iter()
        .map(|l| l["activity"]["seq"].as_u64().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    // Relative to the last delivered snapshot, not the last published one.
    for pair in lines.windows(2) {
        assert_eq!(pair[1]["previous_state"], pair[0]["activity"]["state"]);
    }
}

#[tokio::test]
async fn clear_carries_its_reason_even_when_nothing_else_differs() {
    let (world, log, _dir) = with_hook("");
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    runtime.clear_conversation().await.unwrap();
    until("a clear delivery", || {
        activity_lines(&log).iter().any(|l| l["reason"] == "clear")
    })
    .await;
    // A clear with nothing running still delivers one, never lost to deduplication.
    let before = activity_lines(&log).len();
    runtime.clear_conversation().await.unwrap();
    until("the empty clear", || activity_lines(&log).len() > before).await;
    assert_eq!(activity_lines(&log).last().unwrap()["reason"], "clear");
}

#[tokio::test]
async fn the_final_transition_is_delivered_once_before_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let world = World::new();
    let path = log.to_string_lossy().into_owned();
    world.user_file(&format!(
        "{}{}",
        hook_toml("activity_changed", "", &["--append", &path]),
        hook_toml("session_end", "", &["--append", &path]),
    ));
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    let id = start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;
    runtime.jobs().kill_by_user(&id);
    reaches(&mut rx, "idle", |a| a.state == ActivityState::Idle).await;
    until("the job end delivery", || {
        activity_lines(&log)
            .iter()
            .any(|l| l["reason"] == "job_ended")
    })
    .await;
    let seq_before = rx.borrow().seq;

    runtime.fire_session_end("other", TRANSITION_CAP).await;

    let events = recorded(&log);
    let n = events.len();
    assert_eq!(events[n - 1]["hook_event_name"], "SessionEnd", "{events:?}");
    let shutdown = &events[n - 2];
    assert_eq!(shutdown["hook_event_name"], "ActivityChanged");
    assert_eq!(shutdown["reason"], "shutdown");
    assert_eq!(shutdown["activity"]["seq"].as_u64(), Some(seq_before + 1));
    assert_eq!(shutdown["activity"]["state"], "idle");
    assert_eq!(
        events.iter().filter(|e| e["reason"] == "shutdown").count(),
        1
    );

    // Nothing more once the session has ended.
    start_job(&runtime, "sleep 0.2", true);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(recorded(&log).len(), n);
    runtime.jobs().terminate_all().await;
}

#[tokio::test]
async fn a_slow_activity_hook_cannot_starve_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let world = World::new();
    let path = log.to_string_lossy().into_owned();
    world.user_file(&format!(
        "{}{}",
        hook_toml("activity_changed", "", &["--sleep", "20"]),
        hook_toml("session_end", "", &["--append", &path]),
    ));
    let runtime = runtime(&world);
    let started = std::time::Instant::now();
    runtime
        .fire_session_end("other", Duration::from_millis(300))
        .await;
    assert!(started.elapsed() < Duration::from_secs(5));
    let events = recorded(&log);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["hook_event_name"], "SessionEnd");
}

#[tokio::test]
async fn the_final_exit_delivers_the_transition_after_the_jobs_are_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let world = World::new();
    let path = log.to_string_lossy().into_owned();
    world.user_file(&format!(
        "{}{}",
        hook_toml("activity_changed", "", &["--append", &path]),
        hook_toml("session_end", "", &["--append", &path]),
    ));
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    start_job(&runtime, "sleep 30", true);
    reaches(&mut rx, "the job", |a| a.running_background_jobs == 1).await;

    runtime.begin_final_shutdown();
    runtime
        .finish_final_shutdown(std::time::Instant::now() + Duration::from_secs(5))
        .await;

    let events = recorded(&log);
    let n = events.len();
    assert_eq!(events[n - 1]["hook_event_name"], "SessionEnd", "{events:?}");
    assert_eq!(events[n - 2]["reason"], "shutdown");
    assert_eq!(events[n - 2]["activity"]["running_background_jobs"], 0);
    assert_eq!(events[n - 2]["activity"]["state"], "idle");
}

// ---- monitor lifetime ----

#[test]
fn a_runtime_built_without_tokio_does_not_panic() {
    let world = World::new();
    let runtime = build(&world, Manual, Vec::new(), false).runtime;
    assert_eq!(runtime.activity().borrow().state, ActivityState::Idle);
}

#[tokio::test]
async fn the_monitor_ends_with_its_runtime() {
    let world = World::new();
    let runtime = runtime(&world);
    let mut rx = runtime.activity();
    drop(runtime);
    let ended = tokio::time::timeout(Duration::from_secs(5), rx.changed())
        .await
        .expect("the sender is dropped with the monitor");
    assert!(ended.is_err());
}

#[test]
fn the_shared_object_is_what_the_status_line_carries() {
    let activity = SessionActivity {
        session_id: "s".into(),
        seq: 3,
        state: ActivityState::Waiting,
        turn_running: true,
        waiting_on_human: true,
        running_agents: 1,
        running_background_jobs: 2,
        running_monitors: 1,
        pending_notifications: 0,
        queued_prompts: 4,
    };
    let payload = fontaine_sdk::core::statusline::build_payload(
        "m".into(),
        std::path::Path::new("/w"),
        std::path::Path::new("/w"),
        Vec::new(),
        "s".into(),
        "1".into(),
        None,
        Manual,
        None,
        false,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    )
    .with_activity(&activity);
    let json = serde_json::to_value(&payload).unwrap();
    assert_eq!(json["activity"], activity.to_json());
    assert_eq!(ActivityReason::Clear.as_str(), "clear");
}
