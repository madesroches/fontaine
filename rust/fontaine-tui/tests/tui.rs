mod common;

use common::{Ui, World, text_turn, tool_turn};
use ratatui::crossterm::event::KeyCode;
use serde_json::json;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allowing_a_write_runs_the_tool() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("all done"),
    ]);
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for("all done").await;
        idle(&ui).await;
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(w.work.join("notes.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn denying_a_write_tells_the_model_and_writes_nothing() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("understood"),
    ]);
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        ui.press(KeyCode::Char('n'));
        ui.wait_for("understood").await;
        let screen = ui.screen();
        assert!(screen.contains("denied"), "{screen}");
        idle(&ui).await;
    })
    .await;
    assert!(!w.work.join("notes.txt").exists());
    let requests = w.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("denied"), "{}", requests[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denial_reason_reaches_the_model() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("understood"),
    ]);
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        for _ in 0..3 {
            ui.press(KeyCode::Down);
        }
        ui.press(KeyCode::Enter);
        ui.wait_for("Reason").await;
        ui.type_text("use the docs folder");
        ui.press(KeyCode::Enter);
        ui.wait_for("understood").await;
    })
    .await;
    assert!(!w.work.join("notes.txt").exists());
    assert!(w.requests()[1].contains("use the docs folder"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_tab_cycles_the_mode_and_the_status_bar_follows() {
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        assert!(ui.screen().contains("manual"));
        ui.press(KeyCode::BackTab);
        ui.wait_for("⏵⏵ accept edits on").await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("⏸ plan mode on").await;
        ui.press(KeyCode::BackTab);
        ui.wait_until("manual again", |s| {
            s.contains("manual") && !s.contains("plan mode on") && !s.contains("accept edits")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn add_dir_grants_the_next_read_without_a_prompt_and_keeps_the_conversation() {
    let w = World::scripted(|w| {
        vec![
            text_turn("first reply"),
            tool_turn("c1", "read", json!({"path": w.extra.join("shared.txt")})),
            text_turn("read it"),
        ]
    });
    std::fs::write(w.extra.join("shared.txt"), "SHARED-CONTENT\n").unwrap();
    let extra = w.extra.clone();
    w.run(|ui| async move {
        ui.send("remember this");
        ui.wait_for("first reply").await;
        ui.send(&format!("/add-dir {}", extra.display()));
        ui.wait_for("added workspace root").await;
        ui.send("now read it");
        ui.wait_for("read it").await;
        idle(&ui).await;
        assert!(!ui.screen().contains("Allow read?"));
    })
    .await;
    let requests = w.requests();
    let last = requests.last().unwrap();
    assert!(last.contains("remember this"), "{last}");
    assert!(last.contains("first reply"), "{last}");
    assert!(last.contains("SHARED-CONTENT"), "{last}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_command_that_is_not_one_simple_command_offers_only_once_or_deny() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "ls; echo hi"})),
        text_turn("ok"),
    ]);
    w.run(|ui| async move {
        ui.send("list things");
        ui.wait_for("Allow shell?").await;
        let screen = ui.screen();
        assert!(screen.contains("allow once"), "{screen}");
        assert!(screen.contains("deny"), "{screen}");
        assert!(!screen.contains("allow session"), "{screen}");
        assert!(!screen.contains("always allow"), "{screen}");
        ui.press(KeyCode::Char('n'));
        ui.wait_for("ok").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_prompt_shows_the_exact_rule_for_the_lasting_answers() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("ok"),
    ]);
    let rule = format!("write(/{})", w.work.join("notes.txt").display());
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        let screen = ui.screen();
        assert!(screen.contains("allow session"), "{screen}");
        assert!(screen.contains("always allow"), "{screen}");
        assert!(screen.contains(&rule), "{rule}\n{screen}");
        ui.press(KeyCode::Char('n'));
        ui.wait_for("ok").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_session_covers_the_next_identical_call() {
    let call = json!({"path": "notes.txt", "content": "hello"});
    let w = World::new(vec![
        tool_turn("c1", "write", call.clone()),
        tool_turn("c2", "write", call),
        text_turn("finished"),
    ]);
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        ui.press(KeyCode::Char('s'));
        ui.wait_for("finished").await;
        assert!(!ui.screen().contains("Allow write?"));
    })
    .await;
    assert!(w.work.join("notes.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn always_allow_writes_the_rule_to_the_user_file_and_covers_the_next_call() {
    let call = json!({"path": "notes.txt", "content": "hello"});
    let w = World::new(vec![
        tool_turn("c1", "write", call.clone()),
        tool_turn("c2", "write", call),
        text_turn("finished"),
    ]);
    std::fs::write(
        &w.user_config,
        "# keep me\n[permissions]\ndeny = [\"shell(sudo)\"]\n",
    )
    .unwrap();
    let rule = format!("write(/{})", w.work.join("notes.txt").display());
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        ui.press(KeyCode::Char('a'));
        ui.wait_for("finished").await;
        assert!(!ui.screen().contains("Allow write?"));
    })
    .await;
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.contains("# keep me"), "{saved}");
    assert!(saved.contains("deny = [\"shell(sudo)\"]"), "{saved}");
    assert!(saved.contains(&rule), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_a_pending_prompt_denies_it_and_keeps_the_session_usable() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("after"),
    ]);
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for("Allow write?").await;
        ui.press(KeyCode::Esc);
        ui.wait_for("interrupted").await;
        idle(&ui).await;
        assert!(!ui.screen().contains("Allow write?"));
        ui.send("are you there");
        ui.wait_for("after").await;
    })
    .await;
    assert!(!w.work.join("notes.txt").exists());
    let requests = w.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].contains("c1"), "{}", requests[1]);
    assert!(requests[1].contains("are you there"));
}
