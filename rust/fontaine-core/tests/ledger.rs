mod common;

use common::World;
use fontaine_core::ledger::{FILE_INDEX_CAP, FileIndex, Ledger, Record};
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::{CONVERSATION_LOG, Session, SessionError};
use rig_core::DynModel;
use rig_core::catalog::{Catalog, ModelSpec};
use rig_core::completion::Message;
use rig_core::operation::Completion;
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use rig_memory::{CompactReason, load, save};

const SESSION_ID: &str = "00000000-0000-4000-8000-0000000000aa";

fn small_window_spec(window: u32) -> ModelSpec {
    let resolved = Catalog::builtin().resolve("openai/gpt-4o-mini").unwrap();
    let mut spec: ModelSpec = (*resolved.spec).clone();
    spec.context_window = Some(window);
    spec
}

fn model(
    spec: Option<ModelSpec>,
    turns: Vec<MockTurn>,
) -> (MockCompletionModel, DynModel<Completion>) {
    let mock = MockCompletionModel::from_turns(turns);
    let mock = match spec {
        Some(spec) => mock.priced_as(spec),
        None => mock,
    };
    let dynamic: DynModel<Completion> = mock.clone().into();
    (mock, dynamic)
}

/// A session whose conversation holds `pairs` long user/assistant exchanges, each question also
/// recorded in the ledger as a human message.
fn session_with_history(w: &World, pairs: usize) -> (Session, Vec<String>) {
    let session = Session::create(&w.data, SESSION_ID).unwrap();
    let mut conversation = load(&session.store(), CONVERSATION_LOG).unwrap();
    let mut questions = Vec::new();
    for i in 0..pairs {
        let question = format!("question number {i}: {}", "lorem ipsum ".repeat(100));
        session
            .ledger
            .append(Record::UserMessage {
                text: question.clone(),
            })
            .unwrap();
        conversation.extend([
            Message::user(question.clone()),
            Message::assistant(format!("answer {i}: {}", "dolor sit ".repeat(100))),
        ]);
        questions.push(question);
    }
    save(&session.store(), CONVERSATION_LOG, &mut conversation).unwrap();
    (session, questions)
}

fn saved_texts(session: &Session) -> Vec<String> {
    load(&session.store(), CONVERSATION_LOG)
        .unwrap()
        .messages()
        .iter()
        .map(|m| format!("{m:?}"))
        .collect()
}

fn user_messages(session: &Session) -> Vec<String> {
    session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.record {
            Record::UserMessage { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_ledger_survives_a_compaction_the_user_asked_for() {
    let w = World::new();
    let (session, questions) = session_with_history(&w, 6);
    let (mock, dynamic) = model(
        Some(small_window_spec(100_000)),
        vec![MockTurn::text("SUMMARY-OF-THE-OLD-PART")],
    );
    let before = saved_texts(&session);

    let usage = session
        .compact(
            &dynamic,
            CompactReason::Asked {
                focus: "the lorem".to_string(),
            },
            &DenyAll,
        )
        .await;
    assert!(usage.is_some(), "the summarizer answered");
    assert_eq!(mock.request_count(), 1);

    let after = saved_texts(&session);
    assert!(after.len() < before.len(), "the context shrank");
    assert!(
        after.iter().any(|m| m.contains("SUMMARY-OF-THE-OLD-PART")),
        "the summary is in the context"
    );

    // The ledger is untouched: every human message verbatim, and no model-written text.
    assert_eq!(user_messages(&session), questions);
    let raw = std::fs::read_to_string(session.ledger.path()).unwrap();
    assert!(!raw.contains("SUMMARY-OF-THE-OLD-PART"));
}

#[tokio::test]
async fn a_threshold_compaction_between_runs_leaves_the_context_compacted_and_saved() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let (mock, dynamic) = model(
        Some(small_window_spec(1_000)),
        vec![MockTurn::text("THRESHOLD-SUMMARY")],
    );
    let before = saved_texts(&session).len();
    let usage = session.compact_if_needed(&dynamic, true, &DenyAll).await;
    assert!(usage.is_some());
    assert_eq!(mock.request_count(), 1);
    let after = saved_texts(&session);
    assert!(after.len() < before);
    assert!(after.iter().any(|m| m.contains("THRESHOLD-SUMMARY")));
}

#[tokio::test]
async fn a_conversation_that_fits_is_not_compacted() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 3);
    let (mock, dynamic) = model(
        Some(small_window_spec(1_000_000)),
        vec![MockTurn::text("unused")],
    );
    assert!(
        session
            .compact_if_needed(&dynamic, true, &DenyAll)
            .await
            .is_none()
    );
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn compaction_is_skipped_when_the_window_is_unknown() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let mut spec = small_window_spec(1_000);
    spec.context_window = None;
    let (mock, dynamic) = model(Some(spec), vec![MockTurn::text("unused")]);
    assert!(
        session
            .compact_if_needed(&dynamic, true, &DenyAll)
            .await
            .is_none()
    );
    assert_eq!(mock.request_count(), 0);
    let (mock, dynamic) = model(None, vec![MockTurn::text("unused")]);
    assert!(
        session
            .compact_if_needed(&dynamic, true, &DenyAll)
            .await
            .is_none()
    );
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn an_empty_summary_leaves_the_context_unchanged() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let (mock, dynamic) = model(Some(small_window_spec(1_000)), vec![MockTurn::text("")]);
    let before = saved_texts(&session);
    let usage = session.compact_if_needed(&dynamic, true, &DenyAll).await;
    assert_eq!(mock.request_count(), 1);
    assert!(usage.is_some(), "a call that answered is still paid for");
    assert_eq!(saved_texts(&session), before);
}

#[tokio::test]
async fn a_failed_summarizer_call_leaves_the_context_unchanged_and_does_not_fail() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let (mock, dynamic) = model(Some(small_window_spec(1_000)), vec![]);
    let before = saved_texts(&session);
    assert!(
        session
            .compact_if_needed(&dynamic, true, &DenyAll)
            .await
            .is_none()
    );
    assert_eq!(mock.request_count(), 1);
    assert_eq!(saved_texts(&session), before);
}

#[test]
fn the_ledger_is_append_only_and_owner_only() {
    let w = World::new();
    let session = Session::create(&w.data, SESSION_ID).unwrap();
    let ledger = &session.ledger;
    ledger
        .append(Record::UserMessage { text: "one".into() })
        .unwrap();
    let first = ledger.read().unwrap();
    ledger
        .append(Record::ModeChange {
            from: "manual".into(),
            to: "plan".into(),
        })
        .unwrap();
    ledger
        .append(Record::Denial {
            tool: "shell".into(),
            agent: "root".into(),
            reason: "no".into(),
            rule: None,
            summary: String::new(),
        })
        .unwrap();
    let all = ledger.read().unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[..1], first[..], "earlier records are never rewritten");
    let seqs: Vec<u64> = all.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [0, 1, 2]);

    // Beside the model context, outside the folders rig-memory reads.
    assert_eq!(
        ledger.path(),
        session.dir.join("ledger").join("ledger.jsonl")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(ledger.path()), 0o600);
        assert_eq!(mode(&session.dir), 0o700);
    }
}

#[test]
fn the_ledger_and_the_model_context_share_a_session_directory() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 1);
    assert!(session.dir.join("ledger").join("ledger.jsonl").is_file());
    assert!(session.dir.join("logs").is_dir());
    // rig-memory still opens the directory.
    rig_memory::DirStore::open(&session.dir).unwrap();
}

#[test]
fn a_reused_session_id_is_refused_and_leaves_the_first_ledger_alone() {
    let w = World::new();
    let first = Session::create(&w.data, SESSION_ID).unwrap();
    first
        .ledger
        .append(Record::UserMessage {
            text: "kept".into(),
        })
        .unwrap();
    let again = Session::create(&w.data, SESSION_ID).err().unwrap();
    assert!(matches!(&again, SessionError::InUse(id) if id == SESSION_ID));
    assert_eq!(again.exit_code(), 2);
    assert!(again.to_string().contains("already in use"));
    assert_eq!(first.ledger.read().unwrap().len(), 1);
}

#[test]
fn the_file_index_respects_gitignore() {
    let w = World::new();
    let root = &w.work;
    common::write(&root.join(".gitignore"), "ignored.txt\nbuild/\n");
    common::write(&root.join("kept.txt"), "k");
    common::write(&root.join("ignored.txt"), "i");
    common::write(&root.join("build/out.o"), "o");
    common::write(&root.join("src/main.rs"), "m");
    common::write(&root.join(".env"), "E=1");
    common::git_init(root);

    let index = FileIndex::build(std::slice::from_ref(root), FILE_INDEX_CAP);
    assert!(!index.truncated);
    let names: Vec<String> = index
        .paths
        .iter()
        .map(|p| {
            std::path::Path::new(p)
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert!(names.contains(&"kept.txt".to_string()), "{names:?}");
    assert!(names.contains(&"src/main.rs".to_string()));
    assert!(names.contains(&".env".to_string()));
    assert!(!names.contains(&"ignored.txt".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("build/")), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with(".git/")), "{names:?}");
}

#[test]
fn the_file_index_stops_at_the_cap_and_records_the_truncation() {
    let w = World::new();
    for i in 0..10 {
        common::write(&w.work.join(format!("f{i}.txt")), "x");
    }
    let capped = FileIndex::build(std::slice::from_ref(&w.work), 4);
    assert_eq!(capped.paths.len(), 4);
    assert!(capped.truncated);
    let exact = FileIndex::build(std::slice::from_ref(&w.work), 10);
    assert_eq!(exact.paths.len(), 10);
    assert!(!exact.truncated);
    assert_eq!(FILE_INDEX_CAP, 50_000);
}

#[test]
fn git_remotes_are_read_from_the_repository_config_without_credentials() {
    let w = World::new();
    common::git_init(&w.work);
    common::git(
        &w.work,
        &[
            "remote",
            "add",
            "origin",
            "https://user:token@example.com/org/repo.git",
        ],
    );
    let remotes = fontaine_core::ledger::git_remotes(&w.work);
    assert_eq!(remotes, ["https://example.com/org/repo.git"]);
    assert!(fontaine_core::ledger::git_remotes(&w.root.join("not-a-repo-dir")).is_empty());
}

#[test]
fn a_ledger_can_be_created_only_once_per_session_directory() {
    let w = World::new();
    let dir = w.data.join("sessions-direct");
    std::fs::create_dir_all(&dir).unwrap();
    let _first = Ledger::create(&dir).unwrap();
    assert!(Ledger::create(&dir).is_err());
}
