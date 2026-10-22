mod common;

use std::io::Write;

use common::World;
use fontaine_core::owner_only;

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
