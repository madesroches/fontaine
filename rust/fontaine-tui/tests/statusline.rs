//! The custom status line: config-to-bar wiring and the no-stall property.

mod common;

use common::World;

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_command_replaces_the_status_bar() {
    let w = World::new(Vec::new()).with_config("[statusline]\ncommand = \"echo AFK-TEST\"\n");
    w.run(|ui| async move {
        ui.wait_for("AFK-TEST").await;
        let screen = ui.screen();
        assert!(
            !screen.contains("shift+tab mode · /add-dir"),
            "the custom line replaces the idle hint: {screen}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sleep_script_does_not_stall_the_ui() {
    let w = World::new(Vec::new()).with_config("[statusline]\ncommand = \"exec sleep 10\"\n");
    w.run(|ui| async move {
        ui.wait_for("shift+tab mode · /add-dir").await;
        ui.type_text("still-typing");
        ui.wait_for("still-typing").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_interval_re_runs_the_command() {
    let w = World::new(Vec::new())
        .with_config("[statusline]\ncommand = \"date +%s\"\nrefresh_interval_secs = 1\n");
    w.run(|ui| async move {
        ui.wait_until("a numeric timestamp", |s| {
            s.lines()
                .any(|l| l.chars().filter(|c| c.is_ascii_digit()).count() >= 8)
        })
        .await;
    })
    .await;
}
