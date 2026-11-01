//! `/goal` in the terminal UI: the verdicts between turns, the harness turns they lead to, the
//! pauses and the rule that a goal turn is never the user speaking. The evaluator runs on the
//! scripted guardian model.

mod common;

use common::routed::{Route, Turn, start};
use common::{Ui, World, text_turn, tool_turn};
use ratatui::crossterm::event::KeyCode;
use serde_json::{Value, json};

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn judged(verdict: &str, reason: &str) -> Value {
    json!({"verdict": verdict, "reason": reason})
}

fn count(screen: &str, needle: &str) -> usize {
    screen.matches(needle).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_not_met_verdict_runs_a_harness_turn_and_a_met_one_ends_the_goal() {
    let w =
        World::new(vec![text_turn("first pass"), text_turn("second pass")]).with_guardian(vec![
            judged("not_met", "the file is missing"),
            judged("met", "the file exists"),
        ]);
    w.run(|ui| async move {
        ui.send("/goal the file exists");
        ui.wait_for("Goal achieved: the file exists").await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("Goal set: the file exists"), "{screen}");
        assert!(
            screen.contains("goal: not met — the file is missing"),
            "{screen}"
        );
        assert!(screen.contains("second pass"), "{screen}");
        assert!(screen.contains("2 evaluations"), "{screen}");
        assert!(
            !screen.contains("◎ goal"),
            "the indicator is gone: {screen}"
        );
    })
    .await;

    let main = w.requests();
    assert_eq!(main.len(), 2, "{main:?}");
    assert!(
        main[0].contains(
            "The user set a goal; an evaluator will check it after each turn: the file exists"
        ),
        "{}",
        main[0]
    );
    assert!(
        main[1]
            .contains("Harness note, not the user's words: a goal you were given is not met yet"),
        "{}",
        main[1]
    );
    assert!(main[1].contains("the file is missing"));
    assert_eq!(w.guardian_calls(), 2);

    let ledger = w.ledger_text();
    let said: Vec<&str> = ledger
        .lines()
        .filter(|line| line.contains("\"kind\":\"user_message\""))
        .collect();
    assert_eq!(said.len(), 1, "{ledger}");
    assert!(said[0].contains("/goal the file exists"), "{said:?}");
    assert!(
        !said[0].contains("Harness note"),
        "a goal turn is not a user message"
    );
    for event in ["set", "met"] {
        assert!(
            ledger.contains(&format!("\"kind\":\"goal\",\"event\":\"{event}\"")),
            "{event}: {ledger}"
        );
    }
    assert!(
        !w.data
            .join("sessions")
            .join(w.session_id())
            .join("goal.json")
            .exists()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_impossible_verdict_ends_the_goal_with_its_reason() {
    let w = World::new(vec![text_turn("tried")])
        .with_guardian(vec![judged("impossible", "the file system is read-only")]);
    w.run(|ui| async move {
        ui.send("/goal write to the read-only mount");
        ui.wait_for("Goal impossible: the file system is read-only")
            .await;
        idle(&ui).await;
        assert!(!ui.screen().contains("◎ goal"));
        ui.send("/goal");
        ui.wait_for("No goal set").await;
    })
    .await;
    assert_eq!(w.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_cap_pauses_the_goal_and_a_message_resumes_the_evaluation() {
    let w = World::new(vec![
        text_turn("pass 1"),
        text_turn("pass 2"),
        text_turn("pass 3"),
        text_turn("pass 4"),
        text_turn("pass 5"),
        text_turn("pass 6"),
    ])
    .with_config("[goal]\nmax_turns = 2\n")
    .with_guardian(vec![judged("not_met", "still not there")]);
    w.run(|ui| async move {
        ui.send("/goal be done");
        ui.wait_for("Goal paused: 2 goal turns ran without it being met")
            .await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("Send a message to continue."), "{screen}");
        assert!(screen.contains("◎ goal paused"), "{screen}");
        assert_eq!(
            count(&screen, "goal: not met — still not there"),
            3,
            "{screen}"
        );
        // Nothing more runs on its own.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!ui.screen().contains("pass 4"));

        ui.send("/goal");
        ui.wait_for("last evaluation: still not there").await;
        assert!(ui.screen().contains("Goal paused:"));

        // The user's message starts a fresh stretch: two more goal turns, then the cap again.
        ui.send("keep going");
        ui.wait_for("pass 6").await;
        ui.wait_until("the cap again", |s| {
            count(s, "Goal paused: 2 goal turns") >= 3
        })
        .await;
    })
    .await;
    assert_eq!(w.requests().len(), 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_evaluator_with_no_model_pauses_the_goal_and_clear_removes_it() {
    let w = World::new(vec![text_turn("worked")])
        .with_unavailable_guardian("judge/gone", "no credential");
    w.run(|ui| async move {
        ui.send("/goal be done");
        ui.wait_for("Goal paused: the evaluator gave no verdict")
            .await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("judge/gone"), "{screen}");
        assert!(screen.contains("◎ goal paused"), "{screen}");

        ui.send("/goal clear");
        ui.wait_for("Goal cleared: be done").await;
        idle(&ui).await;
        assert!(!ui.screen().contains("◎ goal"));
        ui.send("/goal stop");
        ui.wait_until("nothing to clear", |s| count(s, "No goal set") >= 1)
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_goal_is_the_status_and_a_too_long_one_is_refused() {
    let w = World::new(Vec::new());
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/goal");
        ui.wait_for("No goal set").await;
        ui.send(&format!("/goal {}", "x".repeat(4_001)));
        ui.wait_for("the limit is 4000").await;
    })
    .await;
    assert!(w.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn escape_during_a_goal_turn_pauses_the_goal() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 5"})),
        text_turn("unreached"),
    ])
    .with_config("[permissions]\nallow = [\"shell(sleep *)\"]\n")
    .with_guardian(vec![judged("not_met", "x")]);
    w.run(|ui| async move {
        ui.send("/goal be done");
        ui.wait_for("• shell sleep 5").await;
        ui.press(KeyCode::Esc);
        ui.wait_for("Goal paused: it was interrupted").await;
        idle(&ui).await;
        assert!(ui.screen().contains("◎ goal paused"));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!ui.screen().contains("goal: not met"));
    })
    .await;
    assert_eq!(w.guardian_calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn escape_during_the_evaluation_pauses_the_goal_without_a_goal_turn() {
    // The evaluator's request quotes the whole conversation, so its route is matched first.
    let server = start(vec![
        Route::new(
            "Goal condition:",
            vec![Turn::tool(
                "g1",
                "submit",
                judged("not_met", "slow verdict"),
            )],
        )
        .delay(0, 3_000),
        Route::new("The user set a goal", vec![Turn::text("worked on it")]),
    ])
    .await;
    let w = World::routed(&server.uri(), "");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/goal be done");
        ui.wait_for("worked on it").await;
        ui.wait_for("esc to interrupt").await;
        assert!(ui.screen().contains("◎ goal 0s"), "{}", ui.screen());
        ui.press(KeyCode::Esc);
        ui.wait_for("Goal paused: it was interrupted").await;
        idle(&ui).await;
        assert!(ui.screen().contains("◎ goal paused"));
        assert!(!ui.screen().contains("goal: not met"));
    })
    .await;
}
