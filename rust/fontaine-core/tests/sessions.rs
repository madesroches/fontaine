//! Resuming and forking: both stores come back, a fork is a copy under a new id, and the
//! newest session of a directory is found without a git or a database.

mod common;

use std::time::Duration;

use common::World;
use fontaine_core::ledger::Record;
use fontaine_core::session::{
    CONVERSATION_LOG, NameError, ResolveError, Session, SessionError, derive_name, latest_for_root,
    list, resolve, validate_name,
};
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

    // A session is held by one owner at a time; resume only after the first lets go.
    drop(source);
    let resumed = Session::open(&w.data, A).unwrap();
    assert_eq!(resumed.todos().root(), list);
    let fork = Session::fork(&w.data, A, B).unwrap();
    assert!(fork.dir.join("todos.json").is_file());
    assert_eq!(fork.todos().root(), list);
    fork.todos().clear().unwrap();
    assert_eq!(resumed.todos().root(), list);
}

#[tokio::test]
async fn the_schedule_is_reloaded_on_resume_and_copied_by_a_fork() {
    use fontaine_core::schedule::{JobSpec, Origin};
    let w = World::new();
    let source = Session::create(&w.data, A).unwrap();
    start(&source, &w.work);
    let job = source
        .scheduler()
        .create(JobSpec {
            cron: "*/5 * * * *".into(),
            prompt: "check the build".into(),
            recurring: true,
            origin: Origin::User,
        })
        .unwrap();
    assert!(source.dir.join("schedule.json").is_file());

    drop(source);
    let resumed = Session::open(&w.data, A).unwrap();
    assert_eq!(resumed.scheduler().take_warning(), None);
    assert_eq!(resumed.scheduler().list()[0].id, job.id);
    let fork = Session::fork(&w.data, A, B).unwrap();
    assert!(fork.dir.join("schedule.json").is_file());
    assert_eq!(fork.scheduler().list()[0].id, job.id);
    fork.scheduler().clear().unwrap();
    assert_eq!(resumed.scheduler().list().len(), 1);
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

const D: &str = "00000000-0000-4000-8000-00000000000d";

fn say_to_ledger(session: &Session, text: &str) {
    session
        .ledger
        .append(Record::UserMessage { text: text.into() })
        .unwrap();
}

#[test]
fn a_name_is_trimmed_bounded_and_free_of_control_characters() {
    assert_eq!(validate_name("  demo run "), Ok("demo run".to_string()));
    assert_eq!(validate_name("   "), Err(NameError::Empty));
    assert_eq!(validate_name(&"x".repeat(80)), Ok("x".repeat(80)));
    assert_eq!(validate_name(&"x".repeat(81)), Err(NameError::TooLong));
    assert_eq!(validate_name("a\nb"), Err(NameError::ControlCharacter));
    assert_eq!(validate_name("a\u{7}b"), Err(NameError::ControlCharacter));
    assert_eq!(validate_name(&"é".repeat(80)), Ok("é".repeat(80)));
}

#[test]
fn a_derived_name_is_the_first_line_cut_to_sixty_characters() {
    assert_eq!(
        derive_name("\n  fix the build\nthen deploy"),
        Some("fix the build".to_string())
    );
    assert_eq!(derive_name(&"y".repeat(100)), Some("y".repeat(60)));
    assert_eq!(derive_name("a\tb"), Some("a b".to_string()));
    assert_eq!(derive_name(" \n "), None);
}

#[test]
fn the_latest_name_wins_and_a_name_leaves_the_users_intent_alone() {
    let w = World::new();
    let session = Session::create(&w.data, A).unwrap();
    // Named before the start is recorded, as `--name` does.
    assert_eq!(session.rename(" first ").unwrap(), "first");
    start(&session, &w.work);
    say_to_ledger(&session, "fix the build");
    session.rename("second").unwrap();
    assert_eq!(session.name().unwrap().as_deref(), Some("second"));
    assert_eq!(session.ledger.view().user_messages, ["fix the build"]);
    assert_eq!(session.suggested_name().as_deref(), Some("fix the build"));
    assert!(session.rename("").is_err());
    assert_eq!(session.name().unwrap().as_deref(), Some("second"));

    drop(session);
    let again = Session::open(&w.data, A).unwrap();
    assert_eq!(again.name().unwrap().as_deref(), Some("second"));
    assert_eq!(again.ledger.view().user_messages, ["fix the build"]);
    // A session named before its start is still found for its directory.
    assert_eq!(latest_for_root(&w.data, &w.work).as_deref(), Some(A));
}

#[tokio::test]
async fn list_reports_each_session_newest_first_for_its_root_and_skips_unreadable_ones() {
    let w = World::new();
    let other = w.root.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();

    let old = Session::create(&w.data, A).unwrap();
    start(&old, &w.work);
    say_to_ledger(&old, "  first thing\nsecond line");
    old.rename("old one").unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let elsewhere = Session::create(&w.data, B).unwrap();
    start(&elsewhere, &other);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let fork = Session::fork_named(&w.data, A, C, None).unwrap();
    fork.ledger
        .append(Record::Resumed {
            source: A.into(),
            how: "fork (branch)".into(),
        })
        .unwrap();
    // A ledger that does not begin as one, a torn record and a directory that is no session.
    let foreign = Session::create(&w.data, D).unwrap();
    std::fs::write(foreign.ledger.path(), "not json\n").unwrap();
    std::fs::create_dir_all(w.data.join("sessions").join("not a session id")).unwrap();
    std::fs::create_dir_all(w.data.join("sessions").join("empty-dir")).unwrap();

    let here = list(&w.data, Some(&w.work));
    let ids: Vec<&str> = here.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, [C, A]);
    assert_eq!(here[1].name.as_deref(), Some("old one"));
    assert_eq!(here[1].first_prompt.as_deref(), Some("first thing"));
    assert_eq!(here[1].branched_from, None);
    assert_eq!(here[1].primary_root, w.work.display().to_string());
    // The copy's id is its directory's, and its name and parent are its own.
    assert_eq!(here[0].name.as_deref(), Some("old one (branch)"));
    assert_eq!(here[0].branched_from.as_deref(), Some(A));
    assert_eq!(list(&w.data, Some(&other)).len(), 1);
    assert_eq!(list(&w.data, None).len(), 3);

    // Writing to the older one makes it the newest again.
    tokio::time::sleep(Duration::from_millis(30)).await;
    say_to_ledger(&old, "again");
    assert_eq!(list(&w.data, Some(&w.work))[0].id, A);

    // A record cut short in the middle of the file is skipped, not fatal.
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(old.ledger.path())
        .unwrap();
    file.write_all(b"{\"seq\":99,\"kind\":\"user_mes\n")
        .unwrap();
    assert_eq!(list(&w.data, Some(&w.work)).len(), 2);
}

#[tokio::test]
async fn a_clear_resets_the_first_prompt_a_listing_shows() {
    let w = World::new();
    let session = Session::create(&w.data, A).unwrap();
    start(&session, &w.work);
    say_to_ledger(&session, "before");
    session.ledger.append(Record::ContextCleared {}).unwrap();
    assert_eq!(list(&w.data, Some(&w.work))[0].first_prompt, None);
    say_to_ledger(&session, "after");
    assert_eq!(
        list(&w.data, Some(&w.work))[0].first_prompt.as_deref(),
        Some("after")
    );
}

#[test]
fn a_fork_is_named_for_the_copy_alone() {
    let w = World::new();
    let source = Session::create(&w.data, A).unwrap();
    start(&source, &w.work);
    source.rename("demo").unwrap();

    let by_default = Session::fork_named(&w.data, A, B, None).unwrap();
    assert_eq!(by_default.name().unwrap().as_deref(), Some("demo (branch)"));
    let given = Session::fork_named(&w.data, A, C, Some("other")).unwrap();
    assert_eq!(given.name().unwrap().as_deref(), Some("other"));
    assert_eq!(source.name().unwrap().as_deref(), Some("demo"));

    // An unnamed source gives an unnamed copy; a long name leaves room for the suffix.
    let plain = Session::create(&w.data, D).unwrap();
    start(&plain, &w.work);
    let copy = Session::fork_named(&w.data, D, "e", None).unwrap();
    assert_eq!(copy.name().unwrap(), None);
    source.rename(&"n".repeat(80)).unwrap();
    let long = Session::fork_named(&w.data, A, "f", None).unwrap();
    let name = long.name().unwrap().unwrap();
    assert_eq!(name.chars().count(), 80);
    assert!(name.ends_with(" (branch)"));
}

#[test]
fn resolve_prefers_an_id_then_matches_names_in_the_root_without_regard_to_case() {
    let w = World::new();
    let other = w.root.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    let a = Session::create(&w.data, A).unwrap();
    start(&a, &w.work);
    a.rename("Demo").unwrap();
    let b = Session::create(&w.data, B).unwrap();
    start(&b, &w.work);
    b.rename("other").unwrap();
    let c = Session::create(&w.data, C).unwrap();
    start(&c, &other);
    c.rename("demo").unwrap();

    assert_eq!(resolve(&w.data, &w.work, A).as_deref(), Ok(A));
    assert_eq!(resolve(&w.data, &w.work, "demo").as_deref(), Ok(A));
    assert_eq!(resolve(&w.data, &w.work, "DEMO").as_deref(), Ok(A));
    // An id is taken whatever root it was started in; a name is not.
    assert_eq!(resolve(&w.data, &w.work, C).as_deref(), Ok(C));
    assert_eq!(resolve(&w.data, &other, "demo").as_deref(), Ok(C));
    assert_eq!(
        resolve(&w.data, &w.work, "missing"),
        Err(ResolveError::Missing("missing".into()))
    );
    for hostile in ["../x", "a/b", ""] {
        assert!(matches!(
            resolve(&w.data, &w.work, hostile),
            Err(ResolveError::Missing(_))
        ));
    }

    // A session directory named like a session name wins over the name.
    let by_dir = Session::create(&w.data, "other").unwrap();
    start(&by_dir, &w.work);
    assert_eq!(resolve(&w.data, &w.work, "other").as_deref(), Ok("other"));

    // Two sessions of one name are ambiguous, and the ids are offered.
    b.rename("demo").unwrap();
    let err = resolve(&w.data, &w.work, "demo").unwrap_err();
    match &err {
        ResolveError::Ambiguous { ids, .. } => {
            let mut ids = ids.clone();
            ids.sort();
            assert_eq!(ids, [A, B]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains(A) && err.to_string().contains(B));
}
