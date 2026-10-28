use std::path::Path;

use rig_tools::process::{Launcher, Spawn};

use super::*;

#[derive(Default)]
struct FakeSink {
    notes: Mutex<Vec<JobNote>>,
}

impl FakeSink {
    fn notes(&self) -> Vec<JobNote> {
        self.notes.lock().unwrap().clone()
    }
}

impl JobSink for FakeSink {
    fn post(&self, note: JobNote) {
        self.notes.lock().unwrap().push(note);
    }
}

fn jobs() -> (Jobs, Arc<FakeSink>) {
    let jobs = Jobs::new();
    let sink = Arc::new(FakeSink::default());
    jobs.set_sink(sink.clone());
    (jobs, sink)
}

fn process(command: &str) -> Process {
    Launcher::default()
        .spawn("sh", ["-c", command], Spawn::default())
        .expect("sh starts")
}

fn start(jobs: &Jobs, command: &str, background: bool) -> String {
    jobs.start(
        Start {
            command: command.to_owned(),
            background,
            monitor: None,
        },
        process(command),
    )
    .expect("the job starts")
}

fn monitor(jobs: &Jobs, command: &str, timeout: Duration) -> String {
    jobs.start(
        Start {
            command: command.to_owned(),
            background: true,
            monitor: Some(MonitorSpec {
                description: "watching".to_owned(),
                timeout,
            }),
        },
        process(command),
    )
    .expect("the monitor starts")
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let give_up = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < give_up, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn running(jobs: &Jobs) -> usize {
    jobs.snapshot().iter().filter(|job| job.running).count()
}

fn lines(chunk: &Chunk) -> String {
    chunk.lines.iter().map(|(_, line)| line.as_str()).collect()
}

/// Whether the process `pid` is gone, or only waiting to be reaped.
#[cfg(target_os = "linux")]
fn gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z')),
    }
}

#[cfg(target_os = "linux")]
async fn pid_in(path: &Path) -> u32 {
    until("the pid file", || {
        std::fs::read_to_string(path).is_ok_and(|text| text.trim().parse::<u32>().is_ok())
    })
    .await;
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn a_command_that_ends_in_time_leaves_no_job() {
    let (jobs, sink) = jobs();
    let id = start(&jobs, "echo hi; exit 3", false);
    let ran = jobs
        .foreground(&id, Duration::from_secs(20), None, true)
        .await;
    assert_eq!(lines(&ran.chunk), "hi\n");
    let end = ran.end.expect("it ended");
    assert!(matches!(end.end, End::Exited(status) if status.code() == Some(3)));
    assert!(jobs.snapshot().is_empty());
    assert!(sink.notes().is_empty());
}

#[tokio::test]
async fn a_command_past_its_timeout_moves_to_the_background_and_reports_its_end_once() {
    let (jobs, sink) = jobs();
    let id = start(&jobs, "echo start; sleep 1; echo finish", false);
    let ran = jobs
        .foreground(&id, Duration::from_millis(300), None, true)
        .await;
    assert!(ran.end.is_none(), "it still runs");
    assert_eq!(lines(&ran.chunk), "start\n");
    assert_eq!(running(&jobs), 1);
    until("the completion note", || !sink.notes().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let notes = sink.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].job, id);
    assert!(
        notes[0].text.contains("ended (exit code 0)"),
        "{}",
        notes[0].text
    );
    assert!(notes[0].text.contains("finish"), "{}", notes[0].text);
    assert!(notes[0].text.contains("untrusted"), "{}", notes[0].text);
}

#[tokio::test]
async fn a_command_that_may_not_move_is_stopped_at_its_timeout() {
    let (jobs, sink) = jobs();
    let id = start(&jobs, "sleep 30", false);
    let started = Instant::now();
    let ran = jobs
        .foreground(&id, Duration::from_millis(200), None, false)
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(
        ran.end.map(|e| e.end),
        Some(End::Killed(Kill::Timeout))
    ));
    assert!(jobs.snapshot().is_empty());
    assert!(sink.notes().is_empty());
}

#[tokio::test]
async fn silence_moves_a_command_only_when_a_quiet_limit_is_given() {
    let (jobs, _) = jobs();
    let id = start(&jobs, "echo once; sleep 30", false);
    let ran = jobs
        .foreground(
            &id,
            Duration::from_secs(60),
            Some(Duration::from_millis(300)),
            true,
        )
        .await;
    assert!(ran.end.is_none());
    assert_eq!(lines(&ran.chunk), "once\n");
    jobs.terminate_all().await;
}

#[tokio::test]
async fn running_in_the_background_returns_at_once() {
    let (jobs, _) = jobs();
    let started = Instant::now();
    let id = start(&jobs, "sleep 30", true);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(running(&jobs), 1);
    assert!(id.starts_with("job-") && id.len() == 8, "{id}");
    jobs.terminate_all().await;
    assert_eq!(running(&jobs), 0);
}

#[tokio::test]
async fn a_ninth_background_job_is_refused_and_a_promotion_past_the_cap_stops_the_command() {
    let (jobs, _) = jobs();
    for _ in 0..MAX_RUNNING {
        start(&jobs, "sleep 30", true);
    }
    assert!(!jobs.has_room());
    let refused = jobs.start(
        Start {
            command: "sleep 30".to_owned(),
            background: true,
            monitor: None,
        },
        process("sleep 30"),
    );
    assert!(matches!(refused, Err(JobError::TooMany)));
    let foreground = start(&jobs, "sleep 30", false);
    let ran = jobs
        .foreground(&foreground, Duration::from_millis(200), None, true)
        .await;
    assert!(matches!(
        ran.end.map(|e| e.end),
        Some(End::Killed(Kill::Cap))
    ));
    jobs.terminate_all().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_the_foreground_future_stops_the_group_but_not_a_promoted_job() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let (jobs, _) = jobs();
    let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
    let id = start(&jobs, &command, false);
    let dropped = tokio::time::timeout(
        Duration::from_millis(500),
        jobs.foreground(&id, Duration::from_secs(60), None, true),
    )
    .await;
    assert!(dropped.is_err(), "the wait was cut short");
    let pid = pid_in(&pid_file).await;
    until("the child to be stopped", || gone(pid)).await;
    until("the job to be removed", || jobs.snapshot().is_empty()).await;

    let promoted = dir.path().join("promoted");
    let command = format!("sleep 30 & echo $! > {}; wait", promoted.display());
    let id = start(&jobs, &command, false);
    let ran = jobs
        .foreground(&id, Duration::from_millis(500), None, true)
        .await;
    assert!(ran.end.is_none());
    let pid = pid_in(&promoted).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!gone(pid), "a promoted job survives the call");
    jobs.terminate_all().await;
    until("the child to be stopped", || gone(pid)).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancelling_a_job_ends_its_whole_group_even_when_it_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let (jobs, sink) = jobs();
    let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
    let id = start(&jobs, &command, true);
    let pid = pid_in(&pid_file).await;
    let started = Instant::now();
    let read = jobs.cancel(&id).await.expect("the job exists");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(
        read.end.map(|e| e.end),
        Some(End::Killed(Kill::Model))
    ));
    until("the child to be stopped", || gone(pid)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(sink.notes().is_empty(), "the model cancelled it itself");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_daemonized_child_dies_after_the_post_exit_grace() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let (jobs, _) = jobs();
    let command = format!(
        "sleep 30 > /dev/null 2>&1 & echo $! > {}",
        pid_file.display()
    );
    start(&jobs, &command, true);
    let pid = pid_in(&pid_file).await;
    until("the job to end", || running(&jobs) == 0).await;
    until("the child to be killed", || gone(pid)).await;
}

#[tokio::test]
async fn the_buffer_drops_old_lines_and_counts_them_and_a_read_returns_only_new_ones() {
    let (jobs, _) = jobs();
    let id = start(
        &jobs,
        "yes 0123456789012345678901234567890123456789 | head -n 60000; sleep 30",
        true,
    );
    until("the output", || {
        jobs.snapshot().iter().any(|job| job.id == id) && {
            let inner = lock(&jobs.shared.inner, "job registry");
            inner.find(&id).is_some_and(|job| job.buffer.next >= 60000)
        }
    })
    .await;
    let first = jobs.output(&id, Duration::ZERO).await.unwrap();
    assert!(first.chunk.missed > 0, "lines were dropped");
    let kept: usize = first.chunk.lines.iter().map(|(_, line)| line.len()).sum();
    assert!(kept <= BUFFER_BYTES, "{kept}");
    assert_eq!(first.chunk.lines.last().map(|(seq, _)| *seq), Some(59999));
    let second = jobs.output(&id, Duration::ZERO).await.unwrap();
    assert!(second.chunk.is_empty());
    assert_eq!(second.chunk.missed, 0);
    jobs.terminate_all().await;
}

#[tokio::test]
async fn waiting_returns_early_on_new_output() {
    let (jobs, _) = jobs();
    let id = start(&jobs, "sleep 1; echo news; sleep 30", true);
    let started = Instant::now();
    let read = jobs.output(&id, Duration::from_secs(20)).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(lines(&read.chunk), "news\n");
    assert!(read.end.is_none());
    jobs.terminate_all().await;
}

#[tokio::test]
async fn the_end_news_goes_out_exactly_once_whichever_side_gets_it() {
    // The waiter holds the news when the job ends: no note.
    let (jobs, sink) = jobs();
    let id = start(&jobs, "sleep 1", true);
    let read = jobs.output(&id, Duration::from_secs(20)).await.unwrap();
    assert!(read.end.is_some() && !read.already_reported);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(sink.notes().is_empty(), "{:?}", sink.notes());
    let again = jobs.output(&id, Duration::ZERO).await.unwrap();
    assert!(again.already_reported, "the end was consumed");

    // The note goes out first: the read returns a marker.
    let id = start(&jobs, "echo quick", true);
    until("the note", || !sink.notes().is_empty()).await;
    let read = jobs.output(&id, Duration::ZERO).await.unwrap();
    assert!(read.already_reported);
    assert!(read.chunk.is_empty());
    assert_eq!(sink.notes().len(), 1);
}

#[tokio::test]
async fn an_abandoned_wait_hands_the_held_note_on() {
    let (jobs, sink) = jobs();
    let id = start(&jobs, "sleep 1", true);
    let waiting = tokio::time::timeout(
        Duration::from_millis(300),
        jobs.output(&id, Duration::from_secs(30)),
    )
    .await;
    assert!(waiting.is_err(), "the wait was dropped");
    until("the note", || !sink.notes().is_empty()).await;
    assert_eq!(sink.notes().len(), 1);
}

#[tokio::test]
async fn the_user_killing_a_job_tells_the_model_and_leaves_the_tail_readable() {
    let (jobs, sink) = jobs();
    let id = start(&jobs, "echo before; sleep 30", true);
    until("the output", || {
        lock(&jobs.shared.inner, "job registry")
            .find(&id)
            .is_some_and(|job| job.buffer.next > 0)
    })
    .await;
    assert!(jobs.kill_by_user(&id));
    until("the note", || !sink.notes().is_empty()).await;
    assert!(sink.notes()[0].text.contains("killed by the user"));
    let read = jobs.output(&id, Duration::ZERO).await.unwrap();
    assert!(read.user_killed && !read.already_reported);
    assert!(lines(&read.chunk).contains("before"));
}

#[tokio::test]
async fn the_user_killing_a_monitor_says_monitor() {
    let (jobs, sink) = jobs();
    let id = monitor(&jobs, "sleep 30", Duration::from_secs(60));
    assert!(jobs.kill_by_user(&id));
    until("the note", || !sink.notes().is_empty()).await;
    assert!(sink.notes()[0].text.starts_with("[Monitor "));
}

#[tokio::test]
async fn a_monitor_is_refused_by_job_output_but_listed() {
    let (jobs, _) = jobs();
    let id = monitor(&jobs, "sleep 30", Duration::from_secs(60));
    assert!(matches!(
        jobs.output(&id, Duration::ZERO).await,
        Err(JobError::Monitor(_))
    ));
    assert!(
        jobs.snapshot()
            .iter()
            .any(|job| job.monitor && job.id == id)
    );
    assert!(matches!(
        jobs.output("job-nope", Duration::ZERO).await,
        Err(JobError::UnknownJob(_))
    ));
    jobs.terminate_all().await;
}

#[tokio::test]
async fn a_monitor_batches_a_burst_into_one_notification_and_ends_with_a_final_note() {
    let (jobs, sink) = jobs();
    let id = monitor(
        &jobs,
        "echo l1; echo l2; echo l3; sleep 30",
        Duration::from_secs(3),
    );
    until("the lines", || !sink.notes().is_empty()).await;
    let first = sink.notes()[0].clone();
    assert!(first.text.contains("l1\nl2\nl3\n"), "{}", first.text);
    assert!(first.text.contains(&id) && first.text.contains("watching"));
    until("the final note", || sink.notes().len() >= 2).await;
    let last = sink.notes().last().cloned().unwrap();
    assert!(
        last.text.contains("ended (reached its timeout)"),
        "{}",
        last.text
    );
    assert_eq!(sink.notes().len(), 2);
}

fn bare_monitor(
    posted: usize,
    pending: usize,
    last_post: Option<Instant>,
) -> (Shared, Arc<FakeSink>) {
    let (changed, _) = watch::channel(0);
    let shared = Shared {
        inner: Mutex::new(Inner { jobs: Vec::new() }),
        changed,
        sink: Mutex::new(None),
    };
    let sink = Arc::new(FakeSink::default());
    *shared.sink.lock().unwrap() = Some(sink.clone());
    let old = Instant::now() - Duration::from_secs(10);
    shared.inner.lock().unwrap().jobs.push(Job {
        id: "job-test".to_owned(),
        command: "x".to_owned(),
        mode: Mode::Background,
        started: old,
        last_output: old,
        buffer: Buffer {
            next: pending as u64,
            ..Buffer::new()
        },
        cursor: 0,
        control: Arc::new(Control {
            kill: AtomicU8::new(0),
            deadline: None,
        }),
        ended: None,
        reported: false,
        waiters: 0,
        held: false,
        monitor: Some(Monitor {
            description: "d".to_owned(),
            pending: (0..pending).map(|n| format!("line {n}\n")).collect(),
            lost: 0,
            since: Some(old),
            last_post,
            posted,
        }),
    });
    (shared, sink)
}

#[test]
fn a_monitor_posts_at_most_two_hundred_lines_and_one_notification_a_second() {
    let (shared, sink) = bare_monitor(0, 450, None);
    shared.monitor_tick("job-test");
    let notes = sink.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text.matches("line ").count(), MONITOR_LINES);
    // The next batch waits for the interval.
    shared.monitor_tick("job-test");
    assert_eq!(sink.notes().len(), 1);
    let mut inner = shared.inner.lock().unwrap();
    let monitor = inner.jobs[0].monitor.as_mut().unwrap();
    assert_eq!(monitor.pending.len(), 250);
    monitor.last_post = Some(Instant::now() - MONITOR_INTERVAL * 2);
    monitor.since = Some(Instant::now() - MONITOR_BATCH * 2);
    drop(inner);
    shared.monitor_tick("job-test");
    assert_eq!(sink.notes().len(), 2);
}

#[test]
fn a_monitors_end_note_carries_at_most_two_hundred_lines() {
    let (shared, _) = bare_monitor(0, 450, None);
    let inner = shared.inner.lock().unwrap();
    let note = end_note(&inner.jobs[0]);
    assert_eq!(note.text.matches("line ").count(), MONITOR_LINES);
    assert!(
        note.text.contains("[250 earlier lines dropped]"),
        "{}",
        note.text
    );
    assert!(note.text.contains("line 449\n") && !note.text.contains("line 249\n"));
}

#[tokio::test]
async fn a_command_that_prints_a_lot_and_exits_keeps_the_end_of_its_output() {
    let (jobs, _) = jobs();
    let id = start(&jobs, "seq 1 300000", false);
    let ran = jobs
        .foreground(&id, Duration::from_secs(20), None, true)
        .await;
    assert!(ran.end.is_some());
    assert_eq!(
        ran.chunk.lines.last().map(|(_, l)| l.as_str()),
        Some("300000\n")
    );
}

#[test]
fn a_monitor_past_its_notification_total_is_terminated() {
    let (shared, sink) = bare_monitor(MONITOR_NOTIFICATIONS - 1, 3, None);
    shared.monitor_tick("job-test");
    assert_eq!(sink.notes().len(), 1);
    let inner = shared.inner.lock().unwrap();
    assert_eq!(inner.jobs[0].control.requested(), Some(Kill::Flood));
    let info = EndInfo {
        end: End::Killed(Kill::Flood),
        forced: false,
        remote: None,
    };
    assert!(info.describe().contains("1000 notifications"));
}

#[tokio::test]
async fn terminating_everything_kills_every_group_and_posts_nothing() {
    let (jobs, sink) = jobs();
    for _ in 0..3 {
        start(&jobs, "sleep 30", true);
    }
    let foreground = start(&jobs, "sleep 30", false);
    let wait = jobs.foreground(&foreground, Duration::from_secs(60), None, true);
    let terminating = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        jobs.terminate_all().await;
    };
    let (ran, ()) = tokio::join!(wait, terminating);
    assert!(matches!(
        ran.end.map(|e| e.end),
        Some(End::Killed(Kill::Shutdown))
    ));
    assert_eq!(running(&jobs), 0);
    assert!(sink.notes().is_empty());
}

#[tokio::test]
async fn dropping_the_registry_terminates_its_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let jobs = Jobs::new();
    let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
    start(&jobs, &command, true);
    #[cfg(target_os = "linux")]
    {
        let pid = pid_in(&pid_file).await;
        drop(jobs);
        assert!(gone(pid));
    }
    #[cfg(not(target_os = "linux"))]
    drop(jobs);
}

#[test]
fn ages_read_as_short_durations() {
    assert_eq!(format_age(Duration::from_secs(37)), "37s");
    assert_eq!(format_age(Duration::from_secs(252)), "4m12s");
    assert_eq!(format_age(Duration::from_secs(3720)), "1h02m");
}

#[test]
fn a_notice_is_cut_to_its_byte_budget_keeping_the_newest_lines() {
    let (shared, sink) = bare_monitor(0, 0, None);
    {
        let mut inner = shared.inner.lock().unwrap();
        let monitor = inner.jobs[0].monitor.as_mut().unwrap();
        monitor.pending = (0..MONITOR_LINES)
            .map(|n| format!("{n:03} {}\n", "x".repeat(NOTICE_LINE)))
            .collect();
    }
    shared.monitor_tick("job-test");
    let text = sink.notes()[0].text.clone();
    assert!(text.len() <= NOTE_BYTES + 512, "{}", text.len());
    assert!(text.contains("earlier lines dropped]"), "{}", &text[..300]);
    assert!(text.contains("199 x") && !text.contains("000 x"));
    let inner = shared.inner.lock().unwrap();
    assert!(end_note(&inner.jobs[0]).text.len() < 512);
}

#[test]
fn a_monitors_batch_moves_the_read_cursor_past_what_it_sent() {
    let (shared, _) = bare_monitor(0, 0, None);
    for n in 0..3 {
        shared.append("job-test", &OutputLine::Out(format!("line {n}\n")));
    }
    shared.monitor_tick("job-test");
    let inner = shared.inner.lock().unwrap();
    assert_eq!(inner.jobs[0].cursor, 3);
}

#[tokio::test]
async fn cancelling_after_the_end_note_returns_the_marker_not_the_output_again() {
    let (jobs, sink) = jobs();
    let id = start(&jobs, "echo quick", true);
    until("the note", || !sink.notes().is_empty()).await;
    let read = jobs.cancel(&id).await.unwrap();
    assert!(read.already_reported);
    assert!(read.chunk.is_empty());
}

#[tokio::test]
async fn cancelling_a_monitor_does_not_return_lines_it_already_sent() {
    let (jobs, sink) = jobs();
    let id = monitor(&jobs, "echo l1; sleep 30", Duration::from_secs(20));
    until("the batch", || !sink.notes().is_empty()).await;
    let read = jobs.cancel(&id).await.unwrap();
    assert!(read.chunk.is_empty(), "{}", lines(&read.chunk));
    assert!(read.end.is_some() && !read.already_reported);
}

#[tokio::test]
async fn a_dropped_call_leaves_no_record_whichever_stop_came_first() {
    let (jobs, _) = jobs();
    let id = start(&jobs, "sleep 30", false);
    let call = tokio::time::timeout(
        Duration::from_millis(300),
        jobs.foreground(&id, Duration::from_millis(100), None, false),
    )
    .await;
    assert!(call.is_err() || call.is_ok());
    until("the record to go", || jobs.snapshot().is_empty()).await;
}
