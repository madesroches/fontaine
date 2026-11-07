//! A failed model call keeps its turn: the conversation store holds the interrupted task, and
//! a retryable transport failure is retried before the failure reaches the user. A run dropped
//! mid-turn keeps what it committed too, marked as unfinished.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::World;
use common::runtime::{self, Built, build};
use fontaine_core::checkpoint::RewindWhat;
use fontaine_core::config::{AccountKind, PermissionMode::Manual};
use fontaine_core::ledger::Record;
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::session::{CONVERSATION_LOG, Session};
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn, user_message};
use fontaine_core::turn_recovery::{Fallback, Unsettled};
use rig_core::ProviderResponseError;
use rig_core::completion::Usage;
use rig_core::driver::{Exchange, Model, Opening, Transport};
use rig_core::test_utils::{
    MockCompletionModel, MockError, MockFrame, MockRuntime, MockScript, MockStreamEvent, mock_final,
};
use rig_memory::AsyncStore;
use serde_json::json;
use tokio::sync::Notify;

fn error_turn(status: u16) -> Vec<MockStreamEvent> {
    vec![MockStreamEvent::Error(MockError::ProviderResponse(
        ProviderResponseError::new(http::StatusCode::from_u16(status).unwrap(), "boom"),
    ))]
}

fn answer_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::FinalResponse(mock_final(Usage::new())),
    ]
}

async fn run(built: &Built, prompt: &str) -> TurnOutcome {
    run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: prompt.to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(3),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
            generation_options: None,
        },
        &mut |_| Flow::Continue,
    )
    .await
}

async fn stored(built: &Built) -> String {
    let conversation = rig_memory::load_async(&built.runtime.session.store(), CONVERSATION_LOG)
        .await
        .unwrap();
    format!("{:?}", conversation.messages())
}

/// A scripted transport that never answers its `hold`th request (from 0), and tells `seen`
/// when it arrives, so a test can drop the run while that model call is in flight.
#[derive(Clone)]
struct Holding {
    inner: MockRuntime,
    hold: usize,
    sent: Arc<AtomicUsize>,
    seen: Arc<Notify>,
}

impl Transport<MockScript> for Holding {
    fn send(
        &self,
        request: <MockScript as rig_core::wire::Wire>::Payload,
        exchange: Exchange,
    ) -> Opening<MockFrame> {
        if self.sent.fetch_add(1, Ordering::SeqCst) == self.hold {
            let seen = self.seen.clone();
            return Opening::new(async move {
                seen.notify_one();
                std::future::pending().await
            });
        }
        self.inner.send(request, exchange)
    }
}

/// Models whose main model answers `turns` but holds request `hold`, and the mock that records
/// every request it answered.
fn holding(
    turns: Vec<Vec<MockStreamEvent>>,
    hold: usize,
) -> (Models, MockCompletionModel, Arc<Notify>) {
    let mock = MockCompletionModel::from_stream_turns(turns);
    let seen = Arc::new(Notify::new());
    let transport = Holding {
        inner: mock.transport.clone(),
        hold,
        sent: Arc::new(AtomicUsize::new(0)),
        seen: seen.clone(),
    };
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        Model::new(MockScript::default(), transport).into(),
    );
    let models = Models {
        main: handle.clone(),
        main_listing: Vec::new(),
        guardian: Some(GuardianModel::Ready(handle)),
        guardian_choice: GuardianChoice::MainFallback,
        notices: Vec::new(),
    };
    (models, mock, seen)
}

fn shell_turn(command: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: json!({ "command": command }),
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(Usage::new())),
    ]
}

fn params<'a>(runtime: &'a fontaine_core::agent::AgentRuntime, prompt: &str) -> TurnParams<'a> {
    TurnParams {
        agent: &runtime.agent,
        state: &runtime.state,
        prompt: prompt.to_string(),
        attachments: Vec::new(),
        modalities: rig_core::catalog::Modalities::default(),
        max_turns: Some(5),
        priced: true,
        model: "scripted".to_string(),
        provider: "mock".to_string(),
        native_search: false,
        generation_options: None,
    }
}

/// Runs `prompt` until the held request arrives, then drops the run, as an interrupt does.
async fn drop_mid_turn(runtime: &fontaine_core::agent::AgentRuntime, seen: &Notify, prompt: &str) {
    let mut on_event = |_| Flow::Continue;
    let run = run_turn(params(runtime, prompt), &mut on_event);
    tokio::select! {
        outcome = run => panic!("the run was meant to be held, got {outcome:?}"),
        () = seen.notified() => {}
    }
}

async fn conversation(runtime: &fontaine_core::agent::AgentRuntime, log: &str) -> Vec<String> {
    rig_memory::load_async(&runtime.session.store(), log)
        .await
        .unwrap()
        .messages()
        .iter()
        .map(|m| format!("{m:?}"))
        .collect()
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn an_interrupted_turn_keeps_its_tool_round_and_the_next_turn_reads_it() {
    let w = World::new();
    w.user_file("[permissions]\nallow = [\"shell\"]\n");
    let (models, mock, seen) = holding(
        vec![shell_turn("echo kept-output"), answer_turn("carried on")],
        1,
    );
    let rt = runtime::build_with_models(&w, Manual, models, None, false, Default::default(), None);
    let before = rt.conversation_len().await.unwrap();
    drop_mid_turn(&rt, &seen, "first task").await;
    let fallback = Fallback {
        prompt: user_message("first task", &[], &Default::default()),
        log_len_before: before,
    };
    rt.keep_unsettled_turn(Unsettled::User, Some(fallback))
        .await;

    let main = conversation(&rt, CONVERSATION_LOG).await;
    assert_eq!(main.len(), 4, "{main:?}");
    assert!(main[0].contains("first task"), "{main:?}");
    assert!(main[1].contains("call-1"), "{main:?}");
    assert!(main[2].contains("kept-output"), "{main:?}");
    assert!(
        main[3].contains("[Request interrupted by user]"),
        "{main:?}"
    );
    assert!(conversation(&rt, "main.inflight").await.is_empty());

    let outcome = run_turn(params(&rt, "follow up"), &mut |_| Flow::Continue).await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let requests = mock.requests();
    let history = format!("{:?}", requests.last().unwrap().chat_history);
    let order = [
        "first task",
        "call-1",
        "kept-output",
        "interrupted by user",
        "follow up",
    ]
    .map(|needle| {
        history
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}: {history}"))
    });
    assert!(order.is_sorted(), "{order:?}: {history}");
}

#[tokio::test]
async fn a_resumed_session_folds_the_turn_a_dead_process_left() {
    let w = World::new();
    w.user_file("[permissions]\nallow = [\"shell\"]\n");
    let (models, _, seen) = holding(vec![shell_turn("echo kept-output")], 1);
    let rt = runtime::build_with_models(&w, Manual, models, None, false, Default::default(), None);
    drop_mid_turn(&rt, &seen, "first task").await;
    // The process dies here: nothing folds the journal.
    assert_eq!(conversation(&rt, "main.inflight").await.len(), 3);
    let session_id = rt.session.id.clone();
    drop(rt);
    // A copy, because a dropped runtime still holds its session's files open, which Windows
    // refuses a second writer; a real restart has no first process.
    let restarted = tempfile::tempdir().unwrap();
    copy_tree(&w.data, restarted.path());
    let session = Session::open(restarted.path(), &session_id).unwrap();
    let (models, _, _) = holding(Vec::new(), usize::MAX);
    let resumed = runtime::build_resumed(&w, Manual, models, session);
    resumed.record_session_start().await;
    let main = conversation(&resumed, CONVERSATION_LOG).await;
    assert_eq!(main.len(), 4, "{main:?}");
    assert!(main[0].contains("first task") && main[2].contains("kept-output"));
    assert!(
        main[3].contains("the session ended before this turn finished"),
        "{main:?}"
    );
    assert!(conversation(&resumed, "main.inflight").await.is_empty());
}

#[tokio::test]
async fn a_journal_left_by_a_failed_fold_goes_before_the_next_turn() {
    let w = World::new();
    w.user_file("[permissions]\nallow = [\"shell\"]\n");
    let (models, mock, seen) = holding(
        vec![shell_turn("echo kept-output"), answer_turn("carried on")],
        1,
    );
    let rt = runtime::build_with_models(&w, Manual, models, None, false, Default::default(), None);
    drop_mid_turn(&rt, &seen, "first task").await;
    // The fold after the drop failed, so the journal is still there when the next turn starts;
    // the surfaces fold it first.
    rt.keep_unsettled_turn(Unsettled::SessionEnded, None).await;
    let outcome = run_turn(params(&rt, "follow up"), &mut |_| Flow::Continue).await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    let requests = mock.requests();
    let history = format!("{:?}", requests.last().unwrap().chat_history);
    let old = history.find("kept-output").expect("the old turn");
    let marker = history.find("session ended").expect("the marker");
    let new = history.find("follow up").expect("the new prompt");
    assert!(old < marker && marker < new, "{history}");
    assert!(conversation(&rt, "main.inflight").await.is_empty());
}

#[tokio::test]
async fn clear_drops_the_pending_journal_and_its_marks() {
    let w = World::new();
    w.user_file("[permissions]\nallow = [\"shell\"]\n");
    let (models, _mock, seen) = holding(vec![shell_turn("echo kept-output")], 1);
    let rt = runtime::build_with_models(&w, Manual, models, None, false, Default::default(), None);
    drop_mid_turn(&rt, &seen, "first task").await;
    // A failed fold leaves the interrupted turn pending in the journal, and a fold may have
    // left its mark; a reset must drop them with the conversation.
    assert_eq!(conversation(&rt, "main.inflight").await.len(), 3);
    let store = rt.session.store();
    store
        .write_file_async("main.inflight.fold", br#"{"log_len":0,"journal_bytes":1}"#)
        .await
        .unwrap();
    store
        .write_file_async(
            "main.inflight.settled",
            br#"{"log_len":0,"journal_bytes":1}"#,
        )
        .await
        .unwrap();
    rt.clear_conversation().await.unwrap();
    assert!(conversation(&rt, CONVERSATION_LOG).await.is_empty());
    assert!(conversation(&rt, "main.inflight").await.is_empty());
    for file in ["main.inflight.fold", "main.inflight.settled"] {
        match store.read_file_async(file).await {
            Ok(bytes) => assert!(bytes.is_empty(), "{file}: {bytes:?}"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{file}: {e}"),
        }
    }
}

/// What the TUI does for a typed prompt: note the checkpoint, record the message, run the
/// turn to its answer.
async fn say(runtime: &fontaine_core::agent::AgentRuntime, prompt: &str) {
    runtime.begin_turn(prompt, false);
    runtime.append(Record::UserMessage {
        text: prompt.to_string(),
        attachments: Vec::new(),
        agent: None,
    });
    let outcome = run_turn(params(runtime, prompt), &mut |_| Flow::Continue).await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
}

#[tokio::test]
async fn rewind_drops_the_pending_journal_of_the_interrupted_turn() {
    let w = World::new();
    w.user_file("[permissions]\nallow = [\"shell\"]\n");
    let (models, _mock, seen) = holding(
        vec![
            answer_turn("made a"),
            answer_turn("kept"),
            shell_turn("echo kept-output"),
        ],
        3,
    );
    let rt = runtime::build_with_models(&w, Manual, models, None, false, Default::default(), None);
    say(&rt, "make a").await;
    say(&rt, "keep me").await;
    // The next turn is interrupted mid-flight: its journal holds the prompt, the tool call
    // and its result, and a fold mark may sit beside it.
    rt.begin_turn("second task", false);
    drop_mid_turn(&rt, &seen, "second task").await;
    assert_eq!(conversation(&rt, "main.inflight").await.len(), 3);
    let store = rt.session.store();
    store
        .write_file_async("main.inflight.fold", br#"{"log_len":0,"journal_bytes":1}"#)
        .await
        .unwrap();
    // Rewinding before the interrupted prompt must not resurrect it from the journal.
    let plan = rt.rewind_plan(2).expect("turn two's checkpoint");
    rt.rewind(&plan, RewindWhat::Conversation, false)
        .await
        .unwrap();
    let main = conversation(&rt, CONVERSATION_LOG).await;
    assert_eq!(main.len(), 2, "{main:?}");
    assert!(main[0].contains("make a"), "{main:?}");
    assert!(conversation(&rt, "main.inflight").await.is_empty());
    match store.read_file_async("main.inflight.fold").await {
        Ok(bytes) => assert!(bytes.is_empty(), "{bytes:?}"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}"),
    }
}

#[tokio::test]
async fn a_failed_turn_keeps_its_prompt_in_the_conversation() {
    let w = World::new();
    let built = build(&w, Manual, vec![error_turn(400)], true);
    let outcome = run(&built, "hi").await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(built.main.request_count(), 1);
    let stored = stored(&built).await;
    assert!(stored.contains("hi"), "{stored}");
}

#[tokio::test]
async fn a_retryable_failure_is_retried_and_the_task_continues() {
    let w = World::new();
    let built = build(&w, Manual, vec![error_turn(429), answer_turn("done")], true);
    let outcome = run(&built, "hi").await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    assert_eq!(built.main.request_count(), 2);
    // The retry's request reads the failed attempt's prompt from the store and asks only to
    // go on: the continuation note is its new input, not a repeated user message.
    let requests = built.main.requests();
    let retry_history = format!("{:?}", requests[1].chat_history);
    assert!(retry_history.contains("hi"), "{retry_history}");
    assert!(
        retry_history.contains("continue the task"),
        "{retry_history}"
    );
    let stored = stored(&built).await;
    assert!(stored.contains("hi"), "{stored}");
    assert!(stored.contains("continue the task"), "{stored}");
    // Every attempt's record went with its settle: nothing outlives the retried run.
    let store = built.runtime.session.store();
    match store.read_file_async("main.inflight.run").await {
        Ok(bytes) => assert!(bytes.is_empty(), "{bytes:?}"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}"),
    }
}

#[tokio::test]
async fn a_non_retryable_failure_is_not_retried() {
    let w = World::new();
    let built = build(&w, Manual, vec![error_turn(400), answer_turn("done")], true);
    let outcome = run(&built, "hi").await;
    assert!(matches!(outcome, TurnOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(built.main.request_count(), 1);
}

#[tokio::test]
async fn retries_run_out_and_the_failure_is_reported_once() {
    let w = World::new();
    let turns = vec![error_turn(429), error_turn(429), error_turn(429)];
    let built = build(&w, Manual, turns, true);
    let outcome = run(&built, "hi").await;
    let TurnOutcome::Failed(message) = outcome else {
        panic!("expected a failure, got {outcome:?}");
    };
    assert!(message.contains("boom"), "{message}");
    assert_eq!(built.main.request_count(), 3);
}

#[tokio::test]
async fn a_run_start_is_recorded_for_the_fold_a_kill_before_it_would_read() {
    let w = World::new();
    let built = build(&w, Manual, vec![answer_turn("done")], true);
    built
        .runtime
        .record_run_start(user_message("hi", &[], &Default::default()), 0)
        .await
        .expect("the record writes");
    let store = built.runtime.session.store();
    let bytes = store.read_file_async("main.inflight.run").await.unwrap();
    assert!(!bytes.is_empty(), "{bytes:?}");
}

#[tokio::test]
async fn a_run_start_the_store_refuses_is_an_error_the_turn_must_not_run() {
    let w = World::new();
    let built = build(&w, Manual, vec![answer_turn("done")], true);
    // The record's file path is a directory, so writing it fails; the turn the surfaces
    // refused on this error must not start, since a kill in the unrecorded window that
    // follows would lose the prompt.
    let record_path = built
        .runtime
        .session
        .dir
        .join("files")
        .join("main.inflight.run");
    std::fs::create_dir_all(record_path.parent().unwrap()).unwrap();
    std::fs::create_dir(&record_path).unwrap();
    let result = built
        .runtime
        .record_run_start(user_message("hi", &[], &Default::default()), 0)
        .await;
    let e = result.expect_err("the refusal propagates to the caller");
    assert!(
        matches!(&e, rig_memory::SessionError::Io(_)),
        "the store's refusal is returned, not warned away: {e}"
    );
    assert_eq!(built.main.request_count(), 0, "the turn never ran");
}
