mod common;

use common::World;
use fontaine_core::workspace::{RootSource, resolve};

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
