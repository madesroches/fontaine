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

fn script_config(source: &str) -> String {
    format!("[statusline]\nscript = \"\"\"\n{source}\n\"\"\"\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_script_renders_the_whole_line() {
    let w = World::new(Vec::new()).with_config(&script_config(
        r#"pub fn statusline(ctx) { ctx.format("SCRIPTED {mode} · {model}") }"#,
    ));
    w.run(|ui| async move {
        ui.wait_for("SCRIPTED manual").await;
        let screen = ui.screen();
        assert!(
            !screen.contains("shift+tab mode · /add-dir"),
            "the script owns the line: {screen}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_that_errors_renders_the_default_bar() {
    let w = World::new(Vec::new()).with_config(&script_config(
        r#"pub fn statusline(ctx) { ctx.format("{no_such_name}") }"#,
    ));
    w.run(|ui| async move {
        ui.wait_for("shift+tab mode · /add-dir").await;
        let screen = ui.screen();
        assert!(screen.contains("manual"), "{screen}");
        ui.wait_for("the default status line is shown instead")
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_and_a_script_put_the_commands_line_where_the_script_asks() {
    let w = World::new(Vec::new()).with_config(&format!(
        "[statusline]\ncommand = \"echo FROM-CMD\"\n{}",
        script_config(r#"pub fn statusline(ctx) { ctx.format("<{command}> {mode}") }"#)
            .trim_start_matches("[statusline]\n")
    ));
    w.run(|ui| async move {
        ui.wait_for("<FROM-CMD> manual").await;
    })
    .await;
}

/// Prints the session's activity state from the payload on stdin.
#[cfg(target_os = "linux")]
const ACTIVITY_COMMAND: &str = "[statusline]\ncommand = '''\nsed -n 's/.*\"activity\":{[^}]*\"state\":\"\\([a-z]*\\)\".*/ACT-\\1/p'\n'''\n";

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn the_status_line_follows_the_activity_a_background_job_holds() {
    use common::routed::{Route, Turn, start};
    use ratatui::crossterm::event::KeyCode;
    use serde_json::json;

    let server = start(vec![Route::new(
        "root task",
        vec![
            Turn::tool(
                "c1",
                "shell",
                json!({"command": "sleep 120", "run_in_background": true}),
            ),
            Turn::text("job started"),
            Turn::text("saw the kill"),
        ],
    )])
    .await;
    let w = World::routed(
        &server.uri(),
        &format!(
            "\n[permissions]\nallow = [\"shell\"]\n\n[keybindings]\n\"f2\" = \"agents_panel\"\n\n{ACTIVITY_COMMAND}"
        ),
    );
    w.run(|ui| async move {
        ui.wait_for("ACT-idle").await;
        ui.send("root task");
        ui.wait_for("job started").await;
        // The turn is over, and no refresh interval is set: only the activity publish can
        // redraw the line, and the job keeps the session active.
        ui.wait_for("ACT-active").await;
        ui.press(KeyCode::F(2));
        ui.wait_for("Background jobs").await;
        ui.press(KeyCode::Char('x'));
        ui.wait_for("saw the kill").await;
        ui.wait_for("ACT-idle").await;
    })
    .await;
}
