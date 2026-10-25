//! Scheduled prompts in the terminal UI: `/loop`, the model's scheduling tools, and the rule
//! that a scheduled turn is never the user speaking. Time is the world's fake clock; the
//! interface looks at it about once a second while a task is pending.

mod common;

use std::time::Duration;

use common::{Ui, World, text_turn, tool_turn, verdict};
use fontaine_core::config::PermissionMode;
use ratatui::crossterm::event::KeyCode;
use serde_json::json;

const ALLOW_SCHEDULING: &str = "[permissions]\nallow = [\"cron_create\", \"shell(sleep *)\"]\n";

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn every_minute(prompt: &str, recurring: bool) -> serde_json::Value {
    json!({"cron": "* * * * *", "prompt": prompt, "recurring": recurring})
}

fn minutes(n: u64) -> Duration {
    Duration::from_secs(60 * n)
}

fn count(screen: &str, needle: &str) -> usize {
    screen.matches(needle).count()
}

/// The text of the guardian request's `recent_user_messages` block.
fn recent_messages(request: &str) -> String {
    let text = request.replace("\\\"", "\"").replace("\\n", "\n");
    let start = text
        .find("kind=\"recent_user_messages\">")
        .expect("the request has a recent_user_messages block");
    let body = &text[start..];
    let end = body.find("</untrusted>").expect("the block closes");
    body[..end].to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scheduled_turn_is_not_the_user_speaking() {
    let w = World::new(vec![
        tool_turn("c1", "cron_create", every_minute("RUN-THE-SUITE", false)),
        text_turn("it is scheduled"),
        tool_turn("c2", "shell", json!({"command": "echo from-the-schedule"})),
        text_turn("the schedule ran"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![verdict("allow", "allow", 0.95)]);
    let clock = w.clock.clone();
    w.run(|ui| async move {
        ui.send("check the build in a minute");
        ui.wait_for("it is scheduled").await;
        idle(&ui).await;
        clock.advance(minutes(1) + Duration::from_secs(1));
        ui.wait_for("the schedule ran").await;
        assert!(ui.screen().contains("scheduled: "), "{}", ui.screen());
    })
    .await;

    let ledger = w.ledger_text();
    assert_eq!(count(&ledger, "\"kind\":\"user_message\""), 1, "{ledger}");
    assert!(ledger.contains("\"event\":\"created\""), "{ledger}");
    assert!(ledger.contains("\"event\":\"fired\""), "{ledger}");
    let said: Vec<&str> = ledger
        .lines()
        .filter(|line| line.contains("\"kind\":\"user_message\""))
        .collect();
    assert!(
        said.iter().all(|line| !line.contains("RUN-THE-SUITE")),
        "{said:?}"
    );
    assert!(
        !ledger
            .lines()
            .any(|line| line.contains("\"kind\":\"scheduled\"") && line.contains("RUN-THE-SUITE")),
        "a scheduled record carries no prompt"
    );

    // The model read the note, and the guardian judged the scheduled turn's call on what the
    // user had typed, as it judged the call before.
    let seen = w.requests();
    assert!(
        seen[2].contains("Harness note, not the user's words: scheduled task"),
        "{}",
        seen[2]
    );
    assert!(seen[2].contains("RUN-THE-SUITE"));
    let asked = w.guardian_requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    let before = recent_messages(&asked[0]);
    let during = recent_messages(&asked[1]);
    assert_eq!(before, during);
    assert!(before.contains("check the build in a minute"), "{before}");
    assert!(!during.contains("RUN-THE-SUITE") && !during.contains("Harness note"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_that_comes_due_mid_turn_waits_and_then_fires_once() {
    let w = World::new(vec![
        tool_turn("c1", "cron_create", every_minute("tick", true)),
        text_turn("set"),
        tool_turn("c2", "shell", json!({"command": "sleep 1"})),
        text_turn("slept"),
        text_turn("ticked"),
    ])
    .with_config(ALLOW_SCHEDULING);
    let clock = w.clock.clone();
    w.run(|ui| async move {
        ui.send("tick every minute");
        ui.wait_for("set").await;
        idle(&ui).await;
        ui.send("sleep a little");
        ui.wait_for("• shell sleep 1").await;
        // Three missed runs while the turn is busy.
        clock.advance(minutes(3));
        ui.wait_for("ticked").await;
        let screen = ui.screen();
        assert!(
            screen.find("slept").unwrap() < screen.find("scheduled: ").unwrap(),
            "{screen}"
        );
        idle(&ui).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let screen = ui.screen();
        assert_eq!(count(&screen, "scheduled: "), 1, "{screen}");
    })
    .await;
    assert_eq!(w.requests().len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn loop_with_an_interval_runs_now_then_on_the_clock_until_stopped() {
    let w = World::new(vec![
        text_turn("first pass"),
        text_turn("second pass"),
        text_turn("never"),
    ]);
    let clock = w.clock.clone();
    w.run(|ui| async move {
        ui.send("/loop 1m check the build");
        ui.wait_for("first pass").await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("every minute"), "{screen}");
        assert!(ui.live().contains("⏱ 1"), "{}", ui.live());

        clock.advance(minutes(1) + Duration::from_secs(1));
        ui.wait_for("second pass").await;
        idle(&ui).await;

        ui.send("/loop list");
        ui.wait_for("set by you").await;
        ui.send("/loop stop");
        ui.wait_for("cancelled 1 scheduled task").await;
        assert!(!ui.live().contains("⏱"), "{}", ui.live());
        clock.advance(minutes(5));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!ui.screen().contains("never"));
    })
    .await;

    // The typed line is the user's message; the iterations after it are not.
    let ledger = w.ledger_text();
    assert_eq!(count(&ledger, "\"kind\":\"user_message\""), 1, "{ledger}");
    assert!(ledger.contains("/loop 1m check the build"), "{ledger}");
    let seen = w.requests();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].contains("check the build") && !seen[0].contains("Harness note"));
    assert!(seen[1].contains("Harness note, not the user's words"));
    assert!(seen[1].contains("set by you"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interval_that_does_not_divide_evenly_is_rounded_and_says_so() {
    let w = World::new(vec![text_turn("ran")]);
    w.run(|ui| async move {
        ui.send("/loop 7m poll the queue");
        ui.wait_for("ran").await;
        let screen = ui.screen();
        assert!(screen.contains("7m is not an even cadence"), "{screen}");
        assert!(screen.contains("every 5 minutes"), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn loop_without_a_prompt_is_a_usage_error_and_runs_nothing() {
    let w = World::new(vec![text_turn("never")]);
    w.run(|ui| async move {
        ui.send("/loop");
        ui.wait_for("usage: /loop").await;
        ui.send("/loop 5m");
        ui.wait_until("the second usage error", |s| {
            count(s, "/loop needs a prompt") == 1
        })
        .await;
    })
    .await;
    assert!(w.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_at_the_idle_prompt_ends_a_self_paced_loop_that_waits() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "schedule_wakeup",
            json!({"delay_seconds": 120, "reason": "waiting on CI"}),
        ),
        text_turn("waiting"),
        text_turn("never"),
    ]);
    let clock = w.clock.clone();
    w.run(|ui| async move {
        ui.send("/loop watch CI");
        ui.wait_for("waiting").await;
        idle(&ui).await;
        ui.wait_until("a pending wakeup", |_| ui.live().contains("⏱ 1"))
            .await;

        ui.send("/loop something else");
        ui.wait_for("a self-paced loop is already running").await;

        ui.press(KeyCode::Esc);
        ui.wait_for("the loop is stopped").await;
        ui.wait_until("no wakeup", |_| !ui.live().contains("⏱"))
            .await;
        clock.advance(minutes(30));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!ui.screen().contains("never"));
    })
    .await;
    assert_eq!(w.requests().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_paced_iteration_that_schedules_nothing_gets_one_more_try_then_ends() {
    let w = World::new(vec![
        text_turn("pass one"),
        text_turn("pass two"),
        text_turn("never"),
    ]);
    let clock = w.clock.clone();
    w.run(|ui| async move {
        ui.send("/loop watch CI");
        ui.wait_for("trying once more in 20 minutes").await;
        idle(&ui).await;
        assert!(ui.live().contains("⏱ 1"), "{}", ui.live());

        clock.advance(minutes(21));
        ui.wait_for("pass two").await;
        ui.wait_for("has ended").await;
        idle(&ui).await;
        assert!(!ui.live().contains("⏱"), "{}", ui.live());

        clock.advance(minutes(60));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!ui.screen().contains("never"));
    })
    .await;
    let seen = w.requests();
    assert_eq!(seen.len(), 2, "{seen:?}");
    // The note on a scheduled iteration tells the model to pace the loop.
    assert!(seen[1].contains("schedule_wakeup"), "{}", seen[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_scheduling_off_loop_says_so_and_the_tools_are_not_offered() {
    let w = World::new(vec![text_turn("hello")]).with_config("[schedule]\nenabled = false\n");
    w.run(|ui| async move {
        ui.send("/loop 5m x");
        ui.wait_for("scheduling is off").await;
        ui.send("hi");
        ui.wait_for("hello").await;
    })
    .await;
    let seen = w.requests();
    assert_eq!(seen.len(), 1);
    assert!(
        !seen[0].contains("cron_create"),
        "the tools are not registered"
    );
}
