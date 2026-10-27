//! A private directory per session for the model's temporary and helper files, added to the
//! workspace roots so file tools work there without a prompt. It lives outside the config and
//! data directories, so the protected-path floor never sees it as Fontaine's own state.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{file_lock, owner_only};

type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// How long `create` waits out a sweeper that holds the lock only while it deletes a dead
/// session's leftovers.
const LOCK_ATTEMPTS: u32 = 10;

/// The live scratch directory. Dropping it removes the directory and its lock file.
pub struct Scratch {
    dir: PathBuf,
    lock_path: PathBuf,
    lock: Option<File>,
}

impl Scratch {
    /// Creates `<parent>/<session_id>/`, holding `<session_id>.lock` for as long as the value
    /// lives. `protected` are the directories the permission floor treats as Fontaine's own; a
    /// scratch directory under one of them is refused.
    pub fn create(env: Env<'_>, protected: &[PathBuf], session_id: &str) -> io::Result<Scratch> {
        if !is_valid_id(session_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{session_id}` is not usable as a directory name"),
            ));
        }
        refuse_protected(&parent_path(env)?.join(session_id), protected)?;
        let parent = ensure_parent(env)?;
        let dir = parent.join(session_id);
        refuse_protected(&dir, protected)?;
        let lock_path = parent.join(format!("{session_id}.lock"));

        for _ in 0..LOCK_ATTEMPTS {
            let Some(lock) = lock_with_retry(&lock_path)? else {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "the scratch directory for session {session_id} is held by another running process"
                    ),
                ));
            };
            match owner_only::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // Left by a crashed run of this session; the parent is owner-only, so
                    // only a real directory of ours can be there.
                    if !std::fs::symlink_metadata(&dir)?.file_type().is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("{} exists and is not a directory", dir.display()),
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
            // A sweep that locked the file before us may have unlinked it since.
            if lock_is_current(&lock, &lock_path)? {
                return Ok(Scratch {
                    dir,
                    lock_path,
                    lock: Some(lock),
                });
            }
        }
        Err(io::Error::other(format!(
            "the lock file {} kept disappearing",
            lock_path.display()
        )))
    }

    /// The canonical path of the directory.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A leftover directory keeps its lock file, which is what lets the next sweep find it.
        if owner_only::remove_dir_all_retrying(&self.dir).is_err() {
            return;
        }
        // Unlinking while the lock is held keeps a concurrent sweep from deleting the
        // directory of a session that starts in between; Windows cannot delete an open file.
        if cfg!(windows) {
            drop(self.lock.take());
        }
        let _ = std::fs::remove_file(&self.lock_path);
        drop(self.lock.take());
    }
}

/// An id becomes a path component and is joined to the parent for removal, so `.`, `..` and
/// separators must never get through, from a session or from a planted `*.lock` name.
fn is_valid_id(id: &str) -> bool {
    !(id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']))
}

/// Removes the scratch directories of sessions that died without cleaning up: those whose lock
/// is free. A held lock is a live session and is left alone. Returns a warning per failure.
pub fn sweep_stale(env: Env<'_>) -> Vec<String> {
    let mut warnings = Vec::new();
    let parent = match parent_path(env) {
        Ok(parent) => parent,
        Err(e) => {
            warnings.push(format!(
                "could not locate the scratch directories to clean up: {e}"
            ));
            return warnings;
        }
    };
    match std::fs::symlink_metadata(&parent) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return warnings,
        Err(e) => {
            warnings.push(format!("could not read {}: {e}", parent.display()));
            return warnings;
        }
    }
    if let Err(e) = verify_parent(&parent) {
        warnings.push(format!(
            "not cleaning up scratch directories in {}: {e}",
            parent.display()
        ));
        return warnings;
    }
    let entries = match std::fs::read_dir(&parent) {
        Ok(entries) => entries,
        Err(e) => {
            warnings.push(format!("could not read {}: {e}", parent.display()));
            return warnings;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".lock")) else {
            continue;
        };
        if !is_valid_id(id) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if let Err(e) = sweep_one(&parent, id) {
            warnings.push(format!(
                "could not remove the stale scratch directory {}: {e}",
                parent.join(id).display()
            ));
        }
    }
    warnings
}

fn sweep_one(parent: &Path, id: &str) -> io::Result<()> {
    let lock_path = parent.join(format!("{id}.lock"));
    let Some(lock) = file_lock::try_lock(&lock_path)? else {
        return Ok(());
    };
    let dir = parent.join(id);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() => owner_only::remove_dir_all_retrying(&dir)?,
        Ok(_) => std::fs::remove_file(&dir)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if cfg!(windows) {
        drop(lock);
        // The directory is already gone; another instance opening the lock in the gap
        // makes the unlink fail, and its own sweep or exit removes the file.
        return match std::fs::remove_file(&lock_path) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Ok(()),
            other => other,
        };
    }
    std::fs::remove_file(&lock_path)
}

fn lock_with_retry(path: &Path) -> io::Result<Option<File>> {
    for attempt in 0..LOCK_ATTEMPTS {
        if let Some(lock) = file_lock::try_lock(path)? {
            return Ok(Some(lock));
        }
        if attempt + 1 < LOCK_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn lock_is_current(lock: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(on_disk) => {
            let held = lock.metadata()?;
            Ok(on_disk.dev() == held.dev() && on_disk.ino() == held.ino())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
fn lock_is_current(_lock: &File, path: &Path) -> io::Result<bool> {
    // An open lock handle denies deletion on Windows, so the file cannot have been swept.
    Ok(path.exists())
}

fn refuse_protected(dir: &Path, protected: &[PathBuf]) -> io::Result<()> {
    for owned in protected {
        let canonical = std::fs::canonicalize(owned).ok();
        for form in std::iter::once(owned).chain(canonical.as_ref()) {
            if dir.starts_with(form) {
                return Err(io::Error::other(format!(
                    "{} is inside Fontaine's own directory {}",
                    dir.display(),
                    form.display()
                )));
            }
        }
    }
    Ok(())
}

/// Where scratch directories go: `$XDG_RUNTIME_DIR/fontaine` when the runtime directory is
/// usable, else a per-user directory under the temp dir.
fn parent_path(env: Env<'_>) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        if let Some(runtime) = env("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
        {
            return Ok(runtime.join("fontaine"));
        }
        let temp = env("TMPDIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        Ok(temp.join(format!("fontaine-{}", current_uid(&temp)?)))
    }
    #[cfg(windows)]
    {
        let _ = env;
        Ok(std::env::temp_dir().join(format!("fontaine-{}", owner_only::current_user_sid()?)))
    }
}

/// The uid of this process, read from the owner of a file it just created, which avoids a
/// dependency for `geteuid`.
#[cfg(unix)]
fn current_uid(temp: &Path) -> io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    Ok(tempfile::tempfile_in(temp)?.metadata()?.uid())
}

fn ensure_parent(env: Env<'_>) -> io::Result<PathBuf> {
    let parent = parent_path(env)?;
    match owner_only::create_dir(&parent) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => verify_parent(&parent)?,
        Err(e) => return Err(e),
    }
    // Resolves `/var` -> `/private/var` and a symlinked temp dir, so the root the file tools
    // compare against is the real path.
    std::fs::canonicalize(&parent)
}

/// Rejects a parent someone else could have squatted: it must be a real directory, and on Unix
/// ours alone.
fn verify_parent(parent: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(parent)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a plain directory",
            parent.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mine = current_uid(parent)?;
        if meta.uid() != mine {
            return Err(io::Error::other(format!(
                "{} is owned by another user",
                parent.display()
            )));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other(format!(
                "{} is accessible to other users",
                parent.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_for(runtime: &Path) -> impl Fn(&str) -> Option<OsString> + '_ {
        move |name| (name == "XDG_RUNTIME_DIR").then(|| runtime.as_os_str().to_owned())
    }

    #[test]
    fn create_makes_a_canonical_directory_and_drop_removes_it() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let scratch = Scratch::create(&env, &[], "abc").unwrap();
        let dir = scratch.path().to_path_buf();
        assert_eq!(dir, std::fs::canonicalize(&dir).unwrap());
        assert!(dir.is_dir());
        let lock = dir.with_file_name("abc.lock");
        assert!(lock.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        drop(scratch);
        assert!(!dir.exists());
        assert!(!lock.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_removal_keeps_the_lock_so_a_sweep_retries() {
        use std::os::unix::fs::PermissionsExt;
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let scratch = Scratch::create(&env, &[], "stuck").unwrap();
        let dir = scratch.path().to_path_buf();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), b"x").unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();
        let lock = dir.with_file_name("stuck.lock");
        drop(scratch);
        let stuck = dir.exists();
        if stuck {
            assert!(lock.exists());
        }
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).ok();
        if stuck {
            assert!(sweep_stale(&env).is_empty());
            assert!(!dir.exists());
            assert!(!lock.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_temp_dir_resolves_to_the_real_path() {
        let real = tempfile::tempdir().unwrap();
        let link_home = tempfile::tempdir().unwrap();
        let link = link_home.path().join("tmp");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let missing = real.path().join("no-runtime-dir");
        let env = |name: &str| match name {
            "XDG_RUNTIME_DIR" => Some(missing.as_os_str().to_owned()),
            "TMPDIR" => Some(link.as_os_str().to_owned()),
            _ => None,
        };
        let scratch = Scratch::create(&env, &[], "abc").unwrap();
        assert!(
            scratch
                .path()
                .starts_with(std::fs::canonicalize(real.path()).unwrap())
        );
        assert!(
            scratch
                .path()
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("fontaine-")
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_runtime_dir_is_used_when_valid_and_ignored_when_not() {
        let runtime = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let env = |name: &str| match name {
            "XDG_RUNTIME_DIR" => Some(runtime.path().as_os_str().to_owned()),
            "TMPDIR" => Some(temp.path().as_os_str().to_owned()),
            _ => None,
        };
        assert_eq!(parent_path(&env).unwrap(), runtime.path().join("fontaine"));
        for bad in ["relative/dir", "/definitely/not/a/dir"] {
            let env = |name: &str| match name {
                "XDG_RUNTIME_DIR" => Some(OsString::from(bad)),
                "TMPDIR" => Some(temp.path().as_os_str().to_owned()),
                _ => None,
            };
            assert!(parent_path(&env).unwrap().starts_with(temp.path()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_squatted_parent_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let parent = runtime.path().join("fontaine");

        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Scratch::create(&env, &[], "abc").is_err());
        std::fs::remove_dir(&parent).unwrap();

        let elsewhere = runtime.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &parent).unwrap();
        assert!(Scratch::create(&env, &[], "abc").is_err());
    }

    #[test]
    fn a_scratch_path_under_a_protected_dir_is_refused() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        // Windows ignores XDG_RUNTIME_DIR, so the real ancestor of the scratch parent is the temp dir.
        #[cfg(windows)]
        let protected = [std::env::temp_dir()];
        #[cfg(not(windows))]
        let protected = [runtime.path().to_path_buf()];
        let err = Scratch::create(&env, &protected, "abc").err().unwrap();
        assert!(err.to_string().contains("Fontaine's own directory"));
        let sibling = tempfile::tempdir().unwrap();
        assert!(Scratch::create(&env, &[sibling.path().to_path_buf()], "abc").is_ok());
    }

    #[test]
    fn an_id_that_is_a_path_is_refused() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        for id in ["", "..", "a/b", "a\\b"] {
            assert!(Scratch::create(&env, &[], id).is_err(), "{id:?}");
        }
    }

    #[test]
    fn an_existing_directory_for_the_id_is_reused() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let dir = Scratch::create(&env, &[], "abc")
            .unwrap()
            .path()
            .to_path_buf();
        // A crash leaves the directory behind with its lock free.
        owner_only::create_dir(&dir).unwrap();
        std::fs::write(dir.join("old.txt"), "x").unwrap();
        let second = Scratch::create(&env, &[], "abc").unwrap();
        assert_eq!(second.path(), dir);
        assert!(dir.join("old.txt").exists());
    }

    #[test]
    fn a_second_holder_of_the_same_session_is_refused() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let first = Scratch::create(&env, &[], "abc").unwrap();
        let err = Scratch::create(&env, &[], "abc").err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(first.path().is_dir());
    }

    #[test]
    fn sweep_removes_dead_sessions_and_spares_live_ones() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let live = Scratch::create(&env, &[], "live").unwrap();
        let parent = live.path().parent().unwrap().to_path_buf();
        std::fs::create_dir(parent.join("dead")).unwrap();
        std::fs::write(parent.join("dead").join("f"), "x").unwrap();
        std::fs::write(parent.join("dead.lock"), "").unwrap();
        std::fs::write(parent.join("orphan.lock"), "").unwrap();

        let warnings = sweep_stale(&env);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!parent.join("dead").exists());
        assert!(!parent.join("dead.lock").exists());
        assert!(!parent.join("orphan.lock").exists());
        assert!(live.path().is_dir());
        assert!(parent.join("live.lock").exists());
    }

    #[test]
    fn sweep_ignores_lock_files_whose_id_is_a_dot_path() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        let live = Scratch::create(&env, &[], "live").unwrap();
        let parent = live.path().parent().unwrap().to_path_buf();
        std::fs::write(parent.join("..lock"), b"").unwrap();
        std::fs::write(parent.join("...lock"), b"").unwrap();
        let bystander = runtime.path().join("bystander");
        std::fs::write(&bystander, b"x").unwrap();

        sweep_stale(&env);

        assert!(live.path().is_dir());
        assert!(bystander.is_file());
        assert!(parent.join("..lock").is_file());
    }

    #[test]
    fn sweep_without_a_parent_does_nothing() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        assert!(sweep_stale(&env).is_empty());
        assert!(!runtime.path().join("fontaine").exists());
    }

    #[test]
    fn create_racing_a_sweep_never_loses_its_directory() {
        let runtime = tempfile::tempdir().unwrap();
        let env = env_for(runtime.path());
        for round in 0..20 {
            let id = format!("race{round}");
            std::thread::scope(|scope| {
                let sweeper = scope.spawn(|| {
                    for _ in 0..20 {
                        sweep_stale(&env);
                    }
                });
                let scratch = Scratch::create(&env, &[], &id).unwrap();
                sweeper.join().unwrap();
                assert!(scratch.path().is_dir(), "round {round}");
            });
        }
    }
}
