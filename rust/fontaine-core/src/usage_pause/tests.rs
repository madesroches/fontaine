use super::*;
use crate::RuntimeError;
use crate::quota::QuotaWindow;
use futures::future::BoxFuture;

fn info(reset_in: Option<Duration>) -> PauseInfo {
    PauseInfo::new(
        UsageRole::Main,
        "work",
        reset_in.map(|d| SystemTime::now() + d),
    )
}

#[tokio::test(start_paused = true)]
async fn nothing_waits_when_nothing_is_paused() {
    let pause = UsagePause::new();
    assert_eq!(pause.wait_open().await, None);
    assert_eq!(pause.total_paused(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn engaging_twice_keeps_the_first_pause() {
    let pause = UsagePause::new();
    pause.engage(info(Some(Duration::from_secs(600))));
    pause.engage(PauseInfo::new(UsageRole::Guardian, "other", None));
    let held = pause.info().unwrap();
    assert_eq!(held.account, "work");
    assert_eq!(held.role, UsageRole::Main);
}

#[tokio::test(start_paused = true)]
async fn a_known_reset_ends_the_pause_with_a_probe_after_the_margin() {
    let pause = UsagePause::new();
    pause.engage(info(Some(Duration::from_secs(600))));
    let first = tokio::spawn({
        let pause = pause.clone();
        async move { pause.admit().await }
    });
    let second = tokio::spawn({
        let pause = pause.clone();
        async move { pause.admit().await }
    });
    tokio::time::sleep(Duration::from_secs(601)).await;
    assert!(pause.is_paused(), "the margin has not passed");
    tokio::time::sleep(RESET_MARGIN).await;
    // Exactly one of the two waiters was let through, holding the probe.
    let mut winners = 0;
    for task in [first, second] {
        if task.is_finished() {
            let admission = task.await.unwrap();
            assert_eq!(admission.end, Some(PauseEnd::Probed));
            assert!(admission.probe.is_some());
            winners += 1;
            admission.probe.unwrap().succeeded();
        }
    }
    assert_eq!(winners, 1);
    assert!(!pause.is_paused());
}

#[tokio::test(start_paused = true)]
async fn a_successful_probe_releases_every_waiter() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    let waiters: Vec<_> = (0..3)
        .map(|_| {
            let pause = pause.clone();
            tokio::spawn(async move { pause.wait_open().await })
        })
        .collect();
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    pause.note_success();
    for waiter in waiters {
        assert!(waiter.await.unwrap().is_some());
    }
    assert!(!pause.is_paused());
}

#[tokio::test(start_paused = true)]
async fn a_probe_that_meets_the_limit_again_pauses_on_with_the_new_reset() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    let admission = pause.admit().await;
    assert!(admission.probe.is_some());
    admission.probe.unwrap().detach();
    let later = SystemTime::now() + Duration::from_secs(3600);
    pause.engage(PauseInfo::new(UsageRole::Main, "work", Some(later)));
    let held = pause.info().expect("still paused");
    assert_eq!(held.reset_at, Some(later));
    assert!(pause.is_paused());
}

#[tokio::test(start_paused = true)]
async fn a_dropped_probe_token_lets_the_pause_choose_another_caller() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    let first = pause.admit().await;
    assert!(first.probe.is_some());
    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(600), pause.admit())
        .await
        .expect("a new prober is chosen");
    assert!(second.probe.is_some());
}

#[tokio::test(start_paused = true)]
async fn unknown_reset_probes_back_off_to_the_ceiling() {
    assert_eq!(backoff(0), Duration::from_secs(60));
    assert_eq!(backoff(1), Duration::from_secs(120));
    assert_eq!(backoff(4), Duration::from_secs(900));
    assert_eq!(backoff(30), MAX_PROBE_INTERVAL);
}

#[tokio::test(start_paused = true)]
async fn the_deadline_ends_the_pause_as_a_failure() {
    let pause = UsagePause::new();
    pause.engage(info(None).with_deadline(Some(Duration::from_secs(120))));
    let waiter = tokio::spawn({
        let pause = pause.clone();
        async move { pause.wait_open().await }
    });
    // The waiter is let through as the probe at a minute and never answers; the deadline ends it.
    assert_eq!(waiter.await.unwrap(), Some(PauseEnd::Probed));
    tokio::time::sleep(Duration::from_secs(300)).await;
    let (end, held) = pause.failure().expect("the end is kept");
    assert_eq!(end, PauseEnd::CapReached);
    assert_eq!(held.account, "work");
}

#[tokio::test(start_paused = true)]
async fn the_deadline_wins_when_no_one_is_asking() {
    let pause = UsagePause::new();
    pause.engage(info(None).with_deadline(Some(Duration::from_secs(30))));
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert!(!pause.is_paused());
    assert_eq!(
        pause.failure().map(|(end, _)| end),
        Some(PauseEnd::CapReached)
    );
}

#[tokio::test(start_paused = true)]
async fn a_zero_deadline_ends_at_once() {
    let pause = UsagePause::new();
    pause.engage(info(None).with_deadline(Some(Duration::ZERO)));
    assert_eq!(pause.wait_open().await, Some(PauseEnd::CapReached));
}

#[tokio::test(start_paused = true)]
async fn cancel_and_resume_release_the_waiters() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    let waiter = tokio::spawn({
        let pause = pause.clone();
        async move { pause.wait_open().await }
    });
    tokio::task::yield_now().await;
    pause.cancel();
    assert_eq!(waiter.await.unwrap(), Some(PauseEnd::Cancelled));
    assert!(pause.failure().is_some());
    // The end stays in force for every later call until a turn clears it.
    assert_eq!(pause.wait_open().await, Some(PauseEnd::Cancelled));
    pause.clear_failure();
    assert_eq!(pause.wait_open().await, None);

    pause.engage(info(None));
    assert!(pause.failure().is_none(), "a new pause clears the old end");
    let waiter = tokio::spawn({
        let pause = pause.clone();
        async move { pause.wait_open().await }
    });
    tokio::task::yield_now().await;
    pause.resume();
    assert_eq!(waiter.await.unwrap(), Some(PauseEnd::Resumed));
    assert!(pause.failure().is_none());
}

#[tokio::test(start_paused = true)]
async fn total_paused_counts_closed_and_open_pauses() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(pause.total_paused(), Duration::from_secs(10));
    pause.resume();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pause.total_paused(), Duration::from_secs(10));
    pause.engage(info(None));
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(pause.total_paused(), Duration::from_secs(17));
}

#[tokio::test(start_paused = true)]
async fn a_waiter_that_only_watches_leaves_the_probe_for_the_retry() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    let watcher = tokio::spawn({
        let pause = pause.clone();
        async move { pause.wait_ready().await }
    });
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    assert_eq!(watcher.await.unwrap(), Some(PauseEnd::Probed));
    assert!(pause.is_paused());
    // The retry's own gate takes the probe at once instead of waiting out a lease.
    let admission = tokio::time::timeout(Duration::from_secs(1), pause.admit())
        .await
        .expect("the probe was left open");
    admission.probe.expect("claimed by the retry").succeeded();
    assert!(!pause.is_paused());
}

#[tokio::test(start_paused = true)]
async fn a_deadline_set_later_ends_a_pause_that_had_none() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    pause.set_deadline(Duration::from_secs(30));
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert_eq!(
        pause.failure().map(|(end, _)| end),
        Some(PauseEnd::CapReached)
    );
}

#[tokio::test(start_paused = true)]
async fn an_overflowing_deadline_is_left_unset() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    pause.set_deadline(Duration::MAX);
    assert!(pause.info().unwrap().deadline.is_none());
    assert!(
        info(None)
            .with_deadline(Some(Duration::MAX))
            .deadline
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn a_dropped_waiter_does_not_strand_the_others() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    let doomed = tokio::spawn({
        let pause = pause.clone();
        async move { pause.wait_open().await }
    });
    let survivor = tokio::spawn({
        let pause = pause.clone();
        async move { pause.wait_open().await }
    });
    tokio::task::yield_now().await;
    doomed.abort();
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    pause.note_success();
    assert!(survivor.await.unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn observers_hear_both_ends() {
    let pause = UsagePause::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let seen = seen.clone();
        pause.observe(Arc::new(move |event| {
            let tag = match event {
                PauseEvent::Paused(_) => "paused".to_string(),
                PauseEvent::Resumed { end, .. } => end.as_str().to_string(),
            };
            seen.lock().unwrap().push(tag);
        }));
    }
    pause.engage(info(None));
    pause.cancel();
    assert_eq!(*seen.lock().unwrap(), ["paused", "cancelled"]);
}

#[derive(Debug)]
struct Fixed(Quota);

impl QuotaSource for Fixed {
    fn name(&self) -> &str {
        "fixed"
    }
    fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>> {
        let quota = self.0.clone();
        Box::pin(async move { Ok(quota) })
    }
}

fn window(used: f64, reset: Option<SystemTime>) -> Quota {
    Quota {
        windows: vec![QuotaWindow {
            label: "5h".into(),
            used_ratio: used,
            reset_at: reset,
        }],
    }
}

#[tokio::test(start_paused = true)]
async fn the_quota_source_gives_the_reset_and_ends_the_pause_when_there_is_room() {
    let reset = SystemTime::now() + Duration::from_secs(900);
    let source: Arc<dyn QuotaSource> = Arc::new(Fixed(window(1.0, Some(reset))));
    let pause = UsagePause::new();
    pause.engage(info(None).with_source(Some(source)));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(pause.info().unwrap().reset_at, Some(reset));

    let roomy: Arc<dyn QuotaSource> = Arc::new(Fixed(window(0.2, None)));
    let pause = UsagePause::new();
    pause.engage(info(Some(Duration::from_secs(60))).with_source(Some(roomy)));
    assert_eq!(pause.wait_open().await, Some(PauseEnd::Reset));
}

#[test]
fn the_spent_reset_is_the_soonest_of_the_spent_windows() {
    let soon = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    let late = SystemTime::UNIX_EPOCH + Duration::from_secs(99);
    let quota = Quota {
        windows: vec![
            QuotaWindow {
                label: "7d".into(),
                used_ratio: 1.2,
                reset_at: Some(late),
            },
            QuotaWindow {
                label: "5h".into(),
                used_ratio: 1.0,
                reset_at: Some(soon),
            },
            QuotaWindow {
                label: "1h".into(),
                used_ratio: 0.5,
                reset_at: Some(SystemTime::UNIX_EPOCH),
            },
        ],
    };
    assert_eq!(spent_reset(&quota), Some(soon));
}

#[tokio::test(start_paused = true)]
async fn a_repeat_pause_for_the_same_account_continues_the_backoff() {
    let pause = UsagePause::new();
    pause.engage(info(None));
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    pause.admit().await.probe.expect("the probe").succeeded();
    assert!(!pause.is_paused());
    pause.engage(info(None));
    // The second pause's first probe is not due after one minute again: it starts a step up.
    tokio::time::sleep(FIRST_PROBE_INTERVAL + Duration::from_secs(1)).await;
    let claimed = tokio::time::timeout(Duration::from_secs(1), pause.admit()).await;
    assert!(claimed.is_err(), "the probe waits for the longer interval");
    tokio::time::sleep(FIRST_PROBE_INTERVAL).await;
    assert!(pause.admit().await.probe.is_some());
}

#[test]
fn waits_read_as_hours_and_minutes() {
    assert_eq!(format_wait(Duration::from_secs(30)), "30s");
    assert_eq!(format_wait(Duration::from_secs(4 * 60)), "4m");
    assert_eq!(
        format_wait(Duration::from_secs(2 * 3600 + 14 * 60)),
        "2h 14m"
    );
    assert_eq!(format_wait(Duration::from_secs(3 * 3600)), "3h");
    assert_eq!(format_wait(Duration::from_secs(3599)), "1h");
}

#[test]
fn an_unknown_reset_is_said_so() {
    assert_eq!(
        describe_reset(None, SystemTime::UNIX_EPOCH),
        "reset time unknown"
    );
    let text = describe_reset(
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(3600)),
        SystemTime::UNIX_EPOCH,
    );
    assert!(text.ends_with("(in 1h)"), "{text}");
}
