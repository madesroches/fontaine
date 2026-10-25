//! Cross-process exclusion for read-modify-write of a persisted file: an exclusive lock on a
//! sibling `.lock` file, held until the guard drops.

use std::fs::File;
use std::io;
use std::path::Path;

/// Holds the exclusive lock until dropped.
pub struct FileLock(File);

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Blocks until the lock for `target` is held: `target` with its extension swapped for `lock`
/// (`credentials.toml` -> `credentials.lock`), created beside it along with the parent
/// directory. The path is used as given, never symlink-resolved, so a linked settings file
/// keeps its lock beside the link.
pub fn lock(target: &Path) -> io::Result<FileLock> {
    let path = target.with_extension("lock");
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = crate::owner_only::open_lock(&path)?;
    file.lock()?;
    Ok(FileLock(file))
}
