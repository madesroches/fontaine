mod common;

use common::routed::{Route, Turn, start};
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
        ui.wait_for_prompt("Allow write?").await;
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
        ui.wait_for_prompt("Allow write?").await;
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
        ui.wait_for_prompt("Allow write?").await;
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
        ui.wait_for("⏵⏵ auto mode on").await;
        ui.press(KeyCode::BackTab);
        ui.wait_until("manual again", |s| {
            s.contains("manual")
                && !s.contains("plan mode on")
                && !s.contains("accept edits")
                && !s.contains("auto mode on")
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
        ui.wait_for_prompt("Allow shell?").await;
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
    let rule = absolute_rule("write", &w.work.join("notes.txt"));
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
        let screen = ui.screen();
        assert!(screen.contains("allow session"), "{screen}");
        assert!(screen.contains("always allow"), "{screen}");
        // A long temp path wraps inside the approval box.
        let unwrapped: String = screen.lines().map(|line| line.trim_matches('│')).collect();
        assert!(unwrapped.contains(&rule), "{rule}\n{screen}");
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
        ui.wait_for_prompt("Allow write?").await;
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
    let rule = absolute_rule("write", &w.work.join("notes.txt"));
    w.run(|ui| async move {
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
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
        ui.wait_for_prompt("Allow write?").await;
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

/// The rule an approval offers for an absolute path: `//` plus the path, which on Windows drops
/// the verbatim prefix and uses `/` separators (`//C:/...`).
fn absolute_rule(tool: &str, path: &std::path::Path) -> String {
    let text = path.display().to_string();
    let text = if cfg!(windows) {
        text.trim_start_matches(r"\\?\").replace('\\', "/")
    } else {
        text.trim_start_matches('/').to_string()
    };
    format!("{tool}(//{text})")
}

// ---------------------------------------------------------------------------------------
// auto mode: denials, retries, escalation, model switches and the unavailable guardian
// ---------------------------------------------------------------------------------------

use common::{verdict, verdict_endpoint};
use fontaine_core::config::PermissionMode;

fn ledger_text(w: &World) -> String {
    w.ledger_text()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guardian_denial_shows_its_label_and_words_inline() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "echo x > out.txt"})),
        text_turn("blocked"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![verdict("deny", "Safety Bypass", 0.95)]);
    w.run(|ui| async move {
        idle(&ui).await;
        assert!(ui.screen().contains("auto mode on"), "{}", ui.screen());
        ui.send("do it");
        ui.wait_for("blocked").await;
        let screen = ui.screen();
        assert!(screen.contains("[Safety Bypass]"), "{screen}");
        assert!(screen.contains("deny because of Safety Bypass"), "{screen}");
        // The agent's own note is not what the human reads.
        assert!(
            !screen.contains("Do not retry it or look for another route"),
            "{screen}"
        );
    })
    .await;
    assert!(!w.work.join("out.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_lists_a_denial_and_r_lets_the_agent_retry_once_past_the_guardian() {
    let call = json!({"command": "echo retry > out.txt"});
    let w = World::new(vec![
        tool_turn("c1", "shell", call.clone()),
        text_turn("blocked"),
        tool_turn("c2", "shell", call.clone()),
        tool_turn("c3", "shell", call),
        text_turn("done"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![verdict("deny", "Safety Bypass", 0.95)]);
    w.run(|ui| async move {
        ui.send("do it");
        ui.wait_for("blocked").await;
        idle(&ui).await;

        ui.send("/denied");
        ui.wait_for("Calls the guardian denied").await;
        let screen = ui.screen();
        assert!(screen.contains("echo retry > out.txt"), "{screen}");
        assert!(screen.contains("[Safety Bypass]"), "{screen}");
        ui.press(KeyCode::Char('r'));
        ui.wait_for("approved once").await;
        ui.press(KeyCode::Esc);
        ui.wait_until("the list to close", |s| {
            !s.contains("Calls the guardian denied")
        })
        .await;

        ui.send("go ahead");
        ui.wait_for("done").await;
        // Not paused: the retry fed neither side of the breaker.
        assert!(!ui.screen().contains("paused"), "{}", ui.screen());
    })
    .await;

    // The retry notice reached the agent's next turn, and only there.
    let requests = w.requests();
    assert!(requests[2].contains("approved it once"), "{}", requests[2]);
    assert!(!requests[0].contains("approved it once"));
    let ledger = ledger_text(&w);
    assert!(ledger.contains("go ahead"));
    assert!(!ledger.contains("Harness note"), "{ledger}");

    // The retry ran without the guardian, as a human decision; the identical call after it
    // reached the guardian again.
    assert_eq!(w.guardian_calls(), 2);
    let tiers: Vec<(String, String)> = w
        .audit()
        .iter()
        .map(|r| {
            (
                r["tier"].as_str().unwrap().to_string(),
                r["decision"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        tiers,
        [
            ("guardian".to_string(), "deny".to_string()),
            ("human".to_string(), "allow".to_string()),
            ("guardian".to_string(), "deny".to_string()),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(w.work.join("out.txt"))
            .unwrap()
            .trim(),
        "retry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn three_denials_in_a_row_pause_auto_with_a_notice_and_approval_resumes_it() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "echo 1 > a.txt"})),
        tool_turn("c2", "shell", json!({"command": "echo 2 > a.txt"})),
        tool_turn("c3", "shell", json!({"command": "echo 3 > a.txt"})),
        tool_turn("c4", "shell", json!({"command": "echo 4 > a.txt"})),
        text_turn("finished"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![verdict("deny", "Safety Bypass", 0.95)]);
    w.run(|ui| async move {
        ui.send("keep trying");
        ui.wait_for_prompt("Allow shell?").await;
        let screen = ui.screen();
        assert!(
            screen.contains("paused after repeated guardian denials"),
            "{screen}"
        );
        assert!(screen.contains("auto mode paused"), "{screen}");
        ui.press(KeyCode::Char('y'));
        ui.wait_for("finished").await;
    })
    .await;
    assert_eq!(w.guardian_calls(), 3, "the fourth call went to the human");
    assert_eq!(
        std::fs::read_to_string(w.work.join("a.txt"))
            .unwrap()
            .trim(),
        "4"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_escalate_verdict_opens_a_prompt_with_the_guardians_words() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "echo e > e.txt"})),
        text_turn("done"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![verdict("escalate", "Destroy Pre-existing Files", 0.6)]);
    w.run(|ui| async move {
        ui.send("tidy up");
        ui.wait_for_prompt("Allow shell?").await;
        let screen = ui.screen();
        assert!(screen.contains("[Destroy Pre-existing Files]"), "{screen}");
        assert!(!screen.contains("allow session"), "{screen}");
        ui.press(KeyCode::Char('y'));
        ui.wait_for("done").await;
    })
    .await;
    assert!(w.work.join("e.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_in_auto_lists_the_allow_rules_it_sets_aside_and_shift_tab_leaves_it() {
    let w = World::new(vec![])
        .in_mode(PermissionMode::Auto)
        .with_config("[permissions]\nallow = [\"shell(python *)\", \"shell(npm test)\"]\n")
        .with_guardian(vec![verdict("allow", "allow", 0.9)]);
    w.run(|ui| async move {
        idle(&ui).await;
        let screen = ui.screen();
        assert!(
            screen.contains("sets aside these broad allow rules"),
            "{screen}"
        );
        assert!(screen.contains("shell(python *)"), "{screen}");
        assert!(!screen.contains("npm test"), "{screen}");
        ui.press(KeyCode::BackTab);
        ui.wait_until("manual", |s| {
            s.contains("manual") && !s.contains("auto mode on")
        })
        .await;
        // Cycling back into auto lists them again.
        for _ in 0..3 {
            ui.press(KeyCode::BackTab);
        }
        ui.wait_for("auto mode on").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn model_switches_the_live_session_and_saves_the_choice() {
    let w = World::new(vec![])
        .with_config(
            "# mine\n[accounts.other]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"second\"]\n\n[permissions]\ndeny = [\"shell(sudo)\"]\n",
        )
        .guardian_follows_main();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/model other/second");
        ui.wait_for("model is now other/second").await;
        let screen = ui.screen();
        assert!(screen.contains("the guardian runs on it too"), "{screen}");
        assert!(screen.contains("saved to your settings"), "{screen}");
        // The status bar follows.
        assert!(screen.contains("· other/second ·"), "{screen}");
        ui.send("/model other/missing");
        ui.wait_for("does not offer model `missing`").await;
        ui.send("/model");
        ui.wait_until("the picker", |_| ui.live().contains("type to filter"))
            .await;
        let live = ui.live();
        assert!(
            live.contains("model other/second · guardian other/second"),
            "{live}"
        );
        assert!(live.contains("second (current)"), "{live}");
        ui.press(KeyCode::Esc);
    })
    .await;
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.starts_with("# mine\n"), "{saved}");
    assert!(saved.contains("model = \"other/second\""), "{saved}");
    assert!(saved.contains("deny = [\"shell(sudo)\"]"), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn guardian_model_pins_the_guardian_and_saves_the_choice() {
    let w = World::new(vec![])
        .with_config(
            "[accounts.cloud]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"judge-b\"]\n",
        )
        .with_guardian(vec![verdict("allow", "allow", 0.9)]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/guardian-model");
        ui.wait_until("the picker", |_| ui.live().contains("type to filter"))
            .await;
        let live = ui.live();
        assert!(live.contains("guardian judge/mock"), "{live}");
        assert!(live.contains("│cloud"), "{live}");
        assert!(live.contains("  judge-b"), "{live}");
        ui.press(KeyCode::Enter);
        ui.wait_for("guardian is now cloud/judge-b").await;
        assert!(
            ui.screen().contains("saved to your settings"),
            "{}",
            ui.screen()
        );
        ui.send("/guardian-model cloud/nope");
        ui.wait_for("does not offer model `nope`").await;
    })
    .await;
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.contains("[guardian]"), "{saved}");
    assert!(saved.contains("model = \"cloud/judge-b\""), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn picking_the_current_guardian_saves_it() {
    let config = "[accounts.cloud]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"judge-b\"]\n";
    let w = World::new(vec![])
        .with_config(config)
        .with_guardian(vec![verdict("allow", "allow", 0.9)]);
    let user_config = w.user_config.clone();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/guardian-model cloud/judge-b");
        ui.wait_for("guardian is now cloud/judge-b").await;
        // The live guardian is now one the settings file does not name, as after a session-only
        // change.
        std::fs::write(&user_config, config).unwrap();
        ui.send("/guardian-model");
        ui.wait_until("the picker", |_| ui.live().contains("judge-b (current)"))
            .await;
        ui.press(KeyCode::Enter);
        ui.wait_until("the guardian to be saved again", |screen| {
            screen.matches("guardian is now cloud/judge-b").count() == 2
        })
        .await;
    })
    .await;
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.contains("model = \"cloud/judge-b\""), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_pinned_guardian_offers_retry_another_or_manual() {
    let alt = verdict_endpoint(verdict("allow", "allow", 0.95)).await;
    let config = format!(
        "[accounts.cloud]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"flash\"]\n",
        alt.uri()
    );
    let w = World::new(vec![
        // Picking another guardian: the call is adjudicated again and runs.
        tool_turn(
            "c1",
            "shell",
            json!({"command": "echo picked > picked.txt"}),
        ),
        text_turn("first done"),
        // Retrying while it is still unavailable leaves the call denied.
        tool_turn("c2", "shell", json!({"command": "echo again > again.txt"})),
        text_turn("second done"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_config(&config)
    .with_unavailable_guardian("judge/pinned", "account `judge` has no credential");
    w.run(|ui| async move {
        ui.send("first");
        ui.wait_for_prompt("cannot be used").await;
        let screen = ui.screen();
        assert!(screen.contains("judge/pinned"), "{screen}");
        assert!(screen.contains("no credential"), "{screen}");
        assert!(screen.contains("[r] retry"), "{screen}");
        assert!(screen.contains("[p] pick another"), "{screen}");
        assert!(
            screen.contains("[m] switch this session to manual"),
            "{screen}"
        );
        ui.press(KeyCode::Char('p'));
        ui.wait_for("cloud/flash").await;
        ui.press(KeyCode::Enter);
        ui.wait_for("first done").await;
        assert!(
            ui.screen()
                .contains("guardian is now cloud/flash (saved to your settings)"),
            "{}",
            ui.screen()
        );
    })
    .await;
    assert!(w.work.join("picked.txt").exists());
    assert_eq!(alt.received_requests().await.unwrap().len(), 1);
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.contains("[guardian]"), "{saved}");
    assert!(saved.contains("model = \"cloud/flash\""), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn retrying_an_unavailable_guardian_that_is_still_down_denies_the_call() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "echo again > again.txt"})),
        text_turn("done"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_config("[accounts.judge]\nkind = \"anthropic\"\napi_key_env = \"MISSING_KEY\"\n")
    .with_unavailable_guardian("judge/pinned", "environment variable MISSING_KEY is unset");
    w.run(|ui| async move {
        ui.send("go");
        ui.wait_for_prompt("cannot be used").await;
        ui.press(KeyCode::Char('r'));
        ui.wait_for("done").await;
        let screen = ui.screen();
        assert!(screen.contains("denied"), "{screen}");
    })
    .await;
    assert!(!w.work.join("again.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_to_manual_from_the_unavailable_prompt_asks_the_human_instead() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "echo manual > m.txt"})),
        text_turn("done"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_unavailable_guardian("judge/pinned", "no credential");
    w.run(|ui| async move {
        ui.send("go");
        ui.wait_for_prompt("cannot be used").await;
        ui.press(KeyCode::Char('m'));
        ui.wait_for_prompt("Allow shell?").await;
        assert!(ui.screen().contains("manual"), "{}", ui.screen());
        ui.press(KeyCode::Char('y'));
        ui.wait_for("done").await;
    })
    .await;
    assert!(w.work.join("m.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_replies_scroll_above_the_live_area() {
    let w = World::new(vec![text_turn("first reply"), text_turn("second reply")]);
    w.run(|ui| async move {
        ui.send("one");
        ui.wait_for("first reply").await;
        idle(&ui).await;
        ui.send("two");
        ui.wait_for("second reply").await;
        idle(&ui).await;
        let screen = ui.screen();
        let first = screen.find("first reply").unwrap();
        let second = screen.find("second reply").unwrap();
        assert!(first < second, "{screen}");
        let live = ui.live();
        assert!(!live.contains("reply"), "{live}");
        assert!(live.contains("shift+tab mode"), "{live}");
    })
    .await;
}

// ---------------------------------------------------------------------------------------
// input modes: queued messages, `!` commands and history
// ---------------------------------------------------------------------------------------

const ALLOW_SLEEP: &str = "[permissions]\nallow = [\"shell(sleep *)\"]\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_message_typed_during_a_tool_call_reaches_the_model_in_its_result() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("noted"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("also check the docs");
        ui.wait_for("queued: also check the docs").await;
        assert!(ui.screen().contains("↑ to edit"), "{}", ui.screen());
        ui.wait_for("noted").await;
        idle(&ui).await;
        let screen = ui.screen();
        assert!(screen.contains("> also check the docs"), "{screen}");
        assert!(!ui.live().contains("queued:"), "{}", ui.live());
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(!requests[0].contains("also check the docs"));
    assert!(requests[1].contains("The user sent this message while you were working"));
    assert!(requests[1].contains("also check the docs"));
    let ledger = w.ledger_text();
    assert_eq!(
        ledger.matches("\"kind\":\"user_message\"").count(),
        2,
        "{ledger}"
    );
    assert!(ledger.contains("also check the docs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_message_becomes_guardian_intent_only_when_the_model_receives_it() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        tool_turn("c2", "shell", json!({"command": "echo two > two.txt"})),
        text_turn("done"),
    ])
    .in_mode(PermissionMode::Auto)
    .with_guardian(vec![verdict("allow", "allow", 0.95)]);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("also check the docs");
        ui.wait_for("done").await;
    })
    .await;
    let asked = w.guardian_requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(!asked[0].contains("also check the docs"), "{}", asked[0]);
    assert!(asked[1].contains("also check the docs"), "{}", asked[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_a_turn_puts_the_queued_message_back_in_the_input() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("never"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("not sent");
        ui.wait_for("queued: not sent").await;
        ui.press(KeyCode::Esc);
        ui.wait_for("interrupted").await;
        idle(&ui).await;
        let live = ui.live();
        assert!(live.contains("not sent"), "{live}");
        assert!(!live.contains("queued:"), "{live}");
    })
    .await;
    assert_eq!(w.requests().len(), 1);
    assert!(!w.ledger_text().contains("not sent"));
}

#[tokio::test(flavor = "multi_thread")]
async fn up_takes_a_queued_message_back_to_edit() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("done"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("first thought");
        ui.wait_for("queued: first thought").await;
        ui.press(KeyCode::Up);
        ui.wait_until("the input to hold it", |_| {
            ui.live().contains("first thought") && !ui.live().contains("queued:")
        })
        .await;
        ui.wait_for("done").await;
    })
    .await;
    assert!(!w.requests()[1].contains("first thought"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bang_command_runs_without_a_prompt_and_its_output_reaches_the_next_message() {
    let w = World::new(vec![text_turn("seen it")]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.type_text("!echo hi-from-user");
        ui.wait_for("shell · Enter runs").await;
        ui.press(KeyCode::Enter);
        ui.wait_for("! echo hi-from-user").await;
        ui.wait_until("the output", |s| s.matches("hi-from-user").count() >= 2)
            .await;
        assert!(!ui.screen().contains("Allow"), "{}", ui.screen());
        ui.send("what did it print");
        ui.wait_for("seen it").await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].contains("not the user's words"),
        "{}",
        requests[0]
    );
    assert!(requests[0].contains("$ echo hi-from-user"));
    assert!(requests[0].contains("what did it print"));

    let audit = w.audit();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0]["agent_id"], "user");
    assert_eq!(audit[0]["tool"], "shell");
    assert_eq!(audit[0]["tier"], "human");
    assert_eq!(audit[0]["decision"], "allow");
    assert_eq!(audit[0]["rule"], "user_shell");
    assert_eq!(audit[0]["headless"], false);
    let ledger = w.ledger_text();
    assert!(ledger.contains("\"kind\":\"user_shell\""), "{ledger}");
    assert!(ledger.contains("\"exit_code\":0"), "{ledger}");
    // The command is not the human's message to the agent.
    assert_eq!(
        ledger.matches("\"kind\":\"user_message\"").count(),
        1,
        "{ledger}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_bang_command_sees_the_configured_env() {
    let w =
        World::new(vec![text_turn("seen it")]).with_config("[env]\nPROBE = \"probe-value-xyz\"\n");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.type_text("!printenv PROBE");
        ui.wait_for("shell · Enter runs").await;
        ui.press(KeyCode::Enter);
        ui.wait_until("the value", |s| s.matches("probe-value-xyz").count() >= 1)
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_a_bang_command_records_it_and_tells_the_agent_nothing() {
    let w = World::new(vec![text_turn("fine")]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("!sleep 5");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        ui.press(KeyCode::Esc);
        ui.wait_for("interrupted").await;
        idle(&ui).await;
        ui.send("next");
        ui.wait_for("fine").await;
    })
    .await;
    let ledger = w.ledger_text();
    assert!(ledger.contains("\"kind\":\"user_shell\""), "{ledger}");
    assert!(ledger.contains("\"exit_code\":null"), "{ledger}");
    let audit = w.audit();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0]["rule"], "user_shell");
    let requests = w.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].contains("Harness note"), "{}", requests[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn history_persists_across_runs() {
    let w = World::new(vec![text_turn("ok")]);
    w.run(|ui| async move {
        ui.send("remember me");
        ui.wait_for("ok").await;
        idle(&ui).await;
    })
    .await;
    w.run(|ui| async move {
        idle(&ui).await;
        ui.press(KeyCode::Up);
        ui.wait_until("the recalled message", |_| {
            ui.live().contains("remember me")
        })
        .await;
    })
    .await;
    let file = std::fs::read_to_string(w.data.join("history.jsonl")).unwrap();
    assert_eq!(file.lines().count(), 1, "{file}");
    assert_ne!(w.session_id(), "00000000-0000-4000-8000-000000000001");
}

const OTHER: &str = "\n[accounts.other]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"second\", \"third\"]\n";

async fn picker_open(ui: &Ui) {
    ui.wait_until("the picker", |_| ui.live().contains("type to filter"))
        .await;
}

async fn picker_closed(ui: &Ui) {
    ui.wait_until("the picker to close", |_| {
        !ui.live().contains("type to filter")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_picker_groups_filters_switches_and_remembers() {
    let w = World::routed("http://127.0.0.1:9", OTHER);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        let live = ui.live();
        assert!(live.contains("model mock/scripted"), "{live}");
        assert!(live.contains("scripted (current)"), "{live}");
        assert!(live.contains("other"), "{live}");
        assert!(live.contains("  third"), "{live}");
        ui.type_text("sec");
        ui.wait_until("the filter", |_| !ui.live().contains("  third"))
            .await;
        let live = ui.live();
        assert!(live.contains("filter: sec"), "{live}");
        assert!(!live.contains("  scripted"), "{live}");
        ui.press(KeyCode::Enter);
        ui.wait_for("model is now other/second").await;
        assert!(ui.screen().contains("saved to your settings"));
        picker_closed(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        let live = ui.live();
        assert!(live.contains("Recent"), "{live}");
        assert!(live.contains("other/second (current)"), "{live}");
        ui.press(KeyCode::Esc);
    })
    .await;
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.contains("model = \"other/second\""), "{saved}");
    let recent = std::fs::read_to_string(w.data.join("recent_models.json")).unwrap();
    assert!(recent.contains("\"main\":[\"other/second\"]"), "{recent}");
}

#[tokio::test(flavor = "multi_thread")]
async fn escape_closes_the_model_picker_without_switching() {
    let w = World::routed("http://127.0.0.1:9", OTHER);
    let before = std::fs::read_to_string(&w.user_config).unwrap();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Esc);
        picker_closed(&ui).await;
        assert!(!ui.screen().contains("model is now"));
    })
    .await;
    assert_eq!(std::fs::read_to_string(&w.user_config).unwrap(), before);
    assert!(!w.data.join("recent_models.json").exists());
}

fn openai_models(ids: &[&str]) -> wiremock::ResponseTemplate {
    let data: Vec<_> = ids
        .iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "x"}))
        .collect();
    wiremock::ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data}))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_account_keeps_its_heading_and_only_failures_are_listed_again() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let live = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(openai_models(&["fresh"]))
        .expect(1)
        .mount(&live)
        .await;
    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(2)
        .mount(&broken)
        .await;
    let extra = format!(
        "\n[accounts.live]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[accounts.broken]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n",
        live.uri(),
        broken.uri()
    );
    let w = World::routed("http://127.0.0.1:9", &extra);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        let shown = ui.live();
        assert!(shown.contains("broken"), "{shown}");
        assert!(shown.contains("could not list:"), "{shown}");
        assert!(shown.contains("  fresh"), "{shown}");
        ui.press(KeyCode::Esc);
        picker_closed(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        assert!(ui.live().contains("  fresh"));
        ui.press(KeyCode::Esc);
        picker_closed(&ui).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_main_account_is_listed_afresh_after_a_switch_to_it() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer};
    let live = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(openai_models(&["fresh"]))
        .up_to_n_times(1)
        .mount(&live)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(openai_models(&["fresh", "newer"]))
        .mount(&live)
        .await;
    let extra = format!(
        "\n[accounts.live]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n",
        live.uri()
    );
    let w = World::routed("http://127.0.0.1:9", &extra);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        ui.type_text("fresh");
        ui.press(KeyCode::Enter);
        ui.wait_for("model is now live/fresh").await;
        picker_closed(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        let shown = ui.live();
        assert!(shown.contains("  newer"), "{shown}");
        ui.press(KeyCode::Esc);
        picker_closed(&ui).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pick_made_during_a_notification_turn_applies_when_the_turn_ends() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "worker", "prompt": "child-A: finish", "background": true}),
                ),
                Turn::text("launched"),
                Turn::text("noted the report"),
            ],
        )
        .delay(2, 3000),
        Route::new("child-A", vec![Turn::text("child done")]).delay(0, 1500),
    ])
    .await;
    let w = World::routed(
        &server.uri(),
        &format!("{OTHER}\n[permissions]\nallow = [\"agent\"]\n"),
    );
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_for("launched").await;
        idle(&ui).await;
        ui.send("/model");
        picker_open(&ui).await;
        ui.wait_for("a background agent reported back").await;
        ui.type_text("third");
        ui.press(KeyCode::Enter);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!ui.screen().contains("model is now"), "{}", ui.screen());
        ui.wait_for("model is now other/third").await;
        assert!(ui.screen().contains("noted the report"), "{}", ui.screen());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_enters_plan_mode_and_a_description_is_sent_in_that_mode() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("planned"),
    ]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plan");
        ui.wait_for("plan mode on").await;
        ui.wait_for("⏸ plan mode on").await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("⏵⏵ auto mode on").await;
        ui.press(KeyCode::BackTab);
        ui.wait_for("manual").await;
        ui.send("/plan do x");
        ui.wait_for("planned").await;
        // Plan mode's rules handled the write; manual mode would have asked.
        assert!(!ui.screen().contains("Allow write?"), "{}", ui.screen());
    })
    .await;
    assert!(w.requests()[0].contains("do x"));
    assert!(!w.work.join("notes.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn permissions_shows_the_rules_with_their_scopes() {
    let w = World::new(vec![])
        .with_config("[permissions]\ndeny = [\"shell(sudo)\"]\n")
        .with_project_settings("[permissions]\nask = [\"shell(ls)\"]\n");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/permissions");
        ui.wait_for("/denied lists what the guardian denied").await;
        let screen = ui.screen();
        assert!(screen.contains("shell(sudo) [user]"), "{screen}");
        assert!(screen.contains("shell(ls) [project]"), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_sends_the_latest_reply_to_the_clipboard() {
    let w = World::new(vec![text_turn("first reply"), text_turn("second reply")]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("one");
        ui.wait_for("first reply").await;
        idle(&ui).await;
        ui.send("two");
        ui.wait_for("second reply").await;
        idle(&ui).await;
        ui.send("/copy");
        ui.wait_for("copied reply 1").await;
        assert_eq!(ui.clipboard(), vec!["second reply".to_string()]);
        ui.send("/copy 2");
        ui.wait_for("copied reply 2").await;
        assert_eq!(ui.clipboard().last().unwrap(), "first reply");
        ui.send("/copy 9");
        ui.wait_for("no assistant reply number 9").await;
        assert_eq!(ui.clipboard().len(), 2);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_and_help_report_without_a_turn() {
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/status");
        ui.wait_for("MCP servers: 0 of 0 connected").await;
        ui.send("/help");
        ui.wait_for("esc interrupt").await;
        let screen = ui.screen();
        assert!(screen.contains("/exit, /quit"), "{screen}");
        ui.send("/hooks");
        ui.wait_for("no hooks configured").await;
        ui.send("/mcp");
        ui.wait_for("no MCP servers configured").await;
        ui.send("/mcp reconnect");
        ui.wait_for("/mcp takes no arguments").await;
    })
    .await;
    assert!(w.requests().is_empty());
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

/// Three exchanges, enough for a compaction to have something older than the newest reply.
async fn three_exchanges(ui: &Ui) {
    for (i, word) in ["one", "two", "three"].iter().enumerate() {
        ui.send(&format!("ctx-marker {word}"));
        ui.wait_for(&format!("reply {}", i + 1)).await;
        idle(ui).await;
    }
}

fn context_route() -> Route {
    Route::new(
        "ctx-marker",
        vec![
            Turn::text("reply 1"),
            Turn::text("reply 2"),
            Turn::text("reply 3"),
            // The summary carries the marker so the next request finds its route.
            Turn::text("ctx-marker THE-SUMMARY"),
            Turn::text("reply 5"),
            Turn::text("reply 6"),
        ],
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn compact_clear_and_context_work_on_the_conversation() {
    let server = start(vec![context_route()]).await;
    let w = World::routed_known(&server.uri(), "");
    w.run(|ui| async move {
        idle(&ui).await;
        three_exchanges(&ui).await;

        ui.send("/context");
        ui.wait_for("messages").await;
        let screen = ui.screen();
        assert!(
            screen.contains("tools") && screen.contains("auto-compacts when messages pass"),
            "{screen}"
        );

        ui.send("/compact focus on the parser");
        ui.wait_for("compacted the conversation: ~").await;
        idle(&ui).await;

        ui.send("/clear");
        ui.wait_for("cleared the conversation").await;
        ui.send("ctx-marker after the clear");
        ui.wait_for("reply 5").await;
        idle(&ui).await;
    })
    .await;
    let sent = bodies(&server).await;
    let summarizer = sent
        .iter()
        .find(|b| b.contains("Additional focus: focus on the parser"))
        .expect("the summarizer was asked with the focus");
    assert!(summarizer.contains("ctx-marker one"));
    let last = sent.last().unwrap();
    assert!(last.contains("after the clear"), "{last}");
    for old in ["ctx-marker one", "ctx-marker two", "reply 1", "THE-SUMMARY"] {
        assert!(!last.contains(old), "`{old}` survived the clear: {last}");
    }
    assert!(w.ledger_text().contains("context_cleared"));
}

#[tokio::test(flavor = "multi_thread")]
async fn text_typed_while_compacting_comes_back_to_the_input() {
    let mut route = context_route();
    route = route.delay(3, 1500);
    let server = start(vec![route]).await;
    let w = World::routed_known(&server.uri(), "");
    w.run(|ui| async move {
        idle(&ui).await;
        three_exchanges(&ui).await;
        ui.send("/compact");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        ui.send("typed while compacting");
        ui.wait_for("compacted the conversation: ~").await;
        idle(&ui).await;
        let live = ui.live();
        assert!(live.contains("typed while compacting"), "{live}");
    })
    .await;
    // It never reached the model or the ledger as a steering message.
    assert!(
        !bodies(&server)
            .await
            .iter()
            .any(|b| b.contains("typed while compacting"))
    );
    assert!(!w.ledger_text().contains("typed while compacting"));
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_interrupts_a_compaction_and_leaves_the_conversation() {
    let route = context_route().delay(3, 3000);
    let server = start(vec![route]).await;
    let w = World::routed_known(&server.uri(), "");
    w.run(|ui| async move {
        idle(&ui).await;
        three_exchanges(&ui).await;
        ui.send("/compact");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        ui.press(KeyCode::Esc);
        ui.wait_for("interrupted").await;
        idle(&ui).await;
        assert!(!ui.screen().contains("compacted the conversation"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_is_refused_while_a_subagent_runs_and_works_after() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "worker", "prompt": "child-A: finish", "background": true}),
                ),
                Turn::text("launched"),
                Turn::text("noted the report"),
            ],
        ),
        Route::new("child-A", vec![Turn::text("child done")]).delay(0, 2500),
    ])
    .await;
    let w = World::routed_known(&server.uri(), "\n[permissions]\nallow = [\"agent\"]\n");
    let wr = &w;
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_for("launched").await;
        idle(&ui).await;
        ui.send("/clear");
        ui.wait_for(
            "subagents are still running or have reports to deliver; /clear is refused until they finish or are killed",
        )
        .await;
        assert!(!wr.ledger_text().contains("context_cleared"));
        assert!(ui.screen().contains("launched"), "{}", ui.screen());
        ui.wait_for("noted the report").await;
        idle(&ui).await;
        ui.send("/clear");
        ui.wait_for("cleared the conversation").await;
    })
    .await;
    assert!(w.ledger_text().contains("context_cleared"));
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_with_a_name_and_context_with_a_bad_argument_are_errors() {
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/clear my old work");
        ui.wait_for("needs /resume, which is not available yet")
            .await;
        ui.send("/context bogus");
        ui.wait_for("/context takes no argument or `all`").await;
    })
    .await;
    assert!(!w.ledger_text().contains("context_cleared"));
}

#[tokio::test(flavor = "multi_thread")]
async fn theme_lists_the_themes_and_marks_the_current_one() {
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/theme");
        ui.wait_for("dark-ansi (current)").await;
        let screen = ui.screen();
        for name in [
            "light-ansi",
            "dark-daltonized",
            "light-daltonized",
            "dark,",
            "light,",
        ] {
            assert!(screen.contains(name), "{name} missing from\n{screen}");
        }
        assert!(
            screen.contains("config.toml"),
            "the file is named:\n{screen}"
        );
    })
    .await;
    assert!(
        !w.user_config.exists()
            || !std::fs::read_to_string(&w.user_config)
                .unwrap()
                .contains("[tui]")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn theme_switches_and_saves_the_choice() {
    let w = World::new(vec![]).with_config("# mine\n[model]\nmodel = \"a/b\"\n");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/theme light-daltonized");
        ui.wait_for("theme is now light-daltonized (saved to your settings)")
            .await;
        ui.send("/theme");
        ui.wait_for("light-daltonized (current)").await;
        assert!(!ui.screen().contains("dark-ansi (current)"));
    })
    .await;
    let saved = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(saved.contains("# mine"), "{saved}");
    assert!(
        saved.contains("[tui]\ntheme = \"light-daltonized\""),
        "{saved}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_theme_is_an_error_and_changes_nothing() {
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/theme solarized");
        ui.wait_for("unknown theme `solarized`").await;
        assert!(ui.screen().contains("light-daltonized"));
        ui.send("/theme");
        ui.wait_for("dark-ansi (current)").await;
    })
    .await;
    assert!(!w.user_config.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_user_file_the_theme_applies_for_this_session_only() {
    let mut w = World::new(vec![]);
    w.without_user_config = true;
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/theme");
        ui.wait_for("not saved").await;
        ui.send("/theme dark");
        ui.wait_for("theme is now dark for this session only").await;
        ui.send("/theme");
        ui.wait_for("dark (current)").await;
    })
    .await;
    assert!(!w.user_config.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_configured_theme_is_the_one_in_force() {
    let w = World::new(vec![]).with_config("[tui]\ntheme = \"light\"\n");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/theme");
        ui.wait_for("light (current)").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_with_an_approval_reads_as_labelled_lines_in_screen_reader_mode() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("all done"),
    ]);
    let bytes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    w.run_linear(bytes.clone(), |ui| async move {
        ui.send("make a note");
        ui.wait_for_prompt("Permission Required:").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for("assistant: all done").await;
        // The turn is over once the bell has sounded for it.
        ui.wait_until("the second bell", |s| s.matches('\x07').count() >= 2)
            .await;
    })
    .await;
    let out = String::from_utf8(bytes.borrow().clone()).unwrap();
    for needle in [
        "you: make a note",
        "Permission Required:",
        "selected: [y] allow once",
        "— done",
        "assistant: all done",
    ] {
        assert!(out.contains(needle), "{needle} missing from\n{out:?}");
    }
    assert!(out.contains("tool: write"), "{out:?}");
    assert!(out.contains('\x07'), "{out:?}");
    assert!(
        out.chars().all(|c| !('\u{2500}'..='\u{257f}').contains(&c)),
        "box drawing in {out:?}"
    );
    let bytes = out.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        assert!(
            *b != b'\n' || (i > 0 && bytes[i - 1] == b'\r'),
            "bare \\n in {out:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(w.work.join("notes.txt")).unwrap(),
        "hello"
    );
}
