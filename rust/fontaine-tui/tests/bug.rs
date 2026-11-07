//! `/bug` end to end: the preview, cancelling, filing with a fake `gh`, and the URL fallback.
//! One test, because it changes the process `PATH`.

#![cfg(unix)]

mod common;

use common::{Ui, World, text_turn};
use ratatui::crossterm::event::KeyCode;
use std::path::Path;
use std::time::Duration;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn bundles(data: &Path) -> Vec<String> {
    std::fs::read_dir(data.join("bug-reports"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn bug_records(w: &World) -> Vec<serde_json::Value> {
    w.audit()
        .into_iter()
        .filter(|r| r["tool"] == "bug")
        .collect()
}

fn fake_gh(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    std::fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = auth ]; then exit 0; fi\necho https://github.com/madesroches/fontaine/issues/77\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bug_previews_cancels_and_files() {
    let bin = tempfile::tempdir().unwrap();
    // SAFETY: this is the only test in the binary, so nothing else reads the environment.
    unsafe { std::env::set_var("PATH", bin.path()) };

    // Preview, then Esc: nothing is written or recorded.
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/bug it crashes when I press x");
        ui.wait_for_live("This is everything that will be sent")
            .await;
        assert!(ui.live().contains("title: it crashes when I press x"));
        ui.press(KeyCode::Esc);
        ui.wait_for("nothing was written or sent").await;
    })
    .await;
    assert!(bundles(&w.data).is_empty());
    assert!(bug_records(&w).is_empty());
    assert!(!w.ledger_text().contains("BugReport"));

    // No gh on PATH: the URL fallback writes the bundle and the transcript stays out of the URL.
    let w = World::new(vec![text_turn("the answer")]);
    w.run(|ui| async move {
        ui.send("a distinctive question");
        ui.wait_for("the answer").await;
        idle(&ui).await;
        ui.send("/bug it crashes");
        ui.wait_for_live("This is everything that will be sent")
            .await;
        ui.press(KeyCode::Char('f'));
        tokio::time::sleep(Duration::from_millis(450)).await;
        ui.press(KeyCode::Char('f'));
        ui.wait_for_live("level: full").await;
        ui.press(KeyCode::Enter);
        ui.wait_for("open this URL to file the bug report").await;
    })
    .await;
    let saved = bundles(&w.data);
    assert_eq!(saved.len(), 1);
    assert!(saved[0].contains("a distinctive question"), "{}", saved[0]);
    let records = bug_records(&w);
    assert_eq!(records.len(), 1, "{records:?}");
    let ledger = w.ledger_text();
    let filed = ledger
        .lines()
        .find(|l| l.contains("bug_report"))
        .expect("the ledger records the report");
    assert!(filed.contains("\"url\""), "{filed}");
    assert!(!filed.contains("distinctive"), "{filed}");
    assert!(!records[0].to_string().contains("distinctive"));

    // A logged-in gh files the issue and its URL is recorded.
    fake_gh(bin.path());
    let w = World::new(vec![]);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/bug gh path");
        ui.wait_for_live("via gh issue create").await;
        tokio::time::sleep(Duration::from_millis(450)).await;
        ui.press(KeyCode::Enter);
        ui.wait_for("filed the bug report: https://github.com/madesroches/fontaine/issues/77")
            .await;
    })
    .await;
    assert_eq!(bundles(&w.data).len(), 1);
    assert!(w.ledger_text().contains("issues/77"));
    assert!(bug_records(&w)[0].to_string().contains("filed by gh"));
}
