//! `/rewind` and Esc Esc in the terminal UI over a scripted model.

mod common;

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
