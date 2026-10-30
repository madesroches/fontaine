//! Skills as slash commands, the `/` menu and output styles in the interface.

mod common;

use common::{Ui, World, text_turn, tool_turn};
use ratatui::crossterm::event::KeyCode;
use rig_core::test_utils::MockStreamEvent;
use serde_json::json;

const ALLOW_SLEEP: &str = "[permissions]\nallow = [\"shell(sleep *)\"]\n";

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

/// A world with the user-invocable `review` skill and a script of `turns`.
fn skill_world(turns: Vec<Vec<MockStreamEvent>>) -> World {
    World::new(turns).with_user_file(
        "skills/review/SKILL.md",
        "---\ndescription: Review a pull request\nargument-hint: [pr-number]\n---\nReview pull request $0 and report.\n",
    )
}

fn world() -> World {
    World::new(vec![text_turn("ok"), text_turn("ok again")])
        .with_user_file(
            "skills/review/SKILL.md",
            "---\ndescription: Review a pull request\nargument-hint: [pr-number]\n---\nReview pull request $0 and report.\n",
        )
        .with_user_file(
            "skills/internal/SKILL.md",
            "---\ndescription: Only the model uses this\nuser-invocable: false\n---\nbody\n",
        )
        .with_user_file(
            "output-styles/terse.md",
            "---\ndescription: Short answers\n---\nAnswer in at most two sentences.\n",
        )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_slash_menu_filters_commands_and_user_invocable_skills_with_their_hints() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        // The bare list is windowed to its first rows, so each entry is found by filtering.
        ui.type_text("/rev");
        ui.wait_for("/review [pr-number]").await;
        let screen = ui.screen();
        assert!(screen.contains("Review a pull request"), "{screen}");
        assert!(screen.contains("Tab complete"), "{screen}");
        assert!(!screen.contains("/output-style"), "{screen}");
        ui.press(KeyCode::Esc);
        ui.press(KeyCode::Backspace);
        ui.press(KeyCode::Backspace);
        ui.press(KeyCode::Backspace);
        ui.type_text("out");
        ui.wait_for("/output-style").await;
        ui.press(KeyCode::Backspace);
        ui.press(KeyCode::Backspace);
        ui.press(KeyCode::Backspace);
        ui.type_text("mod");
        ui.wait_for("/model").await;
        // A skill the user cannot invoke is not offered.
        ui.press(KeyCode::Backspace);
        ui.press(KeyCode::Backspace);
        ui.press(KeyCode::Backspace);
        ui.type_text("intern");
        ui.wait_until("nothing matches", |s| !s.contains("Tab complete"))
            .await;
        let screen = ui.screen();
        assert!(!screen.contains("/internal"), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_a_menu_entry_runs_the_command() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.type_text("/mod");
        ui.wait_for("Tab complete").await;
        ui.press(KeyCode::Enter);
        ui.wait_until("the model picker", |_| ui.live().contains("type to filter"))
            .await;
        ui.press(KeyCode::Esc);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skill_runs_as_a_command_with_its_arguments_and_the_users_words_are_what_is_recorded() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/review 42");
        ui.wait_for("ok").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert!(
        requests[0].contains("Review pull request 42 and report."),
        "{}",
        requests[0]
    );
    // The ledger holds what the human typed, not the skill's text.
    let ledger = std::fs::read_to_string(
        w.data
            .join("sessions")
            .join(w.session_id())
            .join("ledger/ledger.jsonl"),
    )
    .unwrap();
    assert!(ledger.contains("\"text\":\"/review 42\""), "{ledger}");
    assert!(!ledger.contains("Review pull request"), "{ledger}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_command_says_so_and_a_skill_only_the_model_may_use_is_not_a_command() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/nonesuch");
        ui.wait_for("unknown command /nonesuch").await;
        ui.send("/internal");
        ui.wait_for("unknown command /internal").await;
    })
    .await;
    assert!(w.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_style_switches_the_prompt_the_agent_gets_and_none_removes_it() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/output-style");
        ui.wait_for("terse (Short answers)").await;
        ui.send("/output-style terse");
        ui.wait_for("output style is now terse").await;
        ui.send("hello");
        ui.wait_for("ok").await;
        idle(&ui).await;
        ui.send("/output-style none");
        ui.wait_for("output style is now none").await;
        ui.send("again");
        ui.wait_for("ok again").await;
        ui.send("/output-style nonesuch");
        ui.wait_for("no output style named `nonesuch`").await;
    })
    .await;
    let requests = w.requests();
    assert!(
        requests[0].contains("Answer in at most two sentences."),
        "{}",
        requests[0]
    );
    assert!(!requests[1].contains("Answer in at most two sentences."));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_skill_list_is_in_the_system_prompt_without_the_skill_bodies() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("hello");
        ui.wait_for("ok").await;
        ui.press(KeyCode::Esc);
    })
    .await;
    let request = &w.requests()[0];
    assert!(
        request.contains("review: Review a pull request"),
        "{request}"
    );
    assert!(
        request.contains("internal: Only the model uses this"),
        "{request}"
    );
    assert!(
        !request.contains("Review pull request $0"),
        "the body loads on demand"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skill_command_that_needs_approval_prompts_and_answering_completes_the_expansion() {
    let w = World::new(vec![text_turn("done")]).with_user_file(
        "skills/greet/SKILL.md",
        "---\ndescription: Greets\n---\nOutput: !`echo hello-from-skill > out.txt && echo marker-ran`\n",
    );
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/greet");
        ui.wait_for_prompt("Allow shell?").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for("done").await;
    })
    .await;
    assert!(
        w.requests()[0].contains("marker-ran"),
        "{}",
        w.requests()[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn help_and_skills_list_the_loaded_skills() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/help");
        ui.wait_for("/review [pr-number] — Review a pull request")
            .await;
        // The model-only skill is not a command, but /skills still shows it.
        assert!(!ui.screen().contains("/internal"), "{}", ui.screen());
        ui.send("/skills");
        ui.wait_for("internal — Only the model uses this [user] (model only)")
            .await;
    })
    .await;
}

// ---------------------------------------------------------------------------------------
// queued input: skills, commands and `!` lines typed during a turn
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_skill_typed_during_a_turn_runs_expanded_once_it_ends() {
    let w = skill_world(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("current answer"),
        text_turn("skill answer"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/review 42");
        ui.wait_for("queued: /review 42").await;
        ui.wait_for("current answer").await;
        ui.wait_for("skill answer").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    // The skill rode out the turn: its raw and its expansion never reached the tool result.
    assert!(!requests[1].contains("/review 42"), "{}", requests[1]);
    assert!(
        !requests[1].contains("Review pull request 42"),
        "{}",
        requests[1]
    );
    // The third request is the skill's turn: the expanded body, never the literal slash line.
    assert!(
        requests[2].contains("Review pull request 42 and report."),
        "{}",
        requests[2]
    );
    assert!(!requests[2].contains("/review 42"), "{}", requests[2]);
    let ledger = w.ledger_text();
    assert_eq!(
        ledger.matches("\"text\":\"/review 42\"").count(),
        1,
        "{ledger}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_queued_after_a_skill_waits_with_it_and_runs_after_it() {
    let w = skill_world(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("current answer"),
        text_turn("skill answer"),
        text_turn("message answer"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/review 42");
        ui.wait_for("queued: /review 42").await;
        ui.send("please also check");
        ui.wait_for("queued: please also check").await;
        ui.wait_for("current answer").await;
        ui.wait_for("skill answer").await;
        ui.wait_for("message answer").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    // The running turn's tool result carried neither the skill nor the message behind it.
    assert!(!requests[1].contains("/review 42"), "{}", requests[1]);
    assert!(
        !requests[1].contains("please also check"),
        "{}",
        requests[1]
    );
    assert!(
        requests[2].contains("Review pull request 42 and report."),
        "{}",
        requests[2]
    );
    // The message ran as a turn of its own, not steered into the skill's turn.
    assert!(requests[3].contains("please also check"), "{}", requests[3]);
    assert!(
        !requests[3].contains("The user sent this message while you were working"),
        "{}",
        requests[3]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_order_spans_replay_and_newer_input_cannot_overtake_it() {
    let w = skill_world(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("first answer"),
        tool_turn("c2", "shell", json!({"command": "sleep 1"})),
        text_turn("skill answer"),
        text_turn("M answer"),
        tool_turn("c3", "shell", json!({"command": "sleep 1"})),
        text_turn("N answer"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/review 42");
        ui.wait_for("queued: /review 42").await;
        ui.send("M message text");
        ui.wait_for("queued: M message text").await;
        // The skill and the message replay once the turn answers.
        ui.wait_for("first answer").await;
        ui.wait_until("the skill turn's tool call", |s| {
            s.matches("• shell sleep 1").count() >= 2
        })
        .await;
        // Typed while the skill's turn runs, with the message still pending: held, not steered.
        ui.send("N message text");
        ui.wait_for("queued: N message text").await;
        ui.wait_for("skill answer").await;
        ui.wait_for("M answer").await;
        ui.wait_until("the message turn's tool call", |s| {
            s.matches("• shell sleep 1").count() >= 3
        })
        .await;
        // The last submission runs with delivery released: this one steers normally.
        ui.send("O message text");
        ui.wait_for("queued: O message text").await;
        ui.wait_for("N answer").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 7, "{requests:?}");
    assert!(!requests[1].contains("/review 42"), "{}", requests[1]);
    assert!(!requests[1].contains("M message text"), "{}", requests[1]);
    assert!(
        requests[2].contains("Review pull request 42 and report."),
        "{}",
        requests[2]
    );
    // N was held out of the skill turn's tool result while M waited behind it.
    assert!(!requests[3].contains("N message text"), "{}", requests[3]);
    assert!(
        !requests[3].contains("The user sent this message while you were working"),
        "{}",
        requests[3]
    );
    // M ran as its own turn; N after it; O, typed last of all, steered into N's turn.
    assert!(requests[4].contains("M message text"), "{}", requests[4]);
    assert!(!requests[4].contains("N message text"), "{}", requests[4]);
    assert!(requests[5].contains("N message text"), "{}", requests[5]);
    assert!(!requests[5].contains("O message text"), "{}", requests[5]);
    assert!(requests[6].contains("O message text"), "{}", requests[6]);
    assert!(
        requests[6].contains("The user sent this message while you were working"),
        "{}",
        requests[6]
    );
    let ledger = w.ledger_text();
    for raw in [
        "/review 42",
        "M message text",
        "N message text",
        "O message text",
    ] {
        assert_eq!(
            ledger.matches(&format!("\"text\":\"{raw}\"")).count(),
            1,
            "{ledger}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_the_turn_puts_a_queued_skill_back_in_the_input() {
    let w = skill_world(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("never"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/review 42");
        ui.wait_for("queued: /review 42").await;
        ui.press(KeyCode::Esc);
        ui.wait_for("interrupted").await;
        idle(&ui).await;
        // Nothing was sent unseen: the invocation waits in the input, unexpanded.
        let live = ui.live();
        assert!(live.contains("/review 42"), "{live}");
        assert!(!live.contains("queued:"), "{live}");
    })
    .await;
    assert_eq!(w.requests().len(), 1);
    assert!(!w.ledger_text().contains("/review 42"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_builtin_queued_mid_turn_runs_after_the_turn_in_typed_order() {
    let w = skill_world(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
        text_turn("skill answer"),
        text_turn("message answer"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/review 42");
        ui.wait_for("queued: /review 42").await;
        ui.send("queued message");
        ui.wait_for("queued: queued message").await;
        ui.send("/rename later");
        ui.wait_for("queued: /rename later").await;
        ui.wait_for("answer").await;
        ui.wait_for("skill answer").await;
        ui.wait_for("message answer").await;
        ui.wait_for("session renamed to later").await;
        idle(&ui).await;
        let screen = ui.screen();
        let skill_turn = screen.find("> /review 42").expect("the skill's turn");
        let message_turn = screen.find("> queued message").expect("the message's turn");
        let rename = screen
            .find("session renamed to later")
            .expect("the rename notice");
        assert!(skill_turn < message_turn, "typed order:\n{screen}");
        assert!(message_turn < rename, "the rename runs last:\n{screen}");
    })
    .await;
    let ledger = w.ledger_text();
    assert!(ledger.contains("\"name\":\"later\""), "{ledger}");
    assert_eq!(
        ledger.matches("\"text\":\"/review 42\"").count(),
        1,
        "{ledger}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_immediate_command_runs_mid_turn_and_never_queues() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/status");
        // The report lands while the tool call still runs, before the answer.
        ui.wait_for("MCP servers: 0 of 0 connected").await;
        assert!(!ui.screen().contains("queued:"), "{}", ui.screen());
        ui.wait_for("answer").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(!requests[1].contains("/status"), "{}", requests[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_command_queued_mid_turn_enters_plan_mode_and_sends_its_description() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
        text_turn("planned reply"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/plan do x");
        ui.wait_for("queued: /plan do x").await;
        ui.wait_for("answer").await;
        // Plan mode turns on as the command executes, after the turn.
        ui.wait_for("plan mode on").await;
        ui.wait_for("planned reply").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(requests[2].contains("do x"), "{}", requests[2]);
    assert!(!requests[2].contains("/plan do x"), "{}", requests[2]);
    let ledger = w.ledger_text();
    assert_eq!(ledger.matches("\"text\":\"do x\"").count(), 1, "{ledger}");
    assert!(!ledger.contains("/plan do x"), "{ledger}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bang_command_typed_mid_turn_runs_after_it_and_reaches_the_next_message() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
        text_turn("message reply"),
    ])
    .with_config(ALLOW_SLEEP);
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("!echo hi-from-queue");
        ui.wait_for("queued: !echo hi-from-queue").await;
        ui.send("queued message");
        ui.wait_for("queued: queued message").await;
        ui.wait_for("answer").await;
        // The shell run executes after the turn, before the queued message.
        ui.wait_for("! echo hi-from-queue").await;
        ui.wait_for("message reply").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(!requests[1].contains("hi-from-queue"), "{}", requests[1]);
    assert!(
        requests[2].contains("$ echo hi-from-queue"),
        "{}",
        requests[2]
    );
    assert!(requests[2].contains("queued message"), "{}", requests[2]);
    let ledger = w.ledger_text();
    assert!(ledger.contains("\"kind\":\"user_shell\""), "{ledger}");
    assert!(!ledger.contains("!echo hi-from-queue"), "{ledger}");
    assert_eq!(
        ledger.matches("\"kind\":\"user_message\"").count(),
        2,
        "{ledger}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_during_a_queued_skill_expansion_aborts_only_that_expansion() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
        text_turn("next reply"),
    ])
    .with_config(ALLOW_SLEEP)
    .with_user_file(
        "skills/slow/SKILL.md",
        "---\ndescription: Slow\n---\nRun: !`sleep 30`\nMarker expanded-done.\n",
    );
    w.run(|ui| async move {
        ui.send("start");
        ui.wait_for("• shell sleep 1").await;
        ui.send("/slow");
        ui.wait_for("queued: /slow").await;
        ui.send("after skill");
        ui.wait_for("queued: after skill").await;
        ui.wait_for("answer").await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        ui.press(KeyCode::Esc);
        ui.wait_for("interrupted").await;
        // The expansion's raw is spent; the next submission still runs.
        ui.wait_for("next reply").await;
        idle(&ui).await;
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(requests[2].contains("after skill"), "{}", requests[2]);
    assert!(!requests[2].contains("expanded-done"), "{}", requests[2]);
    let ledger = w.ledger_text();
    assert_eq!(ledger.matches("\"text\":\"/slow\"").count(), 1, "{ledger}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_builtin_wins_over_a_skill_of_the_same_name() {
    let w = World::new(vec![]).with_user_file(
        "skills/status/SKILL.md",
        "---\ndescription: Shadowed\n---\nbody\n",
    );
    w.run(|ui| async move {
        idle(&ui).await;
        ui.wait_for("skill /status is hidden by the built-in command of the same name")
            .await;
        ui.send("/status");
        ui.wait_for("MCP servers: 0 of 0 connected").await;
        ui.send("/skills");
        ui.wait_for("status — Shadowed [user] (hidden by /status)")
            .await;
    })
    .await;
    assert!(w.requests().is_empty());
}
