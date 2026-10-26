//! Cross-process exclusion for read-modify-write of a persisted file: an exclusive lock on a
//! sibling `.lock` file, held until the guard drops.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Holds the exclusive lock until dropped.
pub struct FileLock(File);

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_path(target: &Path) -> PathBuf {
    target.with_extension("lock")
}

/// Blocks until the lock for `target` is held: `target` with its extension swapped for `lock`
/// (`credentials.toml` -> `credentials.lock`), created beside it along with the parent
/// directory. The path is used as given, never symlink-resolved, so a linked settings file
/// keeps its lock beside the link.
pub fn lock(target: &Path) -> io::Result<FileLock> {
    let path = lock_path(target);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = crate::owner_only::open_lock(&path)?;
    file.lock()?;
    Ok(FileLock(file))
}

/// Reads `target` while holding the shared lock beside it, so a session replacing the file under
/// the exclusive lock is waited for instead of observed mid-rename. Must not be called by a
/// holder of [`lock`] for the same target: the shared lock would wait on its own process.
pub fn read_shared(target: &Path) -> io::Result<String> {
    let path = lock_path(target);
    // A read never creates directories: without the parent, the target cannot exist either.
    if path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .is_some_and(|p| !p.exists())
    {
        return std::fs::read_to_string(target);
    }
    let file = match crate::owner_only::open_lock(&path) {
        Ok(file) => file,
        // A directory we cannot create a file in is one writers cannot replace a file in, and a
        // read-only mounted config must keep loading.
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            return std::fs::read_to_string(target);
        }
        Err(e) => return Err(e),
    };
    // Unsupported, or ENOLCK on NFS (uncategorized in std): reads never needed the lock before.
    if file.lock_shared().is_err() {
        return std::fs::read_to_string(target);
    }
    let _guard = FileLock(file);
    std::fs::read_to_string(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_reader_waits_for_the_exclusive_holder_and_sees_its_write() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("shared.toml");
        crate::owner_only::write_atomic(&target, b"old").unwrap();
        let guard = lock(&target).unwrap();
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| read_shared(&target).unwrap());
            std::thread::sleep(Duration::from_millis(200));
            assert!(!reader.is_finished());
            crate::owner_only::write_atomic(&target, b"new").unwrap();
            drop(guard);
            assert_eq!(reader.join().unwrap(), "new");
        });
    }

    #[test]
    fn a_missing_parent_directory_is_not_found_and_stays_missing() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("absent");
        let err = read_shared(&parent.join("f.toml")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!parent.exists());
    }

    #[test]
    fn a_missing_file_in_an_existing_directory_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_shared(&dir.path().join("f.toml")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_still_reads_its_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.toml");
        std::fs::write(&target, "content").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = read_shared(&target);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result.unwrap(), "content");
    }
}
