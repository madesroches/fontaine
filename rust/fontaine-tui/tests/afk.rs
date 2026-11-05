//! AFK mode in the terminal UI: Shift+Tab walks into and out of it, the report and review, the
//! exclusion with `/guardian-disable`, a blocked session, and a temperature stop that ends the
//! turn, pauses the goal, refuses typed prompts and waits for `/afk review`.

mod common;

use common::{Ui, World, text_turn, tool_turn, verdict};
use fontaine_core::config::PermissionMode;
use ratatui::crossterm::event::KeyCode;
use serde_json::json;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn shell_turn(id: &str, command: &str) -> Vec<rig_core::test_utils::MockStreamEvent> {
    tool_turn(id, "shell", json!({ "command": command }))
}

fn deny() -> serde_json::Value {
    verdict("deny", "Git Destructive", 0.9)
}

/// The mode label the status bar shows last.
fn status_line(ui: &Ui) -> String {
    ui.screen().lines().last().unwrap_or("").to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_tab_enters_afk_reports_and_leaves_into_manual_with_a_review() {
    let w = World::new(Vec::new()).in_mode(PermissionMode::Auto);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        let screen = ui.screen();
        assert!(screen.contains("shift+tab leaves it"), "{screen}");
        assert!(!screen.contains("not in auto mode"), "{screen}");
        ui.wait_until("the label", |s| {
            s.lines().last().unwrap_or("").contains("AFK mode on")
        })
        .await;

        ui.send("/afk report");
        ui.wait_for("AFK report").await;
        assert!(ui.screen().contains("no calls were made"));

        ui.send("/afk");
        ui.wait_for("/afk no longer toggles AFK: Shift+Tab").await;
        ui.send("/afk off");
        ui.wait_for("usage: /afk report [all], /afk review").await;

        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK: ended.").await;
        ui.wait_until("the label gone", |s| {
            !s.lines().last().unwrap_or("").contains("AFK mode on")
        })
        .await;
        assert!(status_line(&ui).contains("manual"), "{}", ui.screen());
    })
    .await;
    let ledger = w.ledger_text();
    assert!(ledger.contains("\"kind\":\"afk\",\"on\":true"), "{ledger}");
    assert!(ledger.contains("\"kind\":\"afk\",\"on\":false"), "{ledger}");
    assert!(ledger.contains("\"to\":\"afk\""), "{ledger}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cycle_walks_all_five_modes_and_a_rebound_key_walks_the_same_one() {
    let w = World::new(Vec::new()).with_config("[keybindings]\n\"f2\" = \"cycle_mode\"\n");
    w.run(|ui| async move {
        idle(&ui).await;
        for expected in [
            "plan mode on",
            "accept edits on",
            "auto mode on",
            "AFK mode on",
        ] {
            ui.press(KeyCode::F(2));
            ui.wait_until(expected, |s| {
                s.lines().last().unwrap_or("").contains(expected)
            })
            .await;
        }
        ui.press(KeyCode::BackTab);
        ui.wait_until("manual", |s| {
            let last = s.lines().last().unwrap_or("");
            last.contains("manual") && !last.contains("AFK mode on")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_entry_shows_the_error_and_lands_in_manual() {
    let w = World::new(Vec::new()).in_mode(PermissionMode::Auto);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/guardian-disable");
        ui.wait_for_live_prompt("Turn the guardian off for this session?")
            .await;
        ui.type_text("y");
        ui.wait_for("guardian off for this session").await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("run /guardian-enable first").await;
        ui.wait_until("manual", |s| {
            let last = s.lines().last().unwrap_or("");
            last.contains("manual") && !last.contains("AFK mode on")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn afk_and_the_guardian_escape_hatch_refuse_each_other() {
    let w = World::new(Vec::new()).in_mode(PermissionMode::Auto);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        ui.send("/guardian-disable");
        ui.wait_for("leave it with the mode-cycle key").await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK: ended.").await;

        ui.send("/guardian-disable");
        ui.wait_for_live_prompt("Turn the guardian off for this session?")
            .await;
        ui.type_text("y");
        ui.wait_for("guardian off for this session").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_out_of_afk_reviews_the_stretch() {
    let w = World::new(Vec::new()).in_mode(PermissionMode::Auto);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        ui.send("/plan");
        ui.wait_for("plan mode on").await;
        ui.wait_for("AFK: ended.").await;
        ui.wait_until("the plan label", |s| {
            s.lines().last().unwrap_or("").contains("plan mode on")
        })
        .await;
    })
    .await;
    assert!(w.ledger_text().contains("\"source\":\"plan\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_afk_during_a_turn_reviews_the_stretch_when_the_turn_ends() {
    let w = World::new(vec![shell_turn("c1", "sleep 2"), text_turn("finished")])
        .in_mode(PermissionMode::Auto)
        .with_guardian(vec![verdict("allow", "allow", 0.95)]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        ui.send("slow work");
        ui.wait_until("the turn running", |s| s.contains("sleep 2"))
            .await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode off; the stretch is reviewed when this work ends")
            .await;
        ui.wait_for("finished").await;
        ui.wait_for("AFK: ended.").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn entering_afk_with_an_approval_open_denies_it_and_opens_no_overlay() {
    let w = World::new(vec![
        shell_turn("c1", "python x.py"),
        text_turn("denied, so stopped"),
    ]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("run it");
        ui.wait_for_prompt("python x.py").await;
        // manual -> plan -> accept edits -> auto -> AFK
        for _ in 0..4 {
            ui.press(KeyCode::BackTab);
        }
        ui.wait_for("denied, so stopped").await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("AFK mode on"), "{screen}");
        assert!(screen.contains("open prompt(s) answered"), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_temperature_stop_cuts_the_turn_refuses_prompts_and_waits_for_a_review() {
    let w = World::new(vec![
        shell_turn("c1", "python x.py"),
        text_turn("back to work"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![deny()])
    .with_config("[afk]\ntemperature_ceiling = 0.5\n");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        ui.send("run it");
        ui.wait_for("AFK: stopped").await;
        ui.wait_until("the stopped indicator", |s| s.contains("AFK STOPPED"))
            .await;
        idle(&ui).await;
        assert!(ui.screen().contains("run /afk review"), "{}", ui.screen());

        // A typed prompt is refused and goes back to the input; nothing reaches the model.
        ui.send("keep going");
        ui.wait_for("the session is stopped (AFK temperature ceiling); run /afk review")
            .await;
        // The refused text is back in the input, ready to send after the review.
        ui.wait_until("the text back in the input", |s| s.contains("│keep going"))
            .await;
        ui.press_with(
            ratatui::crossterm::event::KeyCode::Char('u'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        ui.send("/afk report");
        ui.wait_for("AFK report").await;
        assert!(ui.screen().contains("outcome: stopped"), "{}", ui.screen());

        ui.send("/afk review");
        ui.wait_for_live_prompt("Let the stopped session continue?")
            .await;
        ui.type_text("y");
        ui.wait_for("reviewed: the session can run again, now in manual mode")
            .await;
        ui.wait_until("the indicator gone", |s| !s.contains("AFK STOPPED"))
            .await;

        ui.send("welcome back");
        ui.wait_for("back to work").await;
    })
    .await;
    assert_eq!(
        w.requests().len(),
        2,
        "one before the stop, one after the review"
    );
    let ledger = w.ledger_text();
    assert!(ledger.contains("\"kind\":\"afk_stop\""), "{ledger}");
    assert!(ledger.contains("\"kind\":\"afk_reviewed\""), "{ledger}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_pauses_the_goal_and_starts_no_evaluation_or_turn() {
    let w = World::new(vec![
        shell_turn("c1", "python x.py"),
        text_turn("must not be asked for"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_config("[afk]\ntemperature_ceiling = 0.5\n")
    .with_guardian(vec![deny()]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        ui.send("/goal finish the work");
        ui.wait_for("Goal paused: the AFK temperature ceiling stopped the session")
            .await;
        idle(&ui).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!ui.screen().contains("Goal achieved"), "{}", ui.screen());
    })
    .await;
    assert_eq!(w.requests().len(), 1);
    let evaluations = w
        .guardian_requests()
        .iter()
        .filter(|r| r.contains("Goal condition:"))
        .count();
    assert_eq!(evaluations, 0, "a stopped goal is not evaluated");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_session_announces_it_pauses_the_goal_and_starts_no_turn() {
    let deny = verdict("deny", "Git Destructive", 0.9);
    let w = World::new(vec![
        shell_turn("c1", "git push --force one"),
        shell_turn("c2", "git push --force two"),
        shell_turn("c3", "git push --force three"),
        text_turn("blocked, reporting"),
        text_turn("a goal turn that must not start"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![deny]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("AFK mode on").await;
        ui.send("/goal push the branch");
        ui.wait_for("AFK: blocked — repeated denials paused auto mode")
            .await;
        ui.wait_for("Goal paused: the denial breaker tripped while the user was away")
            .await;
        idle(&ui).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    })
    .await;
    assert_eq!(w.requests().len(), 4, "no goal turn followed the report");
}
