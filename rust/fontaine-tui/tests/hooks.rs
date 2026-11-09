//! The `notification` hook: it fires when an approval or a question starts waiting for the
//! human, once per prompt, whatever `[tui] notify` is set to.

mod common;

use common::World;
use serde_json::{Value, json};

fn touch_marker() -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "touch_marker.exe"
    } else {
        "touch_marker"
    };
    let path = dir.join(name);
    assert!(path.exists(), "{} is not built", path.display());
    path.to_string_lossy().into_owned()
}

fn lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_fires_the_notification_hook_once_even_with_notify_off() {
    let w = World::new(vec![
        common::tool_turn("c1", "shell", json!({"command": "echo hi"})),
        common::text_turn("done"),
    ]);
    let log = w.work.join("notified.jsonl");
    let w = w.with_config(&format!(
        "[tui]\nnotify = \"off\"\n\n[[hooks.notification]]\nmatcher = \"permission_prompt\"\ncommand = '''\n'{}' --append '{}'\n'''\n",
        touch_marker(),
        log.display()
    ));
    let seen = log.clone();
    w.run(|ui| async move {
        ui.wait_until("an idle prompt", |s| {
            s.contains("shift+tab mode · /add-dir")
        })
        .await;
        ui.send("run it");
        ui.wait_for_prompt("Allow shell").await;
        // Let the background hook finish and the screen redraw a few times: still one run.
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        let events = lines(&seen);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["hook_event_name"], "Notification");
        assert_eq!(events[0]["notification_type"], "permission_prompt");
        assert_eq!(events[0]["title"], "Fontaine");
        ui.press(ratatui::crossterm::event::KeyCode::Char('y'));
        ui.wait_for("done").await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(lines(&seen).len(), 1, "answering the prompt fires nothing");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_notification_hook_for_a_question_does_not_fire_for_an_approval() {
    let w = World::new(vec![
        common::tool_turn("c1", "shell", json!({"command": "echo hi"})),
        common::text_turn("done"),
    ]);
    let log = w.work.join("notified.jsonl");
    let w = w.with_config(&format!(
        "[[hooks.notification]]\nmatcher = \"elicitation_dialog\"\ncommand = '''\n'{}' --append '{}'\n'''\n",
        touch_marker(),
        log.display()
    ));
    let seen = log.clone();
    w.run(|ui| async move {
        ui.wait_until("an idle prompt", |s| {
            s.contains("shift+tab mode · /add-dir")
        })
        .await;
        ui.send("run it");
        ui.wait_for_prompt("Allow shell").await;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(lines(&seen).is_empty());
        ui.press(ratatui::crossterm::event::KeyCode::Char('y'));
        ui.wait_for("done").await;
    })
    .await;
}

fn hook_marker() -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "hook_marker.exe"
    } else {
        "hook_marker"
    };
    let path = dir.join(name);
    assert!(path.exists(), "{} is not built", path.display());
    path.to_string_lossy().into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_human_prompt_restarts_the_stop_hook_streak_without_a_prompt_hook() {
    let w = World::new(
        (0..20)
            .map(|n| common::text_turn(&format!("answer {n}")))
            .collect(),
    );
    let log = w.work.join("stops.jsonl");
    let w = w.with_config(&format!(
        "[[hooks.stop]]\ncommand = '''\n'{}' --append --exit 2 --stderr 'keep going' '{}'\n'''\n",
        hook_marker(),
        log.display()
    ));
    let seen = log.clone();
    w.run(|ui| async move {
        ui.wait_until("an idle prompt", |s| {
            s.contains("shift+tab mode · /add-dir")
        })
        .await;
        ui.send("first");
        ui.wait_for("answer 8").await;
        ui.wait_until("an idle prompt", |s| {
            s.contains("shift+tab mode · /add-dir")
        })
        .await;
        let after_first = lines(&seen).len();
        assert_eq!(after_first, 8, "the streak caps at eight");
        ui.send("second");
        ui.wait_for("answer 17").await;
        ui.wait_until("an idle prompt", |s| {
            s.contains("shift+tab mode · /add-dir")
        })
        .await;
        assert_eq!(
            lines(&seen).len(),
            16,
            "the second prompt starts a new streak"
        );
    })
    .await;
}
