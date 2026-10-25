//! The task list panel and its plain-line form for screen readers.

mod common;

use common::{Ui, World, text_turn, tool_turn};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde_json::json;

fn list() -> serde_json::Value {
    json!({"todos": [
        {"content": "read the code", "status": "completed"},
        {"content": "write the fix", "status": "in_progress"},
        {"content": "run the tests", "status": "pending"},
    ]})
}

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_list_shows_collapsed_and_ctrl_o_and_todos_expand_it() {
    let w = World::new(vec![
        tool_turn("c1", "todo_write", list()),
        text_turn("planned"),
    ]);
    w.run(|ui| async move {
        ui.send("plan it");
        ui.wait_for("planned").await;
        idle(&ui).await;
        let live = ui.live();
        assert!(live.contains("Tasks 1/3 · ▶ write the fix"), "{live}");
        assert!(!live.contains("[~] write the fix"), "{live}");

        ui.press_with(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait_until("the expanded list", |s| s.contains("[~] write the fix"))
            .await;
        let live = ui.live();
        for row in ["[x] read the code", "[ ] run the tests"] {
            assert!(live.contains(row), "{live}");
        }

        ui.send("/todos");
        ui.wait_until("the collapsed list", |_| {
            let live = ui.live();
            live.contains("Tasks 1/3") && !live.contains("[~] write the fix")
        })
        .await;
    })
    .await;
    let saved = std::fs::read_to_string(
        w.data
            .join("sessions")
            .join(w.session_id())
            .join("todos.json"),
    )
    .unwrap();
    assert!(saved.contains("write the fix"), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_empties_the_task_list() {
    let w = World::new(vec![
        tool_turn("c1", "todo_write", list()),
        text_turn("planned"),
    ]);
    w.run(|ui| async move {
        ui.send("plan it");
        ui.wait_for("planned").await;
        ui.wait_for("Tasks 1/3").await;
        ui.send("/clear");
        ui.wait_until("no panel", |_| !ui.live().contains("Tasks 1/3"))
            .await;
    })
    .await;
    let saved = std::fs::read_to_string(
        w.data
            .join("sessions")
            .join(w.session_id())
            .join("todos.json"),
    )
    .unwrap();
    assert_eq!(saved.trim(), "[]");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_screen_reader_mode_prints_the_list_as_plain_lines() {
    let w = World::new(vec![
        tool_turn("c1", "todo_write", list()),
        text_turn("planned"),
    ]);
    let bytes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    w.run_linear(bytes.clone(), |ui| async move {
        ui.send("plan it");
        ui.wait_for("[~] write the fix").await;
        ui.wait_for("planned").await;
    })
    .await;
    let out = String::from_utf8(bytes.borrow().clone()).unwrap();
    for line in ["Tasks:", "[x] read the code", "[ ] run the tests"] {
        assert!(out.contains(line), "{line} missing from {out:?}");
    }
    assert_eq!(out.matches("[~] write the fix").count(), 1, "{out:?}");
}
