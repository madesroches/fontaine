mod common;

use common::World;
use fontaine_core::ledger::{FILE_INDEX_CAP, FileIndex, Ledger, Record};
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::{CONVERSATION_LOG, CompactResult, Session, SessionError};
use rig_core::DynModel;
use rig_core::catalog::{Catalog, ModelSpec};
use rig_core::completion::{Cost, Message, Usage};
use rig_core::driver::{Exchange, Opening, Transport};
use rig_core::operation::Completion;
use rig_core::test_utils::{MockCompletionModel, MockScript, MockTurn};
use rig_core::wire::Wire;
use rig_core::{Model, ProviderError};
use rig_memory::{CompactReason, load, save, summary_state};

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
    assert!(
        matches!(usage, CompactResult::Compacted { usage: Some(_), .. }),
        "the summarizer answered: {usage:?}"
    );
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
    let result = session.compact_if_needed(&dynamic, true, &DenyAll).await;
    let CompactResult::Compacted {
        before: was,
        after: now,
        ..
    } = result
    else {
        panic!("{result:?}");
    };
    assert!(now < was);
    assert_eq!(mock.request_count(), 1);
    let after = saved_texts(&session);
    assert!(after.len() < before);
    assert!(after.iter().any(|m| m.contains("THRESHOLD-SUMMARY")));
}

#[tokio::test]
async fn an_unpriced_threshold_compaction_returns_usage_without_a_cost() {
    let cost: Cost = serde_json::from_value(serde_json::json!({"total": 0.25})).unwrap();
    let turn = || {
        MockTurn::text("THRESHOLD-SUMMARY")
            .with_usage(Usage::new().input_tokens(10).output_tokens(5).cost(cost))
    };
    for (priced, expect_cost) in [(false, false), (true, true)] {
        let w = World::new();
        let (session, _) = session_with_history(&w, 8);
        let (_, dynamic) = model(Some(small_window_spec(1_000)), vec![turn()]);
        let result = session.compact_if_needed(&dynamic, priced, &DenyAll).await;
        let usage = result.usage().expect("the summarizer answered");
        assert_eq!(usage.cost.is_some(), expect_cost, "priced = {priced}");
    }
}

#[tokio::test]
async fn a_conversation_that_fits_is_not_compacted() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 3);
    let (mock, dynamic) = model(
        Some(small_window_spec(1_000_000)),
        vec![MockTurn::text("unused")],
    );
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Skipped
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
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Skipped
    );
    assert_eq!(mock.request_count(), 0);
    let (mock, dynamic) = model(None, vec![MockTurn::text("unused")]);
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Skipped
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
    assert!(
        matches!(usage, CompactResult::Failed { usage: Some(_) }),
        "a call that answered is still paid for: {usage:?}"
    );
    assert_eq!(saved_texts(&session), before);
}

#[tokio::test]
async fn a_failed_summarizer_call_leaves_the_context_unchanged_and_does_not_fail() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let (mock, dynamic) = model(Some(small_window_spec(1_000)), vec![]);
    let before = saved_texts(&session);
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Failed { usage: None }
    );
    assert_eq!(mock.request_count(), 1);
    assert_eq!(saved_texts(&session), before);
}

#[test]
fn a_scheduled_record_is_kept_but_adds_nothing_to_what_the_user_asked() {
    use fontaine_core::ledger::ScheduleEvent;
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::create(dir.path()).unwrap();
    ledger
        .append(Record::UserMessage {
            text: "check the build every 5 minutes".into(),
        })
        .unwrap();
    for event in [
        ScheduleEvent::Created,
        ScheduleEvent::Fired,
        ScheduleEvent::Deleted,
        ScheduleEvent::Expired,
    ] {
        ledger
            .append(Record::Scheduled {
                event,
                id: "ab12cd34".into(),
                origin: "model".into(),
                cron: "*/5 * * * *".into(),
                recurring: true,
            })
            .unwrap();
    }
    let check = |ledger: &Ledger| {
        assert_eq!(
            ledger.view().user_messages,
            ["check the build every 5 minutes"]
        );
        assert_eq!(ledger.read().unwrap().len(), 5);
    };
    check(&ledger);
    let text = std::fs::read_to_string(ledger.path()).unwrap();
    assert!(text.contains("\"kind\":\"scheduled\"") && text.contains("\"event\":\"fired\""));
    assert!(
        !text.contains("prompt"),
        "a task's prompt is never recorded"
    );
    drop(ledger);
    check(&Ledger::open(dir.path()).unwrap());
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

#[test]
fn the_autocompact_trigger_is_the_lower_of_the_reserve_and_the_configured_window() {
    let w = World::new();
    let mut session = Session::create(&w.data, SESSION_ID).unwrap();
    let reserve = session.reserve(Some(&small_window_spec(200_000)));
    let known = small_window_spec(200_000);
    let mut unknown = small_window_spec(1);
    unknown.context_window = None;

    assert_eq!(
        session.autocompact_at(Some(&known)),
        Some(200_000 - reserve)
    );
    assert_eq!(session.autocompact_at(Some(&unknown)), None);
    assert_eq!(session.autocompact_at(None), None);

    session.set_autocompact_window(Some(50_000));
    assert_eq!(session.autocompact_at(Some(&known)), Some(50_000));
    assert_eq!(session.autocompact_at(Some(&unknown)), Some(50_000));
    assert_eq!(session.autocompact_at(None), None);
    assert!(session.over_threshold(50_001, &known));
    assert!(!session.over_threshold(50_000, &known));

    session.set_autocompact_window(Some(900_000));
    assert_eq!(
        session.autocompact_at(Some(&known)),
        Some(200_000 - reserve)
    );
    assert!(session.over_threshold(200_000 - reserve + 1, &known));
}

#[tokio::test]
async fn a_configured_window_compacts_a_conversation_the_reserve_would_leave_alone() {
    let w = World::new();
    let (mut session, _) = session_with_history(&w, 8);
    let spec = small_window_spec(100_000);
    let (mock, dynamic) = model(Some(spec.clone()), vec![MockTurn::text("unused")]);
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Skipped
    );
    assert_eq!(mock.request_count(), 0);

    // A summary about the size of the quarter of the window the summary is capped to.
    let summary = "s".repeat(1_500);
    session.set_autocompact_window(Some(2_000));
    let (mock, dynamic) = model(
        Some(spec),
        vec![MockTurn::text(summary.clone()), MockTurn::text("unused")],
    );
    let result = session.compact_if_needed(&dynamic, true, &DenyAll).await;
    let CompactResult::Compacted { after, .. } = result else {
        panic!("{result:?}");
    };
    assert!(
        after <= 2_000,
        "the conversation is under the window: {after}"
    );
    assert_eq!(mock.request_count(), 1);
    assert!(saved_texts(&session).iter().any(|m| m.contains(&summary)));
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Skipped,
        "the next turn does not summarize again"
    );
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn a_listed_model_without_a_window_is_compacted_only_when_a_window_is_configured() {
    let w = World::new();
    let (mut session, _) = session_with_history(&w, 8);
    let mut spec = small_window_spec(1_000);
    spec.context_window = None;
    let (mock, dynamic) = model(Some(spec.clone()), vec![MockTurn::text("SUMMARY")]);
    assert_eq!(
        session.compact_if_needed(&dynamic, true, &DenyAll).await,
        CompactResult::Skipped
    );
    assert_eq!(mock.request_count(), 0);

    session.set_autocompact_window(Some(2_000));
    let result = session.compact_if_needed(&dynamic, true, &DenyAll).await;
    assert!(
        matches!(result, CompactResult::Compacted { .. }),
        "{result:?}"
    );
    assert_eq!(mock.request_count(), 1);

    // A model the catalog does not list is never compacted automatically.
    let (mock, unlisted) = model(None, vec![MockTurn::text("unused")]);
    assert_eq!(
        session.compact_if_needed(&unlisted, true, &DenyAll).await,
        CompactResult::Skipped
    );
    assert_eq!(mock.request_count(), 0);
}

struct Collect(std::sync::Mutex<Vec<String>>);

impl fontaine_core::permissions::Approver for Collect {
    fn approve<'a>(
        &'a self,
        _request: fontaine_core::permissions::ApprovalRequest,
    ) -> futures::future::BoxFuture<'a, fontaine_core::permissions::Answer> {
        Box::pin(async { fontaine_core::permissions::Answer::Deny { feedback: None } })
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

#[tokio::test]
async fn asking_an_unlisted_model_to_summarize_warns_and_fails() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let (mock, unlisted) = model(None, vec![MockTurn::text("unused")]);
    let approver = Collect(Default::default());
    let result = session
        .compact_logged(
            &unlisted,
            true,
            CompactReason::Asked {
                focus: String::new(),
            },
            &approver,
        )
        .await;
    assert_eq!(result, CompactResult::Failed { usage: None });
    assert_eq!(mock.request_count(), 0);
    assert!(approver.0.lock().unwrap()[0].contains("catalog entry is unknown"));
}

/// A failure whose message omits its cause, as a transport error that wraps a TLS or DNS
/// failure does.
#[derive(Debug)]
struct Outer(Inner);

#[derive(Debug)]
struct Inner;

impl std::fmt::Display for Outer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("error sending request")
    }
}

impl std::fmt::Display for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tls handshake rejected the certificate")
    }
}

impl std::error::Error for Inner {}

impl std::error::Error for Outer {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Clone)]
struct NestedFailure;

impl Transport<MockScript> for NestedFailure {
    fn send(
        &self,
        _payload: <MockScript as Wire>::Payload,
        _exchange: Exchange,
    ) -> Opening<<MockScript as Wire>::Frame> {
        Opening::failed(ProviderError::request(Outer(Inner)))
    }
}

#[tokio::test]
async fn a_summarizer_failure_warns_with_its_whole_cause_chain() {
    let w = World::new();
    let (session, _) = session_with_history(&w, 8);
    let script = MockScript::new("failing").priced_as(small_window_spec(1_000));
    let failing: DynModel<Completion> = Model::new(script, NestedFailure).into();
    let approver = Collect(Default::default());
    let result = session
        .compact_logged(
            &failing,
            true,
            CompactReason::Asked {
                focus: String::new(),
            },
            &approver,
        )
        .await;
    assert_eq!(result, CompactResult::Failed { usage: None });
    let warnings = approver.0.lock().unwrap();
    assert!(
        warnings[0].contains("tls handshake rejected the certificate"),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn a_clear_empties_the_context_and_drops_the_old_summary_but_the_log_keeps_the_messages() {
    let w = World::new();
    let (session, questions) = session_with_history(&w, 8);
    let (mock, dynamic) = model(
        Some(small_window_spec(100_000)),
        vec![
            MockTurn::text("OLD-CONVERSATION-SUMMARY"),
            MockTurn::text("SECOND-SUMMARY"),
        ],
    );
    let asked = || CompactReason::Asked {
        focus: String::new(),
    };
    session.compact(&dynamic, asked(), &DenyAll).await;
    assert!(
        saved_texts(&session)
            .iter()
            .any(|m| m.contains("OLD-CONVERSATION-SUMMARY"))
    );

    session.clear().await.unwrap();
    assert!(saved_texts(&session).is_empty());
    let log = std::fs::read_to_string(
        session
            .dir
            .join("logs")
            .join(format!("{CONVERSATION_LOG}.jsonl")),
    )
    .unwrap();
    assert!(
        log.contains(&questions[0]),
        "the old messages stay in the log"
    );

    // The next conversation is summarized from scratch.
    let mut conversation = load(&session.store(), CONVERSATION_LOG).unwrap();
    assert!(summary_state(&conversation).summary.is_empty());
    for i in 0..6 {
        conversation.extend([
            Message::user(format!("fresh question {i}: {}", "ipsum ".repeat(200))),
            Message::assistant(format!("fresh answer {i}: {}", "dolor ".repeat(200))),
        ]);
    }
    save(&session.store(), CONVERSATION_LOG, &mut conversation).unwrap();
    session.compact(&dynamic, asked(), &DenyAll).await;
    assert_eq!(mock.request_count(), 2);
    let sent = format!("{:?}", mock.requests().last().unwrap());
    assert!(!sent.contains("OLD-CONVERSATION-SUMMARY"), "{sent}");
    assert!(!sent.contains("question number"), "{sent}");
}

#[test]
fn session_start_round_trips_with_and_without_env_keys() {
    let record = |env_keys: Vec<String>| Record::SessionStart {
        session_id: "s".into(),
        permission_mode: "manual".into(),
        workspace_roots: vec!["/w".into()],
        workspace_trust: "trusted".into(),
        git_remotes: Vec::new(),
        env_keys,
    };
    for keys in [vec![], vec!["HTTPS_PROXY".to_string(), "PATH".to_string()]] {
        let text = serde_json::to_string(&record(keys.clone())).unwrap();
        assert_eq!(text.contains("env_keys"), !keys.is_empty(), "{text}");
        assert_eq!(serde_json::from_str::<Record>(&text).unwrap(), record(keys));
    }
    let old = r#"{"kind":"session_start","session_id":"s","permission_mode":"manual","workspace_roots":["/w"],"workspace_trust":"trusted","git_remotes":[]}"#;
    assert_eq!(serde_json::from_str::<Record>(old).unwrap(), record(vec![]));
}

#[tokio::test]
async fn the_guardian_is_told_the_env_names_and_never_the_values() {
    use common::runtime::build;
    use fontaine_core::adjudication;
    use fontaine_core::config::PermissionMode::Manual;
    use fontaine_guardian::Profile;
    use fontaine_guardian::prompt_builder::{system_prompt, user_message};

    let w = World::new();
    w.user_file(
        "[env]\nRUST_LOG = \"warn\"\nHTTPS_PROXY = \"http://user:s3cr3t-pa55@proxy:3128\"\n",
    );
    let built = build(&w, Manual, Vec::new(), true);
    built.runtime.record_session_start().await;
    let request = adjudication::build(
        &built.runtime.state,
        "shell",
        &serde_json::json!({"command": "ls"}),
        Vec::new(),
        None,
    );
    assert_eq!(request.session_facts.env_keys, ["HTTPS_PROXY", "RUST_LOG"]);
    let message = user_message(&request);
    assert!(
        message.contains("subprocess_env_set_by_user: HTTPS_PROXY, RUST_LOG"),
        "{message}"
    );
    let ledger = std::fs::read_to_string(
        w.data
            .join("sessions")
            .join(&built.runtime.session.id)
            .join("ledger")
            .join("ledger.jsonl"),
    )
    .unwrap();
    assert!(ledger.contains("HTTPS_PROXY"));
    for sent in [
        ledger,
        message,
        system_prompt(Profile::Full, &request),
        serde_json::to_string(&request).unwrap(),
    ] {
        assert!(!sent.contains("s3cr3t-pa55"), "{sent}");
    }
}

#[tokio::test]
async fn a_corrupt_trust_store_is_reported_once_at_session_start() {
    use common::runtime::build_with_approver;
    use fontaine_core::config::PermissionMode::Manual;

    let w = World::new();
    std::fs::write(w.data.join("trust.toml"), "trusted = [[[").unwrap();
    let approver = std::sync::Arc::new(Collect(Default::default()));
    let handle = fontaine_core::models::ModelHandle::new(
        fontaine_core::models::ModelRef::parse("test/mock").unwrap(),
        fontaine_core::config::AccountKind::named("openai-compatible"),
        rig_core::test_utils::MockCompletionModel::from_stream_turns(Vec::<
            Vec<rig_core::test_utils::MockStreamEvent>,
        >::new())
        .into(),
    );
    let models = fontaine_core::models::Models {
        main: handle,
        main_listing: Vec::new(),
        guardian: None,
        guardian_choice: fontaine_core::models::GuardianChoice::MainFallback,
        notices: Vec::new(),
    };
    let runtime = build_with_approver(
        &w,
        Manual,
        models,
        None,
        true,
        Default::default(),
        None,
        approver.clone(),
    );
    runtime.record_session_start().await;
    let warnings = approver.0.lock().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("trust.toml"), "{warnings:?}");
}
