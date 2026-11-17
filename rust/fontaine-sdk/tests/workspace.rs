#[path = "core_common/mod.rs"]
mod common;

use common::World;
use fontaine_sdk::core::workspace::{RootSource, resolve};

#[test]
fn added_directories_follow_the_primary_root_in_order_without_duplicates() {
    let w = World::new();
    let a = w.root.join("a");
    let b = w.root.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let roots = resolve(
        &w.work,
        &w.work,
        Some(&w.root),
        [
            (RootSource::Flag, a.clone()),
            (RootSource::Settings, b.clone()),
            (RootSource::Settings, a.clone()),
            (RootSource::Flag, w.work.clone()),
        ],
    );
    assert!(roots.warnings.is_empty(), "{:?}", roots.warnings);
    assert_eq!(roots.roots, [w.work.clone(), a, b]);
}

#[test]
fn relative_entries_resolve_against_the_launch_directory() {
    let w = World::new();
    let sibling = w.root.join("sibling");
    std::fs::create_dir_all(&sibling).unwrap();
    let roots = resolve(
        &w.work,
        &w.work,
        Some(&w.root),
        [(RootSource::Flag, std::path::PathBuf::from("../sibling"))],
    );
    assert_eq!(
        roots.roots,
        [w.work.clone(), sibling.canonicalize().unwrap()]
    );
}

#[test]
fn entries_that_cannot_be_roots_are_left_out_with_a_warning_naming_path_and_source() {
    let w = World::new();
    let file = w.root.join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let missing = w.root.join("missing");
    let roots = resolve(
        &w.work,
        &w.work,
        Some(&w.root),
        [
            (RootSource::Flag, missing.clone()),
            (RootSource::Settings, file.clone()),
        ],
    );
    assert_eq!(roots.roots, std::slice::from_ref(&w.work));
    assert_eq!(roots.warnings.len(), 2);
    assert!(roots.warnings[0].contains("--add-dir"));
    assert!(roots.warnings[0].contains(&missing.display().to_string()));
    assert!(roots.warnings[0].contains("does not exist"));
    assert!(roots.warnings[1].contains("additional_directories"));
    assert!(roots.warnings[1].contains(&file.display().to_string()));
    assert!(roots.warnings[1].contains("not a directory"));
}

fn one(
    w: &World,
    home: Option<&std::path::Path>,
    entry: &str,
) -> fontaine_sdk::core::workspace::Roots {
    resolve(
        &w.work,
        &w.work,
        home,
        [(RootSource::Flag, std::path::PathBuf::from(entry))],
    )
}

#[test]
fn a_leading_tilde_names_the_home_directory() {
    let w = World::new();
    let home = w.root.join("home");
    std::fs::create_dir_all(home.join("sub")).unwrap();
    let alone = one(&w, Some(&home), "~");
    assert!(alone.warnings.is_empty(), "{:?}", alone.warnings);
    assert_eq!(alone.roots, [w.work.clone(), home.canonicalize().unwrap()]);
    for entry in ["~/sub", "~/sub/"] {
        let roots = one(&w, Some(&home), entry);
        assert!(roots.warnings.is_empty(), "{:?}", roots.warnings);
        assert_eq!(
            roots.roots,
            [w.work.clone(), home.join("sub").canonicalize().unwrap()]
        );
    }
}

#[test]
fn a_tilde_after_the_first_component_and_a_named_user_stay_literal() {
    let w = World::new();
    let home = w.root.join("home");
    std::fs::create_dir_all(home.join("sub")).unwrap();
    std::fs::create_dir_all(w.work.join("a/~/sub")).unwrap();
    let deeper = one(&w, Some(&home), "a/~/sub");
    assert_eq!(
        deeper.roots,
        [
            w.work.clone(),
            w.work.join("a/~/sub").canonicalize().unwrap()
        ]
    );
    let user = one(&w, Some(&home), "~user/x");
    assert_eq!(user.roots, std::slice::from_ref(&w.work));
    assert!(user.warnings[0].contains("does not exist"));
}

#[test]
fn a_tilde_entry_is_left_out_when_the_home_is_unknown_even_if_a_tilde_directory_exists() {
    let w = World::new();
    std::fs::create_dir_all(w.work.join("~")).unwrap();
    let roots = one(&w, None, "~");
    assert_eq!(roots.roots, std::slice::from_ref(&w.work));
    assert_eq!(roots.warnings.len(), 1);
    assert!(roots.warnings[0].contains("`~`"));
    assert!(roots.warnings[0].contains("home directory is unknown"));
}
