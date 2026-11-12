//! A spent usage window pauses the whole run instead of denying every gated call: the guardian's
//! call stays pending, is judged again once the window reopens, and a wait that ends in failure
//! is a denial of its own kind. A 429 never becomes an allow.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::pipeline::{Options, Rig, allowed, denied_message, verdict_json};
use fontaine_core::config::PermissionMode::Auto;
use fontaine_core::gate::UsageWait;
use fontaine_core::ledger::Record;
use fontaine_core::usage_pause::PauseEnd;
use fontaine_guardian::{Guardian, Settings};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use serde_json::{Value, json};

const KIMI_WINDOW: &str = r#"{"error":{"message":"You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends.","type":"exceeded_current_quota_error"}}"#;
const THROTTLE: &str = r#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#;

fn shell(command: &str) -> Value {
    json!({"command": command})
}

/// The guardian's model answers `refusals` 429s with `body`, then allows every call. The
/// extractor retries twice, so one adjudication consumes three of them.
fn guardian_behind(body: &str, refusals: usize) -> (MockCompletionModel, Arc<Guardian>) {
    let turns = (0..refusals)
        .map(|n| {
            MockTurn::provider_response_error(
                http::StatusCode::TOO_MANY_REQUESTS,
                body,
                format!("req-{n}"),
            )
        })
        .chain(std::iter::repeat_n(
            MockTurn::tool_call("call_1", "submit", verdict_json("allow", "allow", 0.95)),
            200,
        ));
    let mock = MockCompletionModel::from_turns(turns);
    let guardian = Arc::new(Guardian::new(
        mock.clone().into(),
        "test/guardian",
        Settings::default(),
    ));
    (mock, guardian)
}

fn rig(guardian: Arc<Guardian>) -> Rig {
    let rig = Rig::with(Options::new(Auto).guardian(guardian).headless());
    rig.state.wire_usage_pause();
    rig
}

async fn until_paused(rig: &Rig) {
    for _ in 0..200 {
        if rig.state.usage_pause().is_paused() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pause never began");
}

#[tokio::test]
async fn a_spent_guardian_window_keeps_the_call_pending_and_judges_it_again_after_the_reset() {
    let (mock, guardian) = guardian_behind(KIMI_WINDOW, 3);
    let rig = rig(guardian);
    let check = rig.check("shell", shell("make all"));
    tokio::pin!(check);

    // The call is neither denied nor run while the window is spent.
    tokio::select! {
        _ = &mut check => panic!("the call was decided while the window was spent"),
        () = until_paused(&rig) => {}
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut check)
            .await
            .is_err(),
        "the call stays pending"
    );
    let info = rig.state.usage_pause().info().expect("a pause");
    assert_eq!(info.account, "test");
    assert_eq!(
        mock.request_count(),
        3,
        "the failed attempts, and nothing more"
    );

    rig.state.usage_pause().resume();
    let decision = check.await;
    assert!(allowed(&decision), "re-adjudicated after the reset");
    assert_eq!(
        mock.request_count(),
        4,
        "the guardian asked again from the start"
    );

    // Not one GuardianUnavailable denial in the audit log: the regression the issue describes.
    assert!(
        rig.audit()
            .iter()
            .all(|r| r.rule.as_deref() != Some("GuardianUnavailable")),
        "{:?}",
        rig.audit()
    );
    let records: Vec<_> = rig
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .map(|e| e.record)
        .collect();
    assert!(records.iter().any(|r| matches!(
        r,
        Record::UsagePaused { account, .. } if account == "test"
    )));
    assert!(records.iter().any(|r| matches!(
        r,
        Record::UsageResumed {
            end: PauseEnd::Resumed
        }
    )));
}

#[tokio::test]
async fn cancelling_the_wait_denies_the_pending_call_as_usage_pause_ended() {
    let (_, guardian) = guardian_behind(KIMI_WINDOW, 200);
    let rig = rig(guardian);
    let check = rig.check("shell", shell("make all"));
    tokio::pin!(check);
    tokio::select! {
        _ = &mut check => panic!("decided too early"),
        () = until_paused(&rig) => {}
    }
    rig.state.usage_pause().cancel();
    let decision = check.await;
    assert!(!allowed(&decision));
    assert!(denied_message(&decision).contains("[UsagePauseEnded]"));
    let record = rig.audit().last().cloned().unwrap();
    assert_eq!(
        record.fail_closed_reason.as_deref(),
        Some("usage_pause_ended")
    );

    // Every later call fails the same way, without asking the provider, until a turn clears it;
    // none of them is a guardian failure, so the breaker never pauses `auto`.
    for _ in 0..8 {
        let decision = rig.check("shell", shell("make all")).await;
        assert!(denied_message(&decision).contains("[UsagePauseEnded]"));
    }
    assert!(!rig.state.auto_paused());
    rig.state.usage_pause().clear_failure();
}

#[tokio::test]
async fn a_wait_that_runs_out_denies_instead_of_allowing() {
    let (_, guardian) = guardian_behind(KIMI_WINDOW, 200);
    let rig = rig(guardian);
    rig.state.set_usage_wait(UsageWait {
        cap: Some(Duration::from_millis(50)),
        afk_cap: None,
        exit: false,
    });
    let decision = rig.check("shell", shell("make all")).await;
    assert!(denied_message(&decision).contains("[UsagePauseEnded]"));
    let ended = rig.state.usage_pause().failure().expect("the end is kept");
    assert_eq!(ended.0, PauseEnd::CapReached);
}

#[tokio::test]
async fn exit_ends_the_wait_at_once() {
    let (_, guardian) = guardian_behind(KIMI_WINDOW, 200);
    let rig = rig(guardian);
    rig.state.set_usage_wait(UsageWait {
        cap: None,
        afk_cap: None,
        exit: true,
    });
    let decision = rig.check("shell", shell("make all")).await;
    assert!(denied_message(&decision).contains("[UsagePauseEnded]"));
}

#[tokio::test]
async fn a_throttle_without_a_window_is_still_an_ordinary_guardian_failure() {
    let (_, guardian) = guardian_behind(THROTTLE, 200);
    let rig = rig(guardian);
    let decision = rig.check("shell", shell("make all")).await;
    assert!(denied_message(&decision).contains("[GuardianUnavailable]"));
    assert!(!rig.state.usage_pause().is_paused());
}

#[tokio::test(start_paused = true)]
async fn a_probe_won_by_the_guardian_ends_the_pause() {
    let (_, guardian) = guardian_behind(KIMI_WINDOW, 0);
    let rig = rig(guardian);
    rig.state
        .usage_pause()
        .engage(fontaine_core::usage_pause::PauseInfo::new(
            fontaine_core::quota::UsageRole::Guardian,
            "test",
            None,
        ));
    let decision = rig.check("shell", shell("make all")).await;
    assert!(allowed(&decision));
    // Ended by the guardian's answer, not by the lease lapsing into another backoff.
    assert!(!rig.state.usage_pause().is_paused());
    assert_eq!(
        rig.state.usage_pause().failure().map(|(end, _)| end),
        None,
        "not a failure"
    );
}

#[tokio::test(start_paused = true)]
async fn turning_afk_on_gives_a_pause_without_a_deadline_the_afk_cap() {
    let (_, guardian) = guardian_behind(KIMI_WINDOW, 0);
    let rig = rig(guardian);
    rig.state.set_usage_wait(UsageWait {
        cap: None,
        afk_cap: Some(Duration::from_secs(30)),
        exit: false,
    });
    rig.state
        .usage_pause()
        .engage(fontaine_core::usage_pause::PauseInfo::new(
            fontaine_core::quota::UsageRole::Main,
            "test",
            None,
        ));
    assert!(rig.state.usage_pause().info().unwrap().deadline.is_none());
    rig.state
        .set_mode(
            fontaine_core::config::PermissionMode::Afk,
            fontaine_core::afk_report::AfkSource::Cli,
        )
        .expect("AFK starts");
    assert!(rig.state.usage_pause().info().unwrap().deadline.is_some());
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert_eq!(
        rig.state.usage_pause().failure().map(|(end, _)| end),
        Some(PauseEnd::CapReached)
    );
}
