mod common;

use std::path::Path;

use common::{World, git, git_init, write};
use fontaine_core::config::{Column, ConfigError};
use fontaine_core::trust::{self, LocalClassification, ProjectDemotion, classify_project_local};

const LOOSE: &str = "[permissions]\nallow = [\"shell(ls)\"]\n";

fn local_path(work: &Path) -> std::path::PathBuf {
    work.join(".fontaine/settings.local.toml")
}

fn classify(w: &World) -> LocalClassification {
    classify_project_local(&w.work, &local_path(&w.work), &w.data)
}

fn local_column(w: &World) -> Column {
    w.load()
        .unwrap()
        .scopes
        .iter()
        .find(|s| s.slot.label() == "project-local")
        .and_then(|s| s.column)
        .unwrap()
}

#[test]
fn an_untracked_file_in_a_trusted_root_loads_as_user_trust() {
    let w = World::new();
    w.trusted_repo();
    w.local_file(LOOSE);
    assert_eq!(classify(&w), LocalClassification::UserTrust);
    let c = w.load().unwrap();
    assert_eq!(c.permissions.allow.len(), 1);
    assert_eq!(local_column(&w), Column::ProjectLocal);
}

#[test]
fn an_ignored_file_in_a_trusted_root_loads_as_user_trust() {
    let w = World::new();
    w.trusted_repo();
    write(
        &w.work.join(".gitignore"),
        ".fontaine/settings.local.toml\n",
    );
    w.local_file(LOOSE);
    assert_eq!(classify(&w), LocalClassification::UserTrust);
}

#[test]
fn an_untracked_file_in_an_untrusted_root_loads_as_project_scope() {
    let w = World::new();
    git_init(&w.work);
    w.local_file(LOOSE);
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::UntrustedRoot(w.work.clone()))
    );

    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Scope { .. }), "{err:?}");
    assert_eq!(err.exit_code(), 2);
    let msg = err.to_string();
    assert!(msg.contains(&w.work.display().to_string()), "{msg}");
    assert!(msg.contains("trust.toml"), "{msg}");
    assert!(msg.contains("[[trusted]]"), "{msg}");

    // Tightening keys still load from the demoted file.
    w.local_file("[permissions]\ndeny = [\"shell(rm)\"]\n");
    assert_eq!(w.load().unwrap().permissions.deny.len(), 1);
    assert_eq!(local_column(&w), Column::Project);
}

#[test]
fn a_corrupt_git_index_loads_as_project_scope() {
    let w = World::new();
    w.trusted_repo();
    w.local_file(LOOSE);
    write(&w.work.join(".git/index"), "this is not an index");
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::GitError)
    );
    assert!(matches!(w.load().unwrap_err(), ConfigError::Scope { .. }));
}

#[cfg(unix)]
#[test]
fn a_fontaine_symlink_to_a_directory_outside_the_work_tree_loads_as_project_scope() {
    let w = World::new();
    w.trusted_repo();
    let outside = w.root.join("outside");
    write(&outside.join("settings.local.toml"), LOOSE);
    std::os::unix::fs::symlink(&outside, w.work.join(".fontaine")).unwrap();
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::OutsideWorkTree)
    );
    assert!(matches!(w.load().unwrap_err(), ConfigError::Scope { .. }));
}

#[test]
fn a_hand_made_git_skeleton_without_trust_loads_as_project_scope() {
    let w = World::new();
    write(&w.work.join(".git/HEAD"), "ref: refs/heads/main\n");
    std::fs::create_dir_all(w.work.join(".git/objects")).unwrap();
    std::fs::create_dir_all(w.work.join(".git/refs")).unwrap();
    w.local_file(LOOSE);
    assert!(matches!(classify(&w), LocalClassification::Project(_)));
    assert!(matches!(w.load().unwrap_err(), ConfigError::Scope { .. }));
}

#[test]
fn a_repo_with_no_index_file_counts_as_an_empty_index() {
    let w = World::new();
    w.trusted_repo();
    assert!(
        !w.work.join(".git/index").exists(),
        "fresh init has no index"
    );
    w.local_file(LOOSE);
    assert_eq!(classify(&w), LocalClassification::UserTrust);
}

#[test]
fn a_tracked_file_loads_as_project_scope() {
    let w = World::new();
    w.trusted_repo();
    w.local_file(LOOSE);
    git(&w.work, &["add", "-f", ".fontaine/settings.local.toml"]);
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::Tracked)
    );
    assert!(matches!(w.load().unwrap_err(), ConfigError::Scope { .. }));
}

#[test]
fn a_tracked_file_in_a_subdirectory_launch_is_found_through_repo_relative_paths() {
    let w = World::new();
    git_init(&w.work);
    let sub = w.work.join("sub/dir");
    write(&local_path(&sub), LOOSE);
    git(
        &w.work,
        &["add", "-f", "sub/dir/.fontaine/settings.local.toml"],
    );
    fontaine_core::trust::record(&w.data, &w.work).unwrap();
    assert_eq!(
        classify_project_local(&sub, &local_path(&sub), &w.data),
        LocalClassification::Project(ProjectDemotion::Tracked)
    );
}

#[cfg(unix)]
#[test]
fn a_tracked_symlinked_fontaine_dir_loads_as_project_scope() {
    let w = World::new();
    w.trusted_repo();
    write(&w.work.join("cfg/settings.local.toml"), LOOSE);
    std::os::unix::fs::symlink("cfg", w.work.join(".fontaine")).unwrap();
    git(
        &w.work,
        &["add", "-f", ".fontaine", "cfg/settings.local.toml"],
    );
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::Tracked)
    );
}

#[test]
fn a_case_variant_tracked_file_loads_as_project_scope() {
    let w = World::new();
    w.trusted_repo();
    write(&w.work.join(".fontaine/Settings.Local.toml"), LOOSE);
    git(&w.work, &["add", "-f", ".fontaine/Settings.Local.toml"]);
    std::fs::rename(
        w.work.join(".fontaine/Settings.Local.toml"),
        w.work.join(".fontaine/settings.local.toml"),
    )
    .unwrap();
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::Tracked)
    );
}

#[test]
fn a_force_added_file_matched_by_gitignore_loads_as_project_scope() {
    let w = World::new();
    w.trusted_repo();
    write(
        &w.work.join(".gitignore"),
        ".fontaine/settings.local.toml\n",
    );
    w.local_file(LOOSE);
    git(&w.work, &["add", "-f", ".fontaine/settings.local.toml"]);
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::Tracked)
    );
}

#[test]
fn a_directory_with_no_work_tree_loads_as_project_scope() {
    let w = World::new();
    w.local_file(LOOSE);
    fontaine_core::trust::record(&w.data, &w.work).unwrap();
    assert_eq!(
        classify(&w),
        LocalClassification::Project(ProjectDemotion::NoWorkTree)
    );
    assert!(matches!(w.load().unwrap_err(), ConfigError::Scope { .. }));
}

#[test]
fn a_launch_from_a_repo_subdirectory_classifies_an_untracked_file_as_user_trust() {
    let w = World::new();
    git_init(&w.work);
    trust::record(&w.data, &w.work).unwrap();
    let sub = w.work.join("pkg/app");
    write(&local_path(&sub), LOOSE);
    assert_eq!(
        classify_project_local(&sub, &local_path(&sub), &w.data),
        LocalClassification::UserTrust
    );
    let c = w.load_from(&sub, None, &common::no_env).unwrap();
    assert_eq!(c.permissions.allow.len(), 1);
}

#[test]
fn a_launch_from_a_git_worktree_classifies_an_untracked_file_as_user_trust() {
    let w = World::new();
    git_init(&w.work);
    git(&w.work, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let wt = w.root.join("wt");
    git(&w.work, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert!(wt.join(".git").is_file(), "a worktree has a .git file");
    let wt = std::fs::canonicalize(&wt).unwrap();
    trust::record(&w.data, &wt).unwrap();
    write(&local_path(&wt), LOOSE);
    assert_eq!(
        classify_project_local(&wt, &local_path(&wt), &w.data),
        LocalClassification::UserTrust
    );
}

#[test]
fn a_repo_fsmonitor_command_is_never_executed() {
    let w = World::new();
    w.trusted_repo();
    let canary = w.root.join("canary");
    let config = w.work.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[core]\n\tfsmonitor = echo ran > \"{}\"\n",
        canary.display()
    ));
    std::fs::write(&config, text).unwrap();
    w.local_file(LOOSE);

    let _ = classify(&w);
    let _ = trust::is_trusted(&w.data, &w.work);
    let _ = w.load();
    assert!(!canary.exists(), "fsmonitor command ran");
}
