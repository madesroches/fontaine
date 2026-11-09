//! The session's reasoning effort: what a model is sent, what it is refused, and who is told.

mod common;

use std::sync::{Arc, Mutex};

use common::World;
use common::runtime::{Built, build, build_with_approver};
use fontaine_core::config::{AccountKind, PermissionMode::Manual};
use fontaine_core::effort::Effort;
use fontaine_core::models::{GuardianChoice, ModelHandle, ModelRef, Models};
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use futures::future::BoxFuture;
use rig_core::catalog::Catalog;
use rig_core::completion::{Effort as RigEffort, Reasoning};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, mock_final};
use serde_json::json;

/// A handle on the catalog's `id`, whose wire applies that entry's reasoning rules; `None` has
/// no catalog entry at all.
fn handle_with(id: Option<&str>) -> ModelHandle {
    let mock = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
    let mock = match id {
        Some(id) => mock.priced_as((*Catalog::builtin().resolve(id).unwrap().spec).clone()),
        None => mock,
    };
    ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        mock.into(),
    )
}

const LISTED: &str = "openai/o3-mini";
const NOT_REASONING: &str = "openai/gpt-4o-mini";

#[test]
fn no_effort_sends_no_options() {
    for handle in [handle_with(None), handle_with(Some(LISTED))] {
        let (options, refused) = handle.generation_options(None);
        assert!(options.is_none() && refused.is_none());
    }
}

#[test]
fn a_model_the_catalog_does_not_know_is_sent_the_level_as_is() {
    let (options, refused) = handle_with(None).generation_options(Some(Effort::Max));
    assert!(refused.is_none());
    assert_eq!(
        options.unwrap().reasoning,
        Some(Reasoning::Effort(RigEffort::Max))
    );
    let (options, _) = handle_with(None).generation_options(Some(Effort::Off));
    assert_eq!(options.unwrap().reasoning, Some(Reasoning::Off));
}

#[test]
fn a_level_the_catalog_lists_is_sent_and_one_it_does_not_is_dropped_with_the_refusal() {
    let handle = handle_with(Some(LISTED));
    let (options, refused) = handle.generation_options(Some(Effort::High));
    assert!(options.is_some() && refused.is_none(), "{refused:?}");
    let (options, refused) = handle.generation_options(Some(Effort::Max));
    assert!(options.is_none());
    assert_eq!(refused.unwrap().option, "reasoning");
    assert_eq!(
        handle.supported_efforts(),
        [Effort::Low, Effort::Medium, Effort::High]
    );
}

#[test]
fn a_model_that_does_not_reason_takes_only_off() {
    let handle = handle_with(Some(NOT_REASONING));
    assert!(handle.generation_options(Some(Effort::High)).0.is_none());
    assert!(handle.generation_options(Some(Effort::Off)).0.is_some());
}

#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Warnings {
    /// The effort warnings; the runtime has others of its own.
    fn about_effort(&self) -> Vec<String> {
        let all = self.0.lock().unwrap();
        all.iter()
            .filter(|w| w.contains("effort"))
            .cloned()
            .collect()
    }
}

impl Approver for Warnings {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

fn runtime_on(
    world: &World,
    handle: ModelHandle,
    approver: Arc<Warnings>,
) -> fontaine_core::agent::AgentRuntime {
    let models = Models {
        main_chain: fontaine_core::model_roles::ModelChain::single(handle.reference.clone()),
        main: handle,
        main_listing: fontaine_core::models::Listing::default(),
        guardian: None,
        guardian_choice: GuardianChoice::Pinned,
        notices: Vec::new(),
    };
    build_with_approver(
        world,
        Manual,
        models,
        None,
        false,
        Default::default(),
        None,
        approver,
    )
}

#[test]
fn an_unsupported_level_is_warned_once_per_model_and_a_new_model_warns_again() {
    let world = World::new();
    world.user_file("[model]\neffort = \"max\"\n");
    let warnings = Arc::new(Warnings::default());
    let mut runtime = runtime_on(&world, handle_with(Some(LISTED)), warnings.clone());
    assert_eq!(runtime.state.effort(), Some(Effort::Max));

    assert!(runtime.turn_generation_options().is_none());
    assert!(runtime.turn_generation_options().is_none());
    assert_eq!(warnings.about_effort().len(), 1);
    let text = warnings.about_effort()[0].clone();
    assert!(
        text.contains("`max`") && text.contains("test/mock"),
        "{text}"
    );

    // A model switch keeps the level and checks it again against the new model.
    runtime.models.main = handle_with(Some(NOT_REASONING));
    runtime.models.main.reference = ModelRef::parse("test/other").unwrap();
    assert!(runtime.turn_generation_options().is_none());
    assert_eq!(warnings.about_effort().len(), 2);

    runtime.state.set_effort(Some(Effort::Off));
    assert!(runtime.turn_generation_options().is_some());
    assert_eq!(warnings.about_effort().len(), 2);
}

#[test]
fn interleaved_main_turns_and_child_spawns_warn_once_per_model_and_level() {
    let world = World::new();
    world.user_file("[model]\neffort = \"max\"\n");
    let warnings = Arc::new(Warnings::default());
    let mut runtime = runtime_on(&world, handle_with(Some(LISTED)), warnings.clone());
    let other = || {
        let mut handle = handle_with(Some(NOT_REASONING));
        handle.reference = ModelRef::parse("test/other").unwrap();
        handle
    };
    let child_spawn = |runtime: &fontaine_core::agent::AgentRuntime, handle: &ModelHandle| {
        let refused = handle.generation_options(Some(Effort::Max)).1.unwrap();
        runtime
            .state
            .warn_effort_refused(&handle.reference.to_string(), Effort::Max, &refused);
    };
    let first = runtime.models.main.clone();
    let second = other();

    runtime.turn_generation_options();
    child_spawn(&runtime, &second);
    child_spawn(&runtime, &first);
    runtime.models.main = second.clone();
    runtime.turn_generation_options();
    child_spawn(&runtime, &first);
    runtime.models.main = first;
    runtime.turn_generation_options();
    assert_eq!(warnings.about_effort().len(), 2);
}

fn text_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(text.to_string()),
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
    ]
}

fn spawn_turn(agent_type: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: "call-agent".into(),
            name: "agent".into(),
            arguments: json!({"agent_type": agent_type, "prompt": "look around"}),
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(rig_core::completion::Usage::new())),
    ]
}

async fn root_turn(built: &Built) {
    built.runtime.begin_turn("go", false);
    let outcome = run_turn(
        TurnParams {
            served: Default::default(),
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "go".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(5),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: built.runtime.turn_generation_options(),
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
}

fn reasonings(built: &Built) -> Vec<Option<Reasoning>> {
    built
        .main
        .requests()
        .iter()
        .map(|r| r.options.reasoning)
        .collect()
}

#[tokio::test]
async fn a_turn_sends_the_session_level_and_an_unset_level_sends_nothing() {
    let world = World::new();
    world.user_file("[model]\neffort = \"high\"\n");
    let built = build(&world, Manual, vec![text_turn("done")], false);
    root_turn(&built).await;
    assert_eq!(
        reasonings(&built),
        [Some(Reasoning::Effort(RigEffort::High))]
    );

    let world = World::new();
    let built = build(&world, Manual, vec![text_turn("done")], false);
    root_turn(&built).await;
    assert_eq!(reasonings(&built), [None]);
}

#[tokio::test]
async fn effort_set_mid_session_reaches_the_next_turn_without_a_rebuild() {
    let world = World::new();
    let built = build(
        &world,
        Manual,
        vec![text_turn("one"), text_turn("two")],
        false,
    );
    root_turn(&built).await;
    built.runtime.state.set_effort(Some(Effort::Low));
    root_turn(&built).await;
    assert_eq!(
        reasonings(&built),
        [None, Some(Reasoning::Effort(RigEffort::Low))]
    );
}

#[tokio::test]
async fn a_child_inherits_the_session_level_unless_its_type_sets_one() {
    let world = World::new();
    world.user_file(
        "[model]\neffort = \"high\"\n\
         [permissions]\nallow = [\"agent\"]\n\
         [agents.types.scout]\ntools = [\"read\"]\neffort = \"low\"\n",
    );
    let built = build(
        &world,
        Manual,
        vec![
            spawn_turn("worker"),
            text_turn("worker done"),
            text_turn("root one"),
            spawn_turn("scout"),
            text_turn("scout done"),
            text_turn("root two"),
        ],
        false,
    );
    root_turn(&built).await;
    root_turn(&built).await;
    let high = Some(Reasoning::Effort(RigEffort::High));
    let low = Some(Reasoning::Effort(RigEffort::Low));
    // The mock records each call in the order it was made: root, child, root; root, child, root.
    assert_eq!(reasonings(&built), [high, high, high, high, low, high]);
}
