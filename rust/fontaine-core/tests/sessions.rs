//! Resuming and forking: both stores come back, a fork is a copy under a new id, and the
//! newest session of a directory is found without a git or a database.

mod common;

use std::time::Duration;

use common::World;
use fontaine_core::ledger::Record;
use fontaine_core::session::{CONVERSATION_LOG, Session, SessionError, latest_for_root};
use rig_core::message::Message;
use rig_memory::{load_async, save_async};

const A: &str = "00000000-0000-4000-8000-00000000000a";
const B: &str = "00000000-0000-4000-8000-00000000000b";
const C: &str = "00000000-0000-4000-8000-00000000000c";

fn start(session: &Session, root: &std::path::Path) {
    session
        .ledger
        .append(Record::SessionStart {
            session_id: session.id.clone(),
            permission_mode: "manual".into(),
            workspace_roots: vec![root.display().to_string()],
            workspace_trust: "trusted".into(),
            git_remotes: Vec::new(),
            env_keys: Vec::new(),
        })
        .unwrap();
}

async fn say(session: &Session, text: &str) {
    let store = session.store();
    let mut conversation = load_async(&store, CONVERSATION_LOG)
        .await
        .unwrap_or_default();
    conversation.push(Message::user(text));
    save_async(&store, CONVERSATION_LOG, &mut conversation)
        .await
        .unwrap();
}

async fn said(session: &Session) -> Vec<String> {
    let conversation = load_async(&session.store(), CONVERSATION_LOG)
        .await
        .unwrap();
    conversation
        .messages()
        .iter()
        .map(|m| format!("{m:?}"))
        .collect()
}

#[tokio::test]
async fn a_reopened_session_has_its_conversation_and_its_ledger() {
    let w = World::new();
    let first = Session::create(&w.data, A).unwrap();
    start(&first, &w.work);
    first
        .ledger
        .append(Record::UserMessage {
            text: "fix the build".into(),
        })
        .unwrap();
    first
        .ledger
        .append(Record::Grant {
            rule: "shell(cargo test)".into(),
        })
        .unwrap();
    say(&first, "fix the build").await;
    drop(first);

    let again = Session::open(&w.data, A).unwrap();
    assert_eq!(again.id, A);
    assert_eq!(again.ledger.view().user_messages, ["fix the build"]);
    assert!(said(&again).await[0].contains("fix the build"));
    // New records continue the sequence.
    again
        .ledger
        .append(Record::UserMessage {
            text: "and again".into(),
        })
        .unwrap();
    let entries = again.ledger.read().unwrap();
    let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [0, 1, 2, 3]);
    assert_eq!(again.ledger.view().user_messages.len(), 2);
}

#[tokio::test]
async fn a_fork_is_a_copy_under_a_new_id_and_the_original_is_untouched() {
    let w = World::new();
    let source = Session::create(&w.data, A).unwrap();
    start(&source, &w.work);
    say(&source, "original").await;
    source.audit.append(&common::audit_record(A)).unwrap();

    let fork = Session::fork(&w.data, A, B).unwrap();
    assert_eq!(fork.id, B);
    assert!(said(&fork).await[0].contains("original"));
    assert_eq!(fork.ledger.read().unwrap().len(), 1);
    // The fork's audit log starts empty: it records what the fork decides.
    assert!(fork.audit.read().unwrap().is_empty());

    say(&fork, "only in the fork").await;
    assert_eq!(said(&source).await.len(), 1);
    assert_eq!(said(&fork).await.len(), 2);
    fork.ledger
        .append(Record::UserMessage { text: "x".into() })
        .unwrap();
    assert_eq!(source.ledger.read().unwrap().len(), 1);
}

#[tokio::test]
async fn the_task_list_is_reloaded_on_resume_and_copied_by_a_fork() {
    use fontaine_tools::gate::ROOT_AGENT;
    use fontaine_tools::todo::{TodoItem, TodoStatus, TodoStore};
    let w = World::new();
    let source = Session::create(&w.data, A).unwrap();
    start(&source, &w.work);
    let list = vec![TodoItem {
        content: "ship it".to_string(),
        status: TodoStatus::InProgress,
    }];
    source.todos().replace(ROOT_AGENT, list.clone()).unwrap();

    assert_eq!(Session::open(&w.data, A).unwrap().todos().root(), list);
    let fork = Session::fork(&w.data, A, B).unwrap();
    assert!(fork.dir.join("todos.json").is_file());
    assert_eq!(fork.todos().root(), list);
    fork.todos().clear().unwrap();
    assert_eq!(source.todos().root(), list);
}

#[tokio::test]
async fn forking_onto_a_used_id_or_a_missing_source_is_a_usage_error() {
    let w = World::new();
    Session::create(&w.data, A).unwrap();
    Session::create(&w.data, B).unwrap();
    let in_use = Session::fork(&w.data, A, B).err().unwrap();
    assert!(matches!(in_use, SessionError::InUse(_)));
    assert_eq!(in_use.exit_code(), 2);
    let missing = Session::fork(&w.data, C, "00000000-0000-4000-8000-0000000000dd")
        .err()
        .unwrap();
    assert!(matches!(missing, SessionError::Missing(_)));
    assert_eq!(missing.exit_code(), 2);
    assert!(matches!(
        Session::open(&w.data, C).err().unwrap(),
        SessionError::Missing(_)
    ));
}

#[tokio::test]
async fn a_session_id_cannot_climb_out_of_the_sessions_directory() {
    let w = World::new();
    for id in ["../x", "a/b", "", "..", "a\\b"] {
        assert!(
            matches!(
                Session::open(&w.data, id).err().unwrap(),
                SessionError::Missing(_)
            ),
            "{id}"
        );
    }
}

#[tokio::test]
async fn the_newest_session_of_a_root_is_found_and_other_roots_are_ignored() {
    let w = World::new();
    let other = w.root.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();

    let old = Session::create(&w.data, A).unwrap();
    start(&old, &w.work);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let elsewhere = Session::create(&w.data, B).unwrap();
    start(&elsewhere, &other);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let newest = Session::create(&w.data, C).unwrap();
    start(&newest, &w.work);

    assert_eq!(latest_for_root(&w.data, &w.work).as_deref(), Some(C));
    assert_eq!(latest_for_root(&w.data, &other).as_deref(), Some(B));

    // Writing to an older session makes it the most recently updated.
    tokio::time::sleep(Duration::from_millis(30)).await;
    old.ledger
        .append(Record::UserMessage {
            text: "again".into(),
        })
        .unwrap();
    assert_eq!(latest_for_root(&w.data, &w.work).as_deref(), Some(A));

    let nowhere = w.root.join("nowhere");
    assert_eq!(latest_for_root(&w.data, &nowhere), None);
}

#[tokio::test]
async fn a_session_that_never_recorded_its_start_is_not_continued() {
    let w = World::new();
    Session::create(&w.data, A).unwrap();
    assert_eq!(latest_for_root(&w.data, &w.work), None);
}

#[tokio::test]
async fn the_sessions_files_are_owner_only() {
    let w = World::new();
    let first = Session::create(&w.data, A).unwrap();
    start(&first, &w.work);
    let fork = Session::fork(&w.data, A, B).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for session in [&first, &fork] {
            let mode = std::fs::metadata(session.ledger.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{:o}", mode);
        }
    }
    let _ = fork;
}

#[tokio::test]
async fn a_corrupt_ledger_reads_as_a_failure_to_open_not_to_create() {
    use std::error::Error;
    let w = World::new();
    let session = Session::create(&w.data, A).unwrap();
    start(&session, &w.root);
    drop(session);
    let ledger = w
        .data
        .join("sessions")
        .join(A)
        .join(fontaine_core::ledger::LEDGER_DIR)
        .join(fontaine_core::ledger::LEDGER_FILE);
    std::fs::write(&ledger, "not a record\nnot a record either\n").unwrap();
    let err = Session::open(&w.data, A).err().unwrap();
    assert!(err.to_string().starts_with("cannot open "), "{err}");
    assert!(err.source().is_some());
    assert_eq!(err.exit_code(), 1);
}

#[tokio::test]
async fn creating_over_a_file_where_the_directory_belongs_says_create() {
    let w = World::new();
    std::fs::create_dir_all(&w.data).unwrap();
    std::fs::write(w.data.join("sessions"), "x").unwrap();
    let err = Session::create(&w.data, A).err().unwrap();
    assert!(err.to_string().starts_with("cannot create "), "{err}");
}
