//! Multimodal user messages at the agent layer: what `run_turn` hands the model for a
//! prompt carrying inline attachments, and the modalities gate that decides it.

mod common;

use base64::Engine;
use common::World;
use common::runtime::build_with_models;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::AccountKind;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::turn::{Attachment, AttachmentMedia, Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::catalog::{Modalities, ModelSpec};
use rig_core::completion::CompletionRequest;
use rig_core::message::{DocumentMediaType, Message, UserContent};
use rig_core::providers::registry::ProviderId;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, mock_final};

/// A 1x1 PNG; the magic bytes are what the sniff reads.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn png() -> Attachment {
    Attachment {
        label: "pixel.png".into(),
        bytes: base64::engine::general_purpose::STANDARD
            .decode(PNG_B64)
            .unwrap(),
        media: AttachmentMedia::Image(rig_core::message::ImageMediaType::PNG),
    }
}

fn pdf() -> Attachment {
    Attachment {
        label: "notes.pdf".into(),
        bytes: b"%PDF-1.7\nnot really a pdf".to_vec(),
        media: AttachmentMedia::Pdf,
    }
}

fn spec(image: bool, pdf: bool) -> ModelSpec {
    let mut spec = ModelSpec::new(
        ProviderId::catalog("openai").expect("the openai provider is registered"),
        "gpt-4o-mini",
    );
    spec.input.image = image;
    spec.input.pdf = pdf;
    spec
}

fn build(world: &World, spec: ModelSpec) -> (AgentRuntime, MockCompletionModel) {
    let main = MockCompletionModel::from_stream_turns(vec![vec![MockStreamEvent::FinalResponse(
        mock_final(Default::default()),
    )]])
    .priced_as(spec);
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        main.clone().into(),
    );
    let models = Models {
        main: handle.clone(),
        main_listing: Vec::new(),
        guardian: Some(GuardianModel::Ready(handle)),
        guardian_choice: GuardianChoice::MainFallback,
        notices: Vec::new(),
    };
    let runtime = build_with_models(world, Manual, models, None, false, Default::default(), None);
    (runtime, main)
}

/// Runs one turn and returns the request the model was sent.
async fn run(
    runtime: &AgentRuntime,
    main: &MockCompletionModel,
    attachments: Vec<Attachment>,
    modalities: Modalities,
) -> CompletionRequest {
    let outcome = run_turn(
        TurnParams {
            agent: &runtime.agent,
            state: &runtime.state,
            prompt: "look at this".to_string(),
            attachments,
            modalities,
            max_turns: Some(1),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let requests = main.requests();
    requests.last().expect("the model was asked").clone()
}

/// The parts of the last user message of `request`.
fn prompt_parts(request: &CompletionRequest) -> Vec<UserContent> {
    let Some(message) = request
        .chat_history
        .iter()
        .rev()
        .find(|m| matches!(m.as_ref(), Message::User { .. }))
    else {
        panic!("a user message in {:?}", request.chat_history);
    };
    let Message::User { content } = message.as_ref() else {
        unreachable!("filtered above");
    };
    content.clone()
}

#[tokio::test]
async fn an_image_attachment_reaches_the_model_as_an_image_part() {
    let w = World::new();
    let accepted = spec(true, false);
    let modalities = accepted.input;
    let (runtime, main) = build(&w, accepted);
    let parts = prompt_parts(&run(&runtime, &main, vec![png()], modalities).await);
    assert_eq!(parts.len(), 2);
    let UserContent::Image(image) = &parts[1] else {
        panic!("an image part, got {:?}", parts[1]);
    };
    assert_eq!(
        image.media_type,
        Some(rig_core::message::ImageMediaType::PNG)
    );
    assert!(
        format!("{:?}", image.data).contains(PNG_B64),
        "the part carries the base64 bytes"
    );
}

#[tokio::test]
async fn a_pdf_attachment_reaches_the_model_as_a_document_part() {
    let w = World::new();
    let accepted = spec(false, true);
    let modalities = accepted.input;
    let (runtime, main) = build(&w, accepted);
    let parts = prompt_parts(&run(&runtime, &main, vec![pdf()], modalities).await);
    assert_eq!(parts.len(), 2);
    let UserContent::Document(document) = &parts[1] else {
        panic!("a document part, got {:?}", parts[1]);
    };
    assert_eq!(document.media_type, Some(DocumentMediaType::PDF));
    assert!(
        format!("{:?}", document.data).contains("JVBER"),
        "the part carries the base64 bytes, got {:?}",
        document.data
    );
}

#[tokio::test]
async fn an_attachment_the_model_rejects_becomes_a_text_note_not_a_part() {
    let w = World::new();
    let accepted = spec(false, false);
    let modalities = accepted.input;
    let (runtime, main) = build(&w, accepted);
    let parts = prompt_parts(&run(&runtime, &main, vec![png(), pdf()], modalities).await);
    assert_eq!(parts.len(), 3, "the prompt and one note per attachment");
    for part in &parts[1..] {
        assert!(matches!(part, UserContent::Text(_)), "got {part:?}");
    }
    let text = format!("{:?}", parts[1]) + &format!("{:?}", parts[2]);
    assert!(text.contains("does not accept"), "{text}");
    assert!(!text.contains(PNG_B64), "{text}");
    assert!(!text.contains("JVBER"), "{text}");
}
