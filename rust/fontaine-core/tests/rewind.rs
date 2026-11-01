//! `/rewind` on the runtime: files, conversation, ledger and picker, over scripted models.

mod common;

use std::sync::Arc;

use common::World;
use common::runtime::{Built, build};
use fontaine_core::checkpoint::{RewindWhat, Step, StoreCapture};
use fontaine_core::config::{Checkpoints, PermissionMode::AcceptEdits};
use fontaine_core::ledger::Record;
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::{CONVERSATION_LOG, Session};
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use fontaine_tools::checkpoint::PreImages;
use fontaine_tools::scope::Scope;
use rig_core::completion::{Message, Usage};
use rig_core::test_utils::{MockStreamEvent, mock_final};
use rig_memory::{load, save};
use serde_json::json;

fn call(name: &str, arguments: serde_json::Value) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments,
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

fn text(words: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(words.to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

/// What the TUI does for a typed prompt: note the checkpoint, record the message, run the turn.
async fn say(built: &Built, prompt: &str) {
    built.runtime.begin_turn(prompt, false);
    built.runtime.append(Record::UserMessage {
        text: prompt.to_string(),
        attachments: Vec::new(),
        agent: None,
    });
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: prompt.to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(5),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut |_| Flow::Continue,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
}

/// Turn one writes `a.txt`; turn two reads and rewrites it.
fn two_edit_turns() -> Vec<Vec<MockStreamEvent>> {
    vec![
        call("write", json!({"path": "a.txt", "content": "one"})),
        text("made a"),
        call("read", json!({"path": "a.txt"})),
        call(
            "edit",
            json!({"path": "a.txt", "edits": [{"old_text": "one", "new_text": "two"}]}),
        ),
        text("changed a"),
    ]
}

fn conversation_text(built: &Built) -> String {
    let conversation = load(&built.runtime.session.store(), CONVERSATION_LOG).unwrap();
    format!("{:?}", conversation.messages())
}

fn user_messages(built: &Built) -> Vec<String> {
    built.runtime.session.ledger.view().user_messages.clone()
}

#[tokio::test]
async fn rewinding_both_puts_back_the_file_the_conversation_the_prompt_and_the_ledger() {
    let w = World::new();
    let built = build(&w, AcceptEdits, two_edit_turns(), false);
    say(&built, "make a").await;
    say(&built, "change a").await;
    assert_eq!(
        std::fs::read_to_string(w.work.join("a.txt")).unwrap(),
        "two"
    );

    let turns = built.runtime.rewind_turns();
    assert_eq!(
        turns.iter().map(|t| (t.turn, t.files)).collect::<Vec<_>>(),
        [(1, 1), (2, 1)]
    );
    let plan = built.runtime.rewind_plan(1).unwrap();
    assert_eq!(plan.files.len(), 1);
    assert_eq!(plan.files[0].step, Step::Delete);

    let outcome = built
        .runtime
        .rewind(&plan, RewindWhat::Both, false)
        .await
        .unwrap();
    assert_eq!(outcome.prompt, "make a");
    assert!(!w.work.join("a.txt").exists());
    assert!(!conversation_text(&built).contains("make a"));
    assert!(user_messages(&built).is_empty());

    // The ledger only grew: both prompts and the file actions are still in it.
    let records = built.runtime.session.ledger.read().unwrap();
    let rewound: Vec<_> = records
        .iter()
        .filter_map(|e| match &e.record {
            Record::Rewound {
                turn,
                what,
                dropped_user_messages,
            } => Some((*turn, *what, *dropped_user_messages)),
            _ => None,
        })
        .collect();
    assert_eq!(rewound, [(1, RewindWhat::Both, 2)]);
    let prompts = records
        .iter()
        .filter(|e| matches!(e.record, Record::UserMessage { .. }))
        .count();
    assert_eq!(prompts, 2);
    assert!(
        records
            .iter()
            .any(|e| matches!(&e.record, Record::Action { tool, .. } if tool == "write"))
    );
}

#[tokio::test]
async fn a_code_rewind_restores_the_earliest_state_and_leaves_the_conversation() {
    let w = World::new();
    std::fs::write(w.work.join("keep.txt"), "untouched").unwrap();
    let built = build(&w, AcceptEdits, two_edit_turns(), false);
    say(&built, "make a").await;
    say(&built, "change a").await;
    let before = conversation_text(&built);

    let plan = built.runtime.rewind_plan(2).unwrap();
    assert!(matches!(plan.files[0].step, Step::Restore { .. }));
    built
        .runtime
        .rewind(&plan, RewindWhat::Code, false)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(w.work.join("a.txt")).unwrap(),
        "one"
    );
    assert_eq!(
        std::fs::read_to_string(w.work.join("keep.txt")).unwrap(),
        "untouched"
    );
    assert_eq!(conversation_text(&built), before);
    assert_eq!(user_messages(&built).len(), 2);
    // A code rewind hides nothing: the picker still offers both prompts.
    assert_eq!(built.runtime.rewind_turns().len(), 2);
    let records = built.runtime.session.ledger.read().unwrap();
    assert!(records.iter().any(|e| matches!(
        e.record,
        Record::Rewound {
            what: RewindWhat::Code,
            dropped_user_messages: 0,
            ..
        }
    )));
}

#[tokio::test]
async fn a_file_edited_since_is_flagged_and_kept_unless_the_overwrite_is_confirmed() {
    let w = World::new();
    let built = build(&w, AcceptEdits, two_edit_turns(), false);
    say(&built, "make a").await;
    say(&built, "change a").await;
    std::fs::write(w.work.join("a.txt"), "the user's own edit").unwrap();

    let plan = built.runtime.rewind_plan(2).unwrap();
    assert!(plan.files[0].changed_since);
    let kept = built
        .runtime
        .rewind(&plan, RewindWhat::Code, false)
        .await
        .unwrap();
    assert_eq!(kept.code.unwrap().kept_changed.len(), 1);
    assert_eq!(
        std::fs::read_to_string(w.work.join("a.txt")).unwrap(),
        "the user's own edit"
    );
    built
        .runtime
        .rewind(&plan, RewindWhat::Code, true)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(w.work.join("a.txt")).unwrap(),
        "one"
    );
}

#[tokio::test]
async fn a_new_prompt_after_a_rewind_gets_a_fresh_turn_and_the_old_ones_leave_the_picker() {
    let w = World::new();
    let mut turns = two_edit_turns();
    turns.push(call("write", json!({"path": "b.txt", "content": "bee"})));
    turns.push(text("made b"));
    let built = build(&w, AcceptEdits, turns, false);
    say(&built, "make a").await;
    say(&built, "change a").await;
    let plan = built.runtime.rewind_plan(2).unwrap();
    built
        .runtime
        .rewind(&plan, RewindWhat::Both, false)
        .await
        .unwrap();
    assert_eq!(
        built
            .runtime
            .rewind_turns()
            .iter()
            .map(|t| t.turn)
            .collect::<Vec<_>>(),
        [1]
    );
    say(&built, "make b").await;
    let shown: Vec<_> = built
        .runtime
        .rewind_turns()
        .iter()
        .map(|t| (t.turn, t.prompt.clone()))
        .collect();
    assert_eq!(
        shown,
        [(1, "make a".to_string()), (3, "make b".to_string())]
    );
}

#[tokio::test]
async fn the_guardian_request_after_a_rewind_omits_the_retracted_prompt_but_keeps_denials() {
    let w = World::new();
    let built = build(&w, AcceptEdits, two_edit_turns(), false);
    say(&built, "make a").await;
    built.runtime.append(Record::Denial {
        tool: "shell".into(),
        agent: "root".into(),
        reason: "no".into(),
        rule: Some("deny_rule".into()),
        summary: "rm -rf".into(),
    });
    say(&built, "change a").await;
    let plan = built.runtime.rewind_plan(2).unwrap();
    built
        .runtime
        .rewind(&plan, RewindWhat::Conversation, false)
        .await
        .unwrap();
    let request = fontaine_core::adjudication::build(
        &built.runtime.state,
        fontaine_tools::gate::ROOT_AGENT,
        "shell",
        &json!({"command": "ls"}),
        Vec::new(),
        None,
    );
    assert_eq!(request.recent_user_messages, ["make a"]);
    assert_eq!(request.prior_denials.len(), 1);
    assert_eq!(request.prior_denials[0].summary, "rm -rf");
}

#[tokio::test]
async fn after_a_clear_the_picker_lists_only_the_later_prompts() {
    let w = World::new();
    let built = build(
        &w,
        AcceptEdits,
        vec![text("one"), text("two"), text("three")],
        false,
    );
    say(&built, "before the clear").await;
    built.runtime.clear_conversation().await.unwrap();
    assert!(built.runtime.rewind_turns().is_empty());
    say(&built, "after the clear").await;
    say(&built, "and again").await;
    let shown: Vec<_> = built
        .runtime
        .rewind_turns()
        .iter()
        .map(|t| (t.turn, t.prompt.clone()))
        .collect();
    assert_eq!(
        shown,
        [
            (1, "after the clear".to_string()),
            (2, "and again".to_string())
        ]
    );
    assert!(built.runtime.rewind_plan(3).is_none());
}

#[tokio::test]
async fn a_rewind_is_refused_while_a_subagent_runs() {
    let w = World::new();
    let built = build(&w, AcceptEdits, vec![text("hi")], false);
    say(&built, "hello").await;
    let plan = built.runtime.rewind_plan(1).unwrap();
    built.runtime.state.agents().notify_report(
        fontaine_tools::gate::ROOT_AGENT,
        "agent-7",
        "done".to_string(),
    );
    let err = built
        .runtime
        .rewind(&plan, RewindWhat::Both, false)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        fontaine_core::agent::RewindError::SubagentsRunning
    ));
    assert!(conversation_text(&built).contains("hello"));
}

const SESSION: &str = "00000000-0000-4000-8000-0000000000bb";

fn configured(session: &Session) {
    session
        .checkpoints()
        .configure(Checkpoints::default(), None, Arc::new(DenyAll));
}

fn saved_pairs(session: &Session, from: usize, to: usize) {
    let mut conversation = load(&session.store(), CONVERSATION_LOG).unwrap();
    for i in from..to {
        conversation.extend([
            Message::user(format!("question {i}: {}", "lorem ipsum ".repeat(100))),
            Message::assistant(format!("answer {i}: {}", "dolor sit ".repeat(100))),
        ]);
    }
    save(&session.store(), CONVERSATION_LOG, &mut conversation).unwrap();
}

#[tokio::test]
async fn rewinding_across_a_compaction_restores_the_original_messages() {
    use rig_core::DynModel;
    use rig_core::operation::Completion;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use rig_memory::CompactReason;

    let w = World::new();
    let session = Session::create(&w.data, SESSION).unwrap();
    configured(&session);
    saved_pairs(&session, 0, 3);
    session.begin_turn("question 3", false).unwrap();
    let originals = format!(
        "{:?}",
        load(&session.store(), CONVERSATION_LOG).unwrap().messages()
    );
    saved_pairs(&session, 3, 8);

    let resolved = rig_core::catalog::Catalog::builtin()
        .resolve("openai/gpt-4o-mini")
        .unwrap();
    let mut spec: rig_core::catalog::ModelSpec = (*resolved.spec).clone();
    spec.context_window = Some(100_000);
    let mock = MockCompletionModel::from_turns(vec![MockTurn::text("SUMMARY-OF-THE-OLD-PART")])
        .priced_as(spec);
    let model: DynModel<Completion> = mock.into();
    let result = session
        .compact(
            &model,
            CompactReason::Asked {
                focus: String::new(),
            },
            &DenyAll,
        )
        .await;
    assert!(matches!(
        result,
        fontaine_core::session::CompactResult::Compacted { .. }
    ));
    let compacted = format!(
        "{:?}",
        load(&session.store(), CONVERSATION_LOG).unwrap().messages()
    );
    assert!(compacted.contains("SUMMARY-OF-THE-OLD-PART"));

    let plan = session
        .checkpoints()
        .plan(1, &Scope::new(w.work.clone(), []))
        .unwrap();
    session.rewind_conversation(plan.conv_n).await.unwrap();
    let restored = format!(
        "{:?}",
        load(&session.store(), CONVERSATION_LOG).unwrap().messages()
    );
    assert_eq!(restored, originals);
    assert!(!restored.contains("SUMMARY-OF-THE-OLD-PART"));
}

#[tokio::test]
async fn rewinding_to_a_turn_after_a_compaction_keeps_its_summary_state() {
    use rig_core::DynModel;
    use rig_core::operation::Completion;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use rig_memory::{CompactReason, summary_state, undo_summary};

    let w = World::new();
    let session = Session::create(&w.data, SESSION).unwrap();
    configured(&session);
    saved_pairs(&session, 0, 8);
    let resolved = rig_core::catalog::Catalog::builtin()
        .resolve("openai/gpt-4o-mini")
        .unwrap();
    let mut spec: rig_core::catalog::ModelSpec = (*resolved.spec).clone();
    spec.context_window = Some(100_000);
    let mock = MockCompletionModel::from_turns(vec![MockTurn::text("SUMMARY-OF-THE-OLD-PART")])
        .priced_as(spec);
    let model: DynModel<Completion> = mock.into();
    let asked = || CompactReason::Asked {
        focus: String::new(),
    };
    let result = session.compact(&model, asked(), &DenyAll).await;
    assert!(matches!(
        result,
        fontaine_core::session::CompactResult::Compacted { .. }
    ));
    let state = summary_state(&load(&session.store(), CONVERSATION_LOG).unwrap());
    assert_ne!(state, rig_memory::SummaryState::default());

    session.begin_turn("question after", false).unwrap();
    saved_pairs(&session, 8, 10);
    let plan = session
        .checkpoints()
        .plan(1, &Scope::new(w.work.clone(), []))
        .unwrap();
    session.rewind_conversation(plan.conv_n).await.unwrap();

    let mut restored = load(&session.store(), CONVERSATION_LOG).unwrap();
    assert_eq!(summary_state(&restored), state);
    undo_summary(
        &mut restored,
        &session.store(),
        CONVERSATION_LOG,
        rig_core::conversation::Author::plugin("test"),
    )
    .expect("the compaction before the rewind point can be undone");
    assert!(!format!("{:?}", restored.messages()).contains("SUMMARY-OF-THE-OLD-PART"));
}

#[tokio::test]
async fn a_fork_carries_the_checkpoints_and_a_code_restore_works_in_the_copy() {
    let w = World::new();
    let session = Session::create(&w.data, SESSION).unwrap();
    configured(&session);
    let file = w.work.join("f.txt");
    std::fs::write(&file, "original").unwrap();
    session.begin_turn("edit f", false).unwrap();
    let capture = StoreCapture(session.checkpoints().clone())
        .before("root", "edit", &file)
        .unwrap();
    std::fs::write(&file, "edited").unwrap();
    capture.finish(true);

    let fork = Session::fork(&w.data, SESSION, "00000000-0000-4000-8000-0000000000cc").unwrap();
    assert!(
        fork.dir
            .join("checkpoints")
            .join("manifest.jsonl")
            .is_file()
    );
    configured(&fork);
    let scope = Scope::new(w.work.clone(), []);
    let plan = fork.checkpoints().plan(1, &scope).unwrap();
    assert_eq!(plan.prompt, "edit f");
    let report = fork.checkpoints().restore_code(&plan, &scope, false);
    assert_eq!(report.restored, std::slice::from_ref(&file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
}

#[tokio::test]
async fn a_path_in_the_memory_directory_is_neither_captured_nor_restored() {
    let w = World::new();
    let memory = w.enable_memory();
    let session = Session::create(&w.data, SESSION).unwrap();
    session.checkpoints().configure(
        Checkpoints::default(),
        Some(memory.clone()),
        Arc::new(DenyAll),
    );
    session.begin_turn("remember", false).unwrap();
    let note = memory.join("note.md");
    std::fs::write(&note, "n").unwrap();
    assert!(
        StoreCapture(session.checkpoints().clone())
            .before("root", "write", &note)
            .is_none()
    );
    assert_eq!(session.checkpoints().turns()[0].files, 0);
    let scope = Scope::new(w.work.clone(), [memory.clone()]);
    let plan = session.checkpoints().plan(1, &scope).unwrap();
    assert!(plan.files.is_empty());
}
