mod common;

use common::{World, git_init, write};
use fontaine_core::config::ConfigError;
use fontaine_core::trust::{
    self, LocalClassification, ProjectDemotion, Tracking, classify_project_local,
    tracked_or_holds_tracked, tracking,
};

#[test]
fn a_trusted_repo_root_trusts_its_subdirectories() {
    let w = World::new();
    git_init(&w.work);
    trust::record(&w.data, &w.work).unwrap();
    let sub = w.work.join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    assert!(trust::is_trusted(&w.data, &w.work));
    assert!(trust::is_trusted(&w.data, &sub));
}

#[test]
fn a_trusted_subdirectory_trusts_neither_its_sibling_nor_its_parent() {
    let w = World::new();
    git_init(&w.work);
    let a = w.work.join("a");
    let b = w.work.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    trust::record(&w.data, &a).unwrap();
    assert!(trust::is_trusted(&w.data, &a));
    assert!(!trust::is_trusted(&w.data, &b));
    assert!(!trust::is_trusted(&w.data, &w.work));
}

#[test]
fn an_entry_above_the_work_tree_root_trusts_neither_the_repo_nor_its_local_settings() {
    let w = World::new();
    let home = w.root.join("home");
    let repo = home.join("project");
    git_init(&repo);
    trust::record(&w.data, &home).unwrap();
    assert!(
        trust::is_trusted(&w.data, &home),
        "the entry itself is outside any repo"
    );
    assert!(!trust::is_trusted(&w.data, &repo));
    assert!(!trust::is_trusted(&w.data, &repo.join("src")));

    let file = repo.join(".fontaine/settings.local.toml");
    write(&file, "[permissions]\nallow = [\"shell\"]\n");
    assert_eq!(
        classify_project_local(&repo, &file, &w.data),
        LocalClassification::Project(ProjectDemotion::UntrustedRoot(repo.clone()))
    );
}

#[test]
fn outside_any_repo_a_trusted_ancestor_trusts_a_descendant() {
    let w = World::new();
    let parent = w.root.join("plain");
    let child = parent.join("x/y");
    std::fs::create_dir_all(&child).unwrap();
    trust::record(&w.data, &parent).unwrap();
    assert!(trust::is_trusted(&w.data, &child));
    assert!(!trust::is_trusted(&w.data, &w.root.join("other")));
}

#[test]
fn a_discovery_error_other_than_not_found_is_not_trusted() {
    let w = World::new();
    let home = w.root.join("home");
    let repo = home.join("project");
    git_init(&repo);
    write(&repo.join(".git/config"), "[core\n\tbroken = = =\n");
    trust::record(&w.data, &home).unwrap();
    trust::record(&w.data, &repo).unwrap();
    assert!(!trust::is_trusted(&w.data, &repo));
}

#[cfg(unix)]
#[test]
fn a_non_canonical_recorded_path_still_matches() {
    let w = World::new();
    let real = w.root.join("real");
    let project = real.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let link = w.root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    write(
        &w.data.join("trust.toml"),
        &format!("[[trusted]]\nroot = '{}'\n", link.join("project").display()),
    );
    assert!(trust::is_trusted(&w.data, &project));
    assert!(trust::is_trusted(&w.data, &link.join("project")));
}

#[test]
fn an_unresolvable_entry_is_ignored() {
    let w = World::new();
    write(
        &w.data.join("trust.toml"),
        &format!(
            "[[trusted]]\nroot = '{}'\n\n[[trusted]]\nroot = '{}'\n",
            w.root.join("gone").display(),
            w.work.display()
        ),
    );
    assert!(trust::is_trusted(&w.data, &w.work));
}

#[test]
fn the_record_is_written_under_the_data_dir_as_literal_strings() {
    let w = World::new();
    trust::record(&w.data, &w.work).unwrap();
    let path = w.data.join("trust.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        format!("[[trusted]]\nroot = '{}'\n", w.work.display())
    );
    assert!(!w.work.join("trust.toml").exists());
    assert!(!w.work.join(".fontaine").exists());

    // Recording twice adds nothing; a second root appends.
    trust::record(&w.data, &w.work).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    trust::record(&w.data, &w.root.join("config")).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .matches("[[trusted]]")
            .count(),
        2
    );
}

#[test]
fn recording_creates_a_missing_data_dir() {
    let w = World::new();
    let data = w.root.join("fresh/data");
    trust::record(&data, &w.work).unwrap();
    assert!(trust::is_trusted(&data, &w.work));
}

#[cfg(unix)]
#[test]
fn trust_toml_is_owner_only_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    trust::record(&w.data, &w.work).unwrap();
    let mode = std::fs::metadata(w.data.join("trust.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(windows)]
#[test]
fn trust_toml_is_current_user_only_on_windows() {
    let w = World::new();
    trust::record(&w.data, &w.work).unwrap();
    common::assert_current_user_only(&w.data.join("trust.toml"));
}

#[test]
fn an_unparseable_trust_toml_trusts_no_root() {
    let w = World::new();
    write(&w.data.join("trust.toml"), "[[trusted]\nroot = ");
    assert!(!trust::is_trusted(&w.data, &w.work));
    write(&w.data.join("trust.toml"), "[[trusted]]\nunknown = 1\n");
    assert!(!trust::is_trusted(&w.data, &w.work));
}

#[test]
fn recording_into_an_unparseable_trust_toml_is_an_error_not_an_overwrite() {
    let w = World::new();
    write(&w.data.join("trust.toml"), "garbage [");
    let err = trust::record(&w.data, &w.work).unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
    assert_eq!(
        std::fs::read_to_string(w.data.join("trust.toml")).unwrap(),
        "garbage ["
    );
}

#[test]
fn a_missing_trust_toml_trusts_nothing() {
    let w = World::new();
    assert!(!trust::is_trusted(&w.data, &w.work));
}

#[test]
fn a_directory_in_place_of_trust_toml_trusts_nothing() {
    let w = World::new();
    std::fs::create_dir_all(w.data.join("trust.toml")).unwrap();
    assert!(!trust::is_trusted(&w.data, &w.work));
}

#[test]
fn concurrent_records_and_approvals_all_land() {
    let w = World::new();
    let roots: Vec<_> = (0..6)
        .map(|i| {
            let root = w.root.join(format!("proj{i}"));
            std::fs::create_dir_all(&root).unwrap();
            root
        })
        .collect();
    std::thread::scope(|scope| {
        for (i, root) in roots.iter().enumerate() {
            let data = &w.data;
            scope.spawn(move || {
                trust::record(data, root).unwrap();
                trust::approve_server(data, root, "srv", &format!("digest{i}")).unwrap();
            });
        }
    });
    for (i, root) in roots.iter().enumerate() {
        assert!(trust::is_trusted(&w.data, root), "{root:?}");
        assert!(trust::server_approved(
            &w.data,
            root,
            "srv",
            &format!("digest{i}")
        ));
    }
}

#[test]
fn unreadable_reports_only_a_corrupt_store() {
    let w = World::new();
    assert!(trust::unreadable(&w.data).is_none());
    trust::record(&w.data, &w.work).unwrap();
    assert!(trust::unreadable(&w.data).is_none());
    std::fs::write(trust::trust_path(&w.data), "trusted = [[[").unwrap();
    let message = trust::unreadable(&w.data).unwrap();
    assert!(message.contains("trust.toml"), "{message}");
    assert!(!message.contains('\n'), "{message}");
}

#[test]
fn tracking_wants_an_exact_index_entry_inside_the_work_tree() {
    let w = World::new();
    git_init(&w.work);
    write(&w.work.join("kept.txt"), "a");
    write(&w.work.join("loose.txt"), "a");
    common::git(&w.work, &["add", "kept.txt"]);
    let work = std::fs::canonicalize(&w.work).unwrap();
    assert_eq!(tracking(&work, &work.join("kept.txt")), Tracking::Tracked);
    assert_eq!(
        tracking(&work, &work.join("loose.txt")),
        Tracking::Untracked
    );
    assert_eq!(tracking(&work, &work.join("new.txt")), Tracking::Untracked);
    assert_eq!(
        tracking(&work, &work.join("no/dir/new.txt")),
        Tracking::Unknown
    );
    // macOS volumes are case-insensitive by default: KEPT.txt resolves to kept.txt.
    #[cfg(all(unix, not(target_os = "macos")))]
    assert_eq!(tracking(&work, &work.join("KEPT.txt")), Tracking::Untracked);
    let outside = std::fs::canonicalize(&w.root).unwrap().join("x.txt");
    assert_eq!(tracking(&work, &outside), Tracking::Unknown);
}

#[test]
fn tracking_outside_any_repo_is_unknown() {
    let w = World::new();
    let plain = std::fs::canonicalize(w.root.join("plain")).unwrap_or_else(|_| {
        std::fs::create_dir_all(w.root.join("plain")).unwrap();
        std::fs::canonicalize(w.root.join("plain")).unwrap()
    });
    assert_eq!(tracking(&plain, &plain.join("a.txt")), Tracking::Unknown);
}

#[test]
fn tracked_or_holds_tracked_answers_for_files_and_directories() {
    let w = World::new();
    git_init(&w.work);
    write(&w.work.join("kept.txt"), "a");
    write(&w.work.join("loose.txt"), "a");
    write(&w.work.join("dir/inner.txt"), "a");
    write(&w.work.join("untracked_dir/x.txt"), "a");
    std::fs::create_dir_all(w.work.join("empty")).unwrap();
    common::git(&w.work, &["add", "kept.txt", "dir/inner.txt"]);
    let work = std::fs::canonicalize(&w.work).unwrap();
    let outside = std::fs::canonicalize(&w.root).unwrap().join("x.txt");
    let paths = [
        work.join("kept.txt"),
        work.join("dir"),
        work.join("loose.txt"),
        work.join("untracked_dir"),
        work.join("empty"),
        work.join("absent.txt"),
        work.join("no/dir/absent.txt"),
        outside,
        work.clone(),
    ];
    assert_eq!(
        tracked_or_holds_tracked(&work, &paths),
        [true, true, false, false, false, false, false, false, true]
    );
}

#[test]
fn the_work_tree_root_holds_tracked_files_only_with_a_non_empty_index() {
    let w = World::new();
    git_init(&w.work);
    let work = std::fs::canonicalize(&w.work).unwrap();
    assert_eq!(
        tracked_or_holds_tracked(&work, std::slice::from_ref(&work)),
        [false]
    );
}

#[test]
fn tracked_or_holds_tracked_outside_any_repo_is_false() {
    let w = World::new();
    std::fs::create_dir_all(w.root.join("plain")).unwrap();
    let plain = std::fs::canonicalize(w.root.join("plain")).unwrap();
    assert_eq!(
        tracked_or_holds_tracked(&plain, &[plain.join("a.txt"), plain.clone()]),
        [false, false]
    );
}
