//! Skills as slash commands, the `/` menu and output styles in the interface.

mod common;

use common::{Ui, World, text_turn};
use ratatui::crossterm::event::KeyCode;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
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
async fn the_slash_menu_lists_commands_and_user_invocable_skills_with_their_hints() {
    let w = world();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.type_text("/");
        ui.wait_for("/review [pr-number]").await;
        let screen = ui.screen();
        assert!(screen.contains("Review a pull request"), "{screen}");
        assert!(screen.contains("/output-style"), "{screen}");
        assert!(screen.contains("/model"), "{screen}");
        // A skill the user cannot invoke is not offered.
        assert!(!screen.contains("/internal"), "{screen}");
        // The menu narrows as the name is typed.
        ui.type_text("rev");
        ui.wait_until("only the matching entry", |s| {
            s.contains("/review [pr-number]") && !s.contains("/output-style")
        })
        .await;
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
