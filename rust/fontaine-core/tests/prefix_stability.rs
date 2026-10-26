//! Two agents built from the same inputs send the same front of the request, so a provider can
//! reuse its cached prefix across sessions and rebuilds.

mod common;

use common::World;
use common::runtime::{Built, build};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::completion::Usage;
use rig_core::test_utils::{MockStreamEvent, mock_final};

async fn first_request(built: &Built) -> String {
    let mut seen = |_| Flow::Continue;
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hello".to_string(),
            max_turns: 5,
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut seen,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    format!("{:?}", built.main.requests()[0])
}

fn answer() -> Vec<Vec<MockStreamEvent>> {
    vec![vec![
        MockStreamEvent::Text("ok".to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]]
}

#[tokio::test]
async fn the_system_prompt_and_tool_order_are_identical_across_builds() {
    let w = World::new();
    common::write(&w.work.join("FONTAINE.md"), "Run cargo test.\n");
    let first = build(&w, Manual, answer(), false);
    let second = build(&w, Manual, answer(), false);
    assert_eq!(first.runtime.tool_names(), second.runtime.tool_names());
    assert_eq!(
        first_request(&first).await,
        first_request(&second).await,
        "the request front differs between two builds from the same inputs"
    );
}
