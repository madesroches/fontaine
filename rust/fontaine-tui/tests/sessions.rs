//! `/resume`, `/branch`, `/rename` and `/export`: the interface asks its caller for another
//! session through `drive`'s outcome, and names and exports land on disk.

mod common;

use std::path::Path;

use common::routed::{Route, Turn, start};
use common::{Ui, World, text_turn, tool_turn};
use fontaine_core::ledger::Record;
use fontaine_core::session::Session;
use fontaine_tui::{DriveOutcome, SessionTarget};
use ratatui::crossterm::event::KeyCode;
use serde_json::json;

const OLD: &str = "00000000-0000-4000-8000-0000000000a1";
const OLDER: &str = "00000000-0000-4000-8000-0000000000a2";

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

/// A session of an earlier run in `w`'s workspace, as `--continue` would find it.
fn earlier(w: &World, id: &str, name: Option<&str>, prompt: &str) {
    let session = Session::create(&w.data, id).unwrap();
    session
        .ledger
        .append(Record::SessionStart {
            session_id: id.into(),
            permission_mode: "manual".into(),
            workspace_roots: vec![w.work.display().to_string()],
            workspace_trust: "trusted".into(),
            git_remotes: Vec::new(),
            env_keys: Vec::new(),
        })
        .unwrap();
    session
        .ledger
        .append(Record::UserMessage {
            text: prompt.into(),
            agent: None,
        })
        .unwrap();
    if let Some(name) = name {
        session.rename(name).unwrap();
    }
}

fn ledger_of(w: &World, id: &str) -> String {
    std::fs::read_to_string(
        w.data
            .join("sessions")
            .join(id)
            .join("ledger")
            .join("ledger.jsonl"),
    )
    .unwrap()
}

fn mentions(dir: &Path, needle: &str) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            mentions(&path, needle)
        } else {
            std::fs::read(&path)
                .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                .unwrap_or(false)
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_copies_the_session_and_asks_the_caller_to_carry_on_in_the_copy() {
    let w = World::new(vec![text_turn("hi there")]);
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("hello world");
            ui.wait_for("hi there").await;
            idle(&ui).await;
            ui.send("/branch variant");
        })
        .await;
    let DriveOutcome::Switch(SessionTarget {
        source,
        record_resume,
    }) = outcome
    else {
        panic!("expected a switch, got {outcome:?}");
    };
    assert!(!record_resume, "the copy's own Resumed is already written");
    assert_ne!(source, w.session_id());

    let copy = ledger_of(&w, &source);
    assert!(copy.contains("\"name\":\"variant\""), "{copy}");
    assert!(copy.contains("\"how\":\"fork (branch)\""), "{copy}");
    assert!(copy.contains("hello world"));
    assert!(mentions(&w.data.join("sessions").join(&source), "hi there"));
    // The original is left as it was.
    let original = w.ledger_text();
    assert!(!original.contains("session_named") && !original.contains("fork"));
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_names_the_copy_after_the_source_when_no_name_is_given() {
    let w = World::new(vec![]);
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("/rename base");
            ui.wait_for("session renamed to base").await;
            ui.send("/fork");
        })
        .await;
    let DriveOutcome::Switch(target) = outcome else {
        panic!("expected a switch");
    };
    assert!(ledger_of(&w, &target.source).contains("\"name\":\"base (branch)\""));
    assert!(w.ledger_text().contains("\"name\":\"base\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_with_a_bad_name_stays_in_the_session() {
    let w = World::new(vec![]);
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send(&format!("/branch {}", "x".repeat(81)));
            ui.wait_for("at most 80 characters").await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    assert_eq!(
        std::fs::read_dir(w.data.join("sessions")).unwrap().count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_is_refused_while_a_subagent_runs() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "worker", "prompt": "child-A: finish", "background": true}),
                ),
                Turn::text("launched"),
                Turn::text("noted the report"),
            ],
        ),
        Route::new("child-A", vec![Turn::text("child done")]).delay(0, 2500),
    ])
    .await;
    let w = World::routed_known(&server.uri(), "\n[permissions]\nallow = [\"agent\"]\n");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("root task");
            ui.wait_for("launched").await;
            idle(&ui).await;
            ui.send("/branch");
            ui.wait_for("refused").await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    assert_eq!(
        std::fs::read_dir(w.data.join("sessions")).unwrap().count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_by_name_or_id_switches_and_an_unknown_one_is_an_error() {
    let w = World::new(vec![]);
    earlier(&w, OLD, Some("Demo"), "earlier work");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("/resume nobody");
            ui.wait_for("no session with ID or name nobody").await;
            ui.send("/resume demo");
        })
        .await;
    assert_eq!(
        outcome,
        DriveOutcome::Switch(SessionTarget {
            source: OLD.into(),
            record_resume: true
        })
    );

    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send(&format!("/resume {OLD}"));
        })
        .await;
    assert!(matches!(outcome, DriveOutcome::Switch(t) if t.source == OLD));
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_with_two_sessions_of_a_name_lists_them_and_stays() {
    let w = World::new(vec![]);
    earlier(&w, OLD, Some("twin"), "one");
    earlier(&w, OLDER, Some("twin"), "two");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("/resume twin");
            ui.wait_for("several sessions are named twin").await;
            // The ids are long enough to wrap, so only their last group is looked for.
            let screen = ui.screen();
            assert!(screen.contains("0000000000a1") && screen.contains("0000000000a2"));
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resume_picker_lists_other_sessions_filters_and_switches() {
    let w = World::new(vec![]);
    earlier(&w, OLD, Some("Demo"), "earlier work");
    earlier(&w, OLDER, None, "write the docs");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("/resume");
            ui.wait_for("sessions started in this directory").await;
            let screen = ui.screen();
            assert!(screen.contains("Demo") && screen.contains("write the docs"));
            assert!(!screen.contains("no other session"));
            ui.type_text("docs");
            ui.wait_until("the filter to narrow", |s| !s.contains("Demo  "))
                .await;
            ui.press(KeyCode::Enter);
        })
        .await;
    assert_eq!(
        outcome,
        DriveOutcome::Switch(SessionTarget {
            source: OLDER.into(),
            record_resume: true
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resume_picker_without_other_sessions_says_so_and_escape_closes_it() {
    let w = World::new(vec![]);
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("/resume");
            ui.wait_for("no other session was started in this directory")
                .await;
            ui.press(KeyCode::Esc);
            idle(&ui).await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resume_picker_reads_out_plainly_in_screen_reader_mode() {
    let w = World::new(vec![]);
    earlier(&w, OLD, Some("Demo"), "earlier work");
    let bytes = std::rc::Rc::default();
    let outcome = w
        .run_linear(bytes, |ui| async move {
            ui.wait_for("> ").await;
            ui.send("/resume");
            ui.wait_for("selected: Demo (just now, 00000000)").await;
            ui.press(KeyCode::Enter);
        })
        .await;
    assert!(matches!(outcome, DriveOutcome::Switch(t) if t.source == OLD));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_branch_with_later_input_is_refused_and_replay_carries_on() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
        text_turn("msg reply"),
    ])
    .with_config("[permissions]\nallow = [\"shell(sleep *)\"]\n");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("start");
            ui.wait_for("• shell sleep 1").await;
            ui.send("/branch");
            ui.wait_for("queued: /branch").await;
            ui.send("keep me");
            ui.wait_for("queued: keep me").await;
            ui.wait_for("answer").await;
            // The switch is refused while input waits; the message runs in this session.
            ui.wait_for("/branch is refused while queued input still waits")
                .await;
            ui.wait_for("msg reply").await;
            idle(&ui).await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    assert_eq!(
        std::fs::read_dir(w.data.join("sessions")).unwrap().count(),
        1,
        "no branch was created"
    );
    assert!(w.ledger_text().contains("keep me"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resume_picker_choice_with_later_input_is_refused_and_replay_carries_on() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 1"})),
        text_turn("answer"),
        text_turn("msg reply"),
    ])
    .with_config("[permissions]\nallow = [\"shell(sleep *)\"]\n");
    earlier(&w, OLD, Some("Demo"), "earlier work");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("start");
            ui.wait_for("• shell sleep 1").await;
            ui.send("/resume");
            ui.wait_for("queued: /resume").await;
            ui.send("keep me");
            ui.wait_for("queued: keep me").await;
            ui.wait_for("answer").await;
            // The queued /resume opens the picker; choosing a session is refused.
            ui.wait_for("sessions started in this directory").await;
            ui.press(KeyCode::Enter);
            ui.wait_for("/resume is refused while queued input still waits")
                .await;
            ui.wait_for("msg reply").await;
            idle(&ui).await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit, "no session switch happened");
    assert!(
        w.ledger_text().contains("keep me"),
        "the message is ledgered in the original session"
    );
    assert!(
        !ledger_of(&w, OLD).contains("keep me"),
        "the other session was never entered"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resume_pick_deferred_from_a_notification_preserves_queued_input() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "worker", "prompt": "child-A: finish", "background": true}),
                ),
                Turn::text("launched"),
                Turn::text("noted the report"),
                Turn::text("queued reply"),
            ],
        )
        .delay(2, 3000),
        Route::new("child-A", vec![Turn::text("child done")]).delay(0, 1500),
    ])
    .await;
    let w = World::routed(&server.uri(), "\n[permissions]\nallow = [\"agent\"]\n");
    earlier(&w, OLD, Some("Demo"), "earlier work");
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("root task");
            ui.wait_for("launched").await;
            idle(&ui).await;
            ui.send("/resume");
            ui.wait_for("sessions started in this directory").await;
            ui.wait_for("a background agent reported back").await;
            ui.press(KeyCode::Enter);
            ui.send("keep me through the deferred pick");
            ui.wait_for("queued: keep me through the deferred pick")
                .await;
            ui.wait_for("/resume is refused while queued input still waits")
                .await;
            ui.wait_for("queued reply").await;
            idle(&ui).await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit, "no session switch happened");
    assert!(
        w.ledger_text()
            .contains("keep me through the deferred pick"),
        "the queued message ran in the original session"
    );
    assert!(
        !ledger_of(&w, OLD).contains("keep me through the deferred pick"),
        "the other session was never entered"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_names_the_session_and_derives_a_name_from_the_first_message() {
    let w = World::new(vec![text_turn("on it")]);
    let outcome = w
        .run(|ui| async move {
            idle(&ui).await;
            ui.send("/rename");
            ui.wait_for("there is no message to draw a name from").await;
            ui.send("fix the failing build\nplease");
            ui.wait_for("on it").await;
            idle(&ui).await;
            ui.send("/rename");
            ui.wait_for("session renamed to fix the failing build")
                .await;
            ui.send("/rename  Better name ");
            ui.wait_for("session renamed to Better name").await;
            ui.send(&format!("/rename {}", "x".repeat(81)));
            ui.wait_for("at most 80 characters").await;
        })
        .await;
    assert_eq!(outcome, DriveOutcome::Exit);
    let ledger = w.ledger_text();
    assert!(
        ledger.contains("\"name\":\"fix the failing build\""),
        "{ledger}"
    );
    assert!(ledger.contains("\"name\":\"Better name\""), "{ledger}");
    assert!(!ledger.contains("xxxxxxxx"));
}

#[tokio::test(flavor = "multi_thread")]
async fn export_writes_the_conversation_once_and_never_overwrites() {
    let w = World::new(vec![text_turn("hi there")]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("hello world");
        ui.wait_for("hi there").await;
        idle(&ui).await;
        ui.send("/export notes.md");
        ui.wait_for("exported the conversation to").await;
        ui.send("/export notes.md");
        ui.wait_for("notes.md already exists").await;
        ui.send("/export");
        // The default path is long on a temp dir, so the notice wraps; count the notices.
        ui.wait_until("a second export notice", |s| {
            s.matches("exported the conversation to").count() >= 2
        })
        .await;
    })
    .await;
    let text = std::fs::read_to_string(w.work.join("notes.md")).unwrap();
    assert!(text.contains("## User\n\nhello world"), "{text}");
    assert!(text.contains("## Assistant\n\nhi there"), "{text}");
    assert!(w.work.join("fontaine-00000000.md").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(w.work.join("notes.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}
