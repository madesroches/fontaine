mod common;

use std::io::Write;

use common::World;
use fontaine_core::{file_lock, owner_only};

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_file_is_owner_only_and_holds_the_content() {
    let w = World::new();
    let path = w.root.join("secret.txt");
    owner_only::write(&path, b"hello").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
    #[cfg(windows)]
    common::assert_current_user_only(&path);
}

#[test]
fn a_pre_existing_file_is_tightened_before_writing() {
    let w = World::new();
    let path = w.root.join("loose.txt");
    std::fs::write(&path, b"old content that is longer").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode(&path), 0o644);
    }

    let mut file = owner_only::open(&path, false).unwrap();
    // The restriction is already in force when the handle is returned, before any write.
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
    #[cfg(windows)]
    common::assert_current_user_only(&path);

    file.write_all(b"new").unwrap();
    drop(file);
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}

#[test]
fn append_keeps_existing_content() {
    let w = World::new();
    let path = w.root.join("log.txt");
    owner_only::write(&path, b"one\n").unwrap();
    let mut file = owner_only::open(&path, true).unwrap();
    file.write_all(b"two\n").unwrap();
    drop(file);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_new_directory_is_owner_only_and_creating_it_twice_reports_it_exists() {
    let w = World::new();
    let path = w.root.join("session");
    owner_only::create_dir(&path).unwrap();
    assert!(path.is_dir());
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o700);
    #[cfg(windows)]
    common::assert_current_user_only(&path);

    let again = owner_only::create_dir(&path).unwrap_err();
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    let orphan = owner_only::create_dir(&w.root.join("missing").join("child")).unwrap_err();
    assert_eq!(orphan.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn an_atomic_write_replaces_the_content_and_stays_owner_only() {
    let w = World::new();
    let path = w.root.join("atomic.txt");
    std::fs::write(&path, b"old content that is longer").unwrap();
    owner_only::write_atomic(&path, b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
    #[cfg(windows)]
    common::assert_current_user_only(&path);
    let leftovers = std::fs::read_dir(&w.root)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn concurrent_atomic_writes_to_one_path_leave_one_complete_file() {
    let w = World::new();
    let path = w.root.join("shared.txt");
    owner_only::write_atomic(&path, b"writer start").unwrap();
    let writing = std::sync::atomic::AtomicBool::new(true);
    std::thread::scope(|scope| {
        // An open reader handle is what a plain rename onto the target cannot replace.
        let reader = scope.spawn(|| {
            while writing.load(std::sync::atomic::Ordering::Relaxed) {
                // An unlocked reader can briefly lose the file on Windows; the writers must not.
                match std::fs::read_to_string(&path) {
                    Ok(text) => assert!(text.starts_with("writer "), "{text}"),
                    Err(e) => assert!(
                        matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                        ),
                        "{e}"
                    ),
                }
            }
        });
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..20 {
                        owner_only::write_atomic(path, format!("writer {i}").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        // Stop the reader before surfacing a writer's panic, or the scope would wait on it.
        let results: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        writing.store(false, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
        for result in results {
            result.unwrap();
        }
    });
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("writer "), "{text}");
    let leftovers: Vec<_> = std::fs::read_dir(&w.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_shared_lock_reader_never_sees_a_replace_in_progress() {
    let w = World::new();
    let path = w.root.join("locked.txt");
    owner_only::write_atomic(&path, b"writer start").unwrap();
    let writing = std::sync::atomic::AtomicBool::new(true);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            while writing.load(std::sync::atomic::Ordering::Relaxed) {
                let text = file_lock::read_shared(&path).unwrap();
                assert!(text.starts_with("writer "), "{text}");
            }
        });
        let writers: Vec<_> = (0..4)
            .map(|i| {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..10 {
                        let _lock = file_lock::lock(path).unwrap();
                        owner_only::write_atomic(path, format!("writer {i}").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        let results: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        writing.store(false, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
        for result in results {
            result.unwrap();
        }
    });
}

#[cfg(unix)]
#[test]
fn an_atomic_write_through_a_symlink_replaces_the_target_and_keeps_the_link() {
    let w = World::new();
    let target = w.root.join("target.txt");
    let link = w.root.join("link.txt");
    std::fs::write(&target, b"old").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    owner_only::write_atomic(&link, b"new").unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[cfg(unix)]
#[test]
fn an_atomic_write_through_a_relative_symlink_chain_replaces_the_final_target() {
    let w = World::new();
    std::fs::create_dir(w.root.join("sub")).unwrap();
    let target = w.root.join("sub").join("target.txt");
    let inner = w.root.join("sub").join("inner.txt");
    let outer = w.root.join("outer.txt");
    std::fs::write(&target, b"old").unwrap();
    std::os::unix::fs::symlink("target.txt", &inner).unwrap();
    std::os::unix::fs::symlink("sub/inner.txt", &outer).unwrap();
    owner_only::write_atomic(&outer, b"new").unwrap();
    assert!(std::fs::symlink_metadata(&outer).unwrap().is_symlink());
    assert!(std::fs::symlink_metadata(&inner).unwrap().is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[cfg(unix)]
#[test]
fn keeping_the_mode_preserves_it_while_write_atomic_tightens_it() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    let path = w.root.join("doc.toml");
    std::fs::write(&path, b"old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    owner_only::write_atomic_keeping_mode(&path, b"kept").unwrap();
    assert_eq!(mode(&path), 0o644);
    assert_eq!(std::fs::read(&path).unwrap(), b"kept");
    owner_only::write_atomic(&path, b"tight").unwrap();
    assert_eq!(mode(&path), 0o600);
}
