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

use common::{SESSION_ID, verdict, verdict_endpoint};
use fontaine_core::config::PermissionMode;

fn ledger_text(w: &World) -> String {
    std::fs::read_to_string(
        w.data
            .join("sessions")
            .join(SESSION_ID)
            .join("ledger")
            .join("ledger.jsonl"),
    )
    .unwrap()
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
        ui.wait_for("guardian other/second").await;
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
        ui.wait_for("available: cloud/judge-b").await;
        ui.send("/guardian-model cloud/judge-b");
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
            ui.screen().contains("guardian is now cloud/flash"),
            "{}",
            ui.screen()
        );
    })
    .await;
    assert!(w.work.join("picked.txt").exists());
    assert_eq!(alt.received_requests().await.unwrap().len(), 1);
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
