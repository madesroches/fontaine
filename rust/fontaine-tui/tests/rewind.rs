//! `/rewind` and Esc Esc in the terminal UI over a scripted model.

mod common;

use common::routed::{Route, Turn, start};
use common::{Ui, World, text_turn, tool_turn};
use fontaine_core::config::PermissionMode;
use ratatui::crossterm::event::KeyCode;
use serde_json::json;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

async fn bodies(server: &wiremock::MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}

/// One conversation: a prompt with a file change, then a turn open long enough to queue a
/// `/rewind` and a message behind it.
async fn replay_world() -> (wiremock::MockServer, World) {
    let server = start(vec![Route::new(
        "make a",
        vec![
            Turn::tool("c1", "write", json!({"path": "a.txt", "content": "one"})),
            Turn::text("made a"),
            Turn::tool("c2", "shell", json!({"command": "sleep 1"})),
            Turn::text("SECOND-ANSWER"),
            Turn::text("msg reply"),
        ],
    )])
    .await;
    let w = World::routed_known(
        &server.uri(),
        "\n[permissions]\nallow = [\"shell(sleep *)\"]\n",
    )
    .in_mode(PermissionMode::AcceptEdits);
    (server, w)
}

/// Drives the conversation up to the rewind picker open in replay, with a message pending
/// behind it.
async fn open_queued_rewind(ui: &Ui) {
    ui.send("make a");
    ui.wait_for("made a").await;
    idle(ui).await;
    ui.send("rewind me second");
    ui.wait_for("• shell sleep 1").await;
    ui.send("/rewind");
    ui.wait_for("queued: /rewind").await;
    ui.send("marker message");
    ui.wait_for("queued: marker message").await;
    ui.wait_for("SECOND-ANSWER").await;
    // The queued /rewind opens the picker; nothing runs underneath it.
    ui.wait_for("rewind to the start of a prompt").await;
}

fn editing_world() -> World {
    World::new(vec![
        tool_turn("c1", "write", json!({"path": "a.txt", "content": "one"})),
        text_turn("made a"),
    ])
    .in_mode(PermissionMode::AcceptEdits)
}

#[tokio::test(flavor = "multi_thread")]
async fn escape_twice_opens_the_picker_and_a_full_rewind_restores_code_and_prompt() {
    let w = editing_world();
    let file = w.work.join("a.txt");
    w.run(|ui| {
        let file = file.clone();
        async move {
            ui.send("make a");
            ui.wait_for("made a").await;
            idle(&ui).await;
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "one");

            ui.press(KeyCode::Esc);
            ui.press(KeyCode::Esc);
            ui.wait_for("rewind to the start of a prompt").await;
            let live = ui.live();
            assert!(live.contains("make a  (1 file)"), "{live}");
            ui.press(KeyCode::Enter);
            ui.wait_for("Restore code and conversation").await;
            ui.press(KeyCode::Enter);
            ui.wait_for("files deleted").await;
            ui.press(KeyCode::Enter);
            ui.wait_for("rewound the conversation").await;
            idle(&ui).await;
            assert!(!file.exists());
            // The prompt is back in the input box for editing.
            let live = ui.live();
            assert!(live.contains("make a"), "{live}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_rewind_suspends_replay_until_the_picker_closes() {
    let (server, w) = replay_world().await;
    let server_ref = &server;
    w.run(|ui| async move {
        open_queued_rewind(&ui).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let asked = bodies(server_ref).await;
        assert_eq!(
            asked.len(),
            4,
            "no further model request while the picker is open: {asked:?}"
        );
        // Esc closes the picker; the pending message then runs.
        ui.press(KeyCode::Esc);
        ui.wait_for("msg reply").await;
        idle(&ui).await;
    })
    .await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 5, "{sent:?}");
    let last = sent.last().unwrap();
    assert!(last.contains("marker message"), "{last}");
    // The conversation is intact: the rewound prompt is still in it.
    assert!(last.contains("rewind me second"), "{last}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_rewind_restoring_the_conversation_runs_the_pending_message_against_it() {
    let (server, w) = replay_world().await;
    w.run(|ui| async move {
        open_queued_rewind(&ui).await;
        // The newest prompt ("rewind me second") is selected; restore the conversation only.
        ui.press(KeyCode::Enter);
        ui.wait_for("Restore code and conversation").await;
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Enter);
        ui.wait_for("rewound the conversation").await;
        ui.wait_for("msg reply").await;
        idle(&ui).await;
    })
    .await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 5, "{sent:?}");
    let last = sent.last().unwrap();
    assert!(last.contains("marker message"), "{last}");
    // The message ran against the rewound conversation: neither the rewound-away prompt nor
    // its answer is in the request.
    assert!(!last.contains("rewind me second"), "{last}");
    assert!(!last.contains("SECOND-ANSWER"), "{last}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_and_its_alias_open_the_picker_and_never_mind_changes_nothing() {
    let w = editing_world();
    let file = w.work.join("a.txt");
    w.run(|ui| {
        let file = file.clone();
        async move {
            ui.send("/rewind");
            ui.wait_for("no prompt to rewind to yet").await;
            ui.send("make a");
            ui.wait_for("made a").await;
            idle(&ui).await;
            ui.send("/checkpoint");
            ui.wait_for("rewind to the start of a prompt").await;
            ui.press(KeyCode::Enter);
            ui.wait_for("Never mind").await;
            ui.press(KeyCode::Up);
            ui.press(KeyCode::Enter);
            idle(&ui).await;
            assert!(file.exists());
        }
    })
    .await;
}
