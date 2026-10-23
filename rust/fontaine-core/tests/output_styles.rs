//! Output styles change what the agent is told and nothing the guardian sees, and the system
//! prompt layers come in a fixed order.

mod common;

use common::World;
use common::runtime::build;
use fontaine_core::adjudication;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::ledger::Record;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use fontaine_guardian::Profile;
use fontaine_guardian::prompt_builder::{system_prompt, user_message};
use rig_core::completion::Usage;
use rig_core::test_utils::{MockStreamEvent, mock_final};
use serde_json::json;

fn answer(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(text.to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

fn world() -> World {
    let w = World::new();
    common::write(
        &w.config.join("output-styles/terse.md"),
        "---\ndescription: Short answers\n---\nAnswer in at most two sentences.\n",
    );
    common::write(
        &w.config.join("skills/review/SKILL.md"),
        "---\ndescription: Review a diff\n---\nbody that loads on demand\n",
    );
    common::write(
        &w.work.join("FONTAINE.md"),
        "Run cargo test before finishing.\n",
    );
    w
}

async fn first_prompt(built: &common::runtime::Built) -> String {
    let mut seen = |_| Flow::Continue;
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hello".to_string(),
            max_turns: 5,
            priced: true,
        },
        &mut seen,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    format!("{:?}", built.main.requests()[0])
}

#[tokio::test]
async fn the_prompt_layers_are_base_then_instructions_then_skills_then_style_then_append() {
    let w = world();
    let mut built = build(&w, Manual, vec![answer("ok")], true);
    built.runtime.set_output_style(Some("terse")).unwrap();
    let request = first_prompt(&built).await;
    let at = |needle: &str| request.find(needle).unwrap_or_else(|| panic!("`{needle}`"));
    let base = at("You are Fontaine");
    let instructions = at("Run cargo test before finishing.");
    let skills = at("review: Review a diff");
    let style = at("Answer in at most two sentences.");
    assert!(base < instructions && instructions < skills && skills < style);
    assert!(!request.contains("body that loads on demand"));
}

#[tokio::test]
async fn the_style_is_gone_after_none() {
    let w = world();
    let mut built = build(&w, Manual, vec![answer("ok")], true);
    built.runtime.set_output_style(Some("terse")).unwrap();
    built.runtime.set_output_style(None).unwrap();
    assert!(
        !first_prompt(&built)
            .await
            .contains("Answer in at most two sentences.")
    );
    assert!(built.runtime.set_output_style(Some("missing")).is_err());
}

#[tokio::test]
async fn the_guardians_request_and_rendered_prompt_are_byte_identical_with_and_without_a_style() {
    let args = json!({"command": "python cleanup.py"});
    let w = world();
    let render = |with_style: bool| {
        let mut built = build(&w, Manual, vec![answer("ok")], true);
        if with_style {
            built.runtime.set_output_style(Some("terse")).unwrap();
        }
        built.runtime.state.append(Record::UserMessage {
            text: "tidy the repo".to_string(),
        });
        let request = adjudication::build(&built.runtime.state, "shell", &args, Vec::new(), None);
        (
            serde_json::to_string(&request).unwrap(),
            user_message(&request),
            system_prompt(Profile::Full, &request),
        )
    };
    let without = render(false);
    let with = render(true);
    assert_eq!(without.0, with.0, "the request");
    assert_eq!(without.1, with.1, "the user message");
    assert_eq!(without.2, with.2, "the system prompt");
    assert!(!without.1.contains("Answer in at most two sentences."));
}
