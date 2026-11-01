//! The shell directory shown in the status bar follows the model's `cd`.
#![cfg(unix)]

mod common;

use common::{Ui, World, text_turn, tool_turn, verdict};
use fontaine_core::config::PermissionMode;
use serde_json::json;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn world(turns: Vec<Vec<rig_core::test_utils::MockStreamEvent>>) -> World {
    World::new(turns)
        .with_scratch()
        .in_mode(PermissionMode::Auto)
        .with_guardian(vec![verdict("allow", "allow", 0.95)])
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cd_shows_in_the_status_bar_and_clear_resets_it() {
    let w = world(vec![
        tool_turn("c1", "shell", json!({"command": "cd sub"})),
        text_turn("moved"),
    ]);
    std::fs::create_dir_all(w.work.join("sub")).unwrap();
    w.run(|ui| async move {
        idle(&ui).await;
        assert!(ui.screen().contains("work ·"), "{}", ui.screen());
        ui.send("go to sub");
        ui.wait_for("moved").await;
        ui.wait_until("the bar to name the directory", |s| s.contains("work/sub"))
            .await;
        ui.send("/clear");
        ui.wait_until("the bar to name the root again", |s| {
            s.contains("work ·") && !s.contains("work/sub ·")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cd_out_of_the_workspace_leaves_the_bar_on_the_root() {
    let w = world(vec![
        tool_turn("c1", "shell", json!({"command": "cd sub"})),
        tool_turn("c2", "shell", json!({"command": "cd /"})),
        text_turn("moved"),
    ]);
    std::fs::create_dir_all(w.work.join("sub")).unwrap();
    w.run(|ui| async move {
        ui.send("go");
        ui.wait_for("moved").await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("shell working directory reset"), "{screen}");
        assert!(!screen.contains("work/sub ·"), "{screen}");
    })
    .await;
}
