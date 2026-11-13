//! Quitting while work runs: `/exit`, `/quit` and an empty-prompt Ctrl-D end the session at once,
//! inside the final-exit budget, whatever the model, a nested agent or a hook is doing. The input
//! stays open throughout, so an end of input cannot stand in for the quit.

mod common;

use std::time::{Duration, Instant};

use common::routed::{Route, Turn, start};
use common::{Finished, Ui, World};
use fontaine_tui::{DriveOptions, DriveOutcome};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde_json::json;

const BUDGET: Duration = Duration::from_millis(1000);
/// What a loaded machine may add to the budget before the test calls the exit slow.
const TOLERANCE: Duration = Duration::from_millis(2500);

fn options() -> DriveOptions {
    DriveOptions {
        budget: BUDGET,
        on_quit: None,
    }
}

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn ctrl_d(ui: &Ui) {
    ui.press_with(KeyCode::Char('d'), KeyModifiers::CONTROL);
}

#[derive(Clone, Copy, Debug)]
enum Trigger {
    Exit,
    Quit,
    CtrlD,
}

impl Trigger {
    fn fire(self, ui: &Ui) {
        match self {
            Trigger::Exit => ui.send("/exit"),
            Trigger::Quit => ui.send("/quit"),
            Trigger::CtrlD => ctrl_d(ui),
        }
    }
}

/// Fires `trigger` and waits for `drive` to return, with the input still open the whole time.
async fn leave(ui: &Ui, finished: &Finished, trigger: Trigger) -> Duration {
    let sent = Instant::now();
    trigger.fire(ui);
    for _ in 0..1000 {
        if let Some(at) = finished.get() {
            return at.duration_since(sent);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{trigger:?} did not end the session; the screen shows:\n{}",
        ui.screen()
    );
}

async fn requests_seen(server: &wiremock::MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

async fn wait_for_request(server: &wiremock::MockServer) {
    for _ in 0..1000 {
        if requests_seen(server).await > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the model was never asked");
}

async fn quit_during_a_held_response(trigger: Trigger) {
    let server = start(vec![
        Route::new("first task", vec![Turn::text("never arrives")]).delay(0, 60_000),
        Route::new("queued behind", vec![Turn::text("must not run")]),
    ])
    .await;
    let w = World::routed(&server.uri(), "");
    let held = &server;
    let outcome = w
        .run_with(options(), |ui, finished| async move {
            idle(&ui).await;
            ui.send("first task");
            wait_for_request(held).await;
            ui.send("queued behind");
            ui.wait_for("queued: queued behind").await;
            let took = leave(&ui, &finished, trigger).await;
            assert!(took < BUDGET + TOLERANCE, "{trigger:?} took {took:?}");
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    assert_eq!(
        requests_seen(&server).await,
        1,
        "nothing ran after the quit"
    );
    assert!(
        !w.ledger_text().contains("queued behind"),
        "the queued message was neither run nor restored"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_quits_while_the_model_is_still_answering() {
    quit_during_a_held_response(Trigger::Exit).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn quit_quits_while_the_model_is_still_answering() {
    quit_during_a_held_response(Trigger::Quit).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_d_on_an_empty_prompt_quits_while_the_model_is_still_answering() {
    quit_during_a_held_response(Trigger::CtrlD).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quit_already_buffered_behind_other_keys_is_served_first() {
    let server = start(vec![
        Route::new("first task", vec![Turn::text("never arrives")]).delay(0, 60_000),
        Route::new("typed after", vec![Turn::text("must not run")]),
    ])
    .await;
    let w = World::routed(&server.uri(), "");
    let held = &server;
    let outcome = w
        .run_with(options(), |ui, finished| async move {
            idle(&ui).await;
            ui.send("first task");
            wait_for_request(held).await;
            // All of it is in the channel before the interface looks again.
            let sent = Instant::now();
            ctrl_d(&ui);
            ui.send("typed after");
            for _ in 0..1000 {
                if let Some(at) = finished.get() {
                    assert!(at.duration_since(sent) < BUDGET + TOLERANCE);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the quit was not served");
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    assert_eq!(requests_seen(&server).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_d_with_text_in_the_prompt_still_edits_while_a_turn_runs() {
    let server = start(vec![
        Route::new("first task", vec![Turn::text("done at last")]).delay(0, 1500),
    ])
    .await;
    let w = World::routed(&server.uri(), "");
    let held = &server;
    let outcome = w
        .run_with(options(), |ui, finished| async move {
            idle(&ui).await;
            ui.send("first task");
            wait_for_request(held).await;
            ui.type_text("ab");
            ui.press(KeyCode::Left);
            ctrl_d(&ui);
            ui.wait_for("done at last").await;
            assert!(finished.get().is_none(), "Ctrl-D with text is not a quit");
            idle(&ui).await;
            assert!(ui.live().contains('a'), "{}", ui.live());
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_still_only_interrupts_a_running_turn() {
    let server = start(vec![
        Route::new("first task", vec![Turn::text("never arrives")]).delay(0, 60_000),
    ])
    .await;
    let w = World::routed(&server.uri(), "");
    let held = &server;
    w.run_with(options(), |ui, finished| async move {
        idle(&ui).await;
        ui.send("first task");
        wait_for_request(held).await;
        ui.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
        ui.wait_for("interrupted").await;
        idle(&ui).await;
        assert!(finished.get().is_none(), "an interrupt is not a quit");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_prompt_quits_within_the_budget() {
    let w = World::new(Vec::new());
    let outcome = w
        .run_with(options(), |ui, finished| async move {
            idle(&ui).await;
            let took = leave(&ui, &finished, Trigger::Exit).await;
            assert!(took < BUDGET + TOLERANCE, "took {took:?}");
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nested_agent_that_never_reaches_a_stopping_point_does_not_hold_the_exit() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "worker", "prompt": "child-A: wait"}),
                ),
                Turn::text("launched"),
            ],
        ),
        // The child's request is never answered, so it cannot reach a hook boundary to be
        // killed gracefully, which an ordinary exit would wait thirty-five seconds for.
        Route::new("child-A", vec![Turn::text("never arrives")]).delay(0, 120_000),
    ])
    .await;
    let w = World::routed(&server.uri(), "\n[permissions]\nallow = [\"agent\"]\n");
    let held = &server;
    let outcome = w
        .run_with(options(), |ui, finished| async move {
            idle(&ui).await;
            ui.send("root task");
            ui.wait_for("launched").await;
            idle(&ui).await;
            wait_for_requests(held, 3).await;
            let took = leave(&ui, &finished, Trigger::Exit).await;
            assert!(took < BUDGET + TOLERANCE, "took {took:?}");
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    // No report was made up for the child: what it journaled is all there is.
    assert!(
        !w.ledger_text().contains("\"status\":\"killed\""),
        "{}",
        w.ledger_text()
    );
}

async fn wait_for_requests(server: &wiremock::MockServer, at_least: usize) {
    for _ in 0..1000 {
        if requests_seen(server).await >= at_least {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the model was asked {} times", requests_seen(server).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_stop_hook_does_not_hold_the_exit() {
    let server = start(vec![Route::new("first task", vec![Turn::text("all done")])]).await;
    let w = World::routed(
        &server.uri(),
        "\n[[hooks.stop]]\ncommand = '''\nsleep 60\n'''\n",
    );
    let outcome = w
        .run_with(options(), |ui, finished| async move {
            idle(&ui).await;
            ui.send("first task");
            ui.wait_for("all done").await;
            // The stop hook is now running; the quit does not wait for it.
            let took = leave(&ui, &finished, Trigger::Exit).await;
            assert!(took < BUDGET + TOLERANCE, "took {took:?}");
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    assert_eq!(requests_seen(&server).await, 1, "no stop-hook continuation");
}
