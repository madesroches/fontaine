//! Files only the current user can read: mode 0600 on Unix, a current-user-only protected
//! DACL on Windows. Restrictions are applied when the file is created, so there is no window
//! where it is world-readable, and a pre-existing file is tightened before anything is written.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Opens `path` for writing. `append` keeps existing content; otherwise the file is truncated.
pub fn open(path: &Path, append: bool) -> io::Result<File> {
    imp::open(path, append)
}

/// Opens the append-mode lock file at `path`. Unlike `open`, other handles may hold it open for
/// writing too: on Windows a write-denying share mode would fail a second holder's open with a
/// sharing violation instead of letting `File::lock` block, so only the lock decides ownership.
pub fn open_lock(path: &Path) -> io::Result<File> {
    imp::open_lock(path)
}

/// Creates the directory `path`, whose parent must exist, readable only by the current user:
/// mode 0700 on Unix, a protected current-user-only DACL that children inherit on Windows. An
/// existing directory is an `AlreadyExists` error, so the caller learns it did not create it.
pub fn create_dir(path: &Path) -> io::Result<()> {
    imp::create_dir(path)
}

/// The current user's SID in string form; what a current-user-only DACL must name.
#[cfg(windows)]
pub use imp::current_user_sid;

/// Replaces the content of `path`.
pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = open(path, false)?;
    file.write_all(contents)?;
    file.flush()
}

/// Replaces the content of `path` all at once: a reader sees the old or the new content, and a
/// failed write leaves the old file in place. The result is owner-only.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    replace(path, Access::OwnerOnly, |file| file.write_all(contents))
}

/// [`write_atomic`] for a file that is the user's own document: an existing file keeps its
/// permissions instead of being tightened to owner-only.
pub fn write_atomic_keeping_mode(path: &Path, contents: &[u8]) -> io::Result<()> {
    replace(path, Access::KeepExisting, |file| file.write_all(contents))
}

enum Access {
    OwnerOnly,
    KeepExisting,
}

// The writer closure is the injection point for the interrupted-write test.
fn replace(
    path: &Path,
    access: Access,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    // Replace the link's target, or the link itself would become a regular file.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut prefix = std::ffi::OsString::from(".");
    prefix.push(target.file_name().unwrap_or_default());
    prefix.push(".");
    // A unique name per call lets concurrent replacers of one file coexist; the TempPath
    // deletes the file on any early return.
    let tmp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(dir)?
        .into_temp_path();

    let mut file = match access {
        Access::OwnerOnly => open(&tmp, false)?,
        Access::KeepExisting => {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            if let Ok(meta) = std::fs::metadata(&target) {
                file.set_permissions(meta.permissions())?;
            }
            file
        }
    };
    write(&mut file)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);

    let mut tmp = tmp;
    // tempfile marks the file temporary on Windows; a persisted file must not keep that mark.
    imp::clear_temporary(&tmp)?;
    // std's rename, unlike `TempPath::persist`, falls back on Windows to a POSIX-semantics rename,
    // which replaces a target other handles hold open (a reader, a racing replacer's
    // canonicalize); a plain MoveFileExW refuses that with access denied. The retry covers a
    // handle that denies delete sharing (a scanner), which no rename can replace.
    let attempts = if cfg!(windows) { 5 } else { 1 };
    for attempt in 1..=attempts {
        match std::fs::rename(&tmp, &target) {
            Ok(()) => {
                tmp.disable_cleanup(true);
                return Ok(());
            }
            Err(e) if attempt < attempts && e.kind() == io::ErrorKind::PermissionDenied => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the last attempt returns")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupted_write_keeps_the_old_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.txt");
        write_atomic(&path, b"old").unwrap();
        let result = replace(&path, Access::OwnerOnly, |file| {
            file.write_all(b"ha")?;
            Err(io::Error::other("disk full"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("data.txt")]);
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    pub fn create_dir(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }

    pub fn open_lock(path: &Path) -> io::Result<File> {
        open(path, true)
    }

    pub fn clear_temporary(_: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn open(path: &Path, append: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).mode(0o600);
        if append {
            options.append(true);
        }
        let file = options.open(path)?;
        // `mode` only applies to newly created files.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        if !append {
            file.set_len(0)?;
        }
        Ok(file)
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::{self, Seek, SeekFrom};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_ALWAYS, SetFileAttributesW,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// The current user's SID in string form, from the process token.
    pub fn current_user_sid() -> io::Result<String> {
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                let mut len = 0u32;
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
                // u64 backing gives the 8-byte alignment TOKEN_USER requires.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: the buffer is 8-byte aligned and was filled with a TOKEN_USER by the
                // successful GetTokenInformation call above; it outlives this borrow.
                let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
                let mut sid_str: *mut u16 = null_mut();
                if ConvertSidToStringSidW(user.User.Sid, &mut sid_str) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut n = 0;
                while *sid_str.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, n));
                LocalFree(sid_str.cast());
                Ok(s)
            })();
            CloseHandle(token);
            result
        }
    }

    /// A security descriptor granting full control to the current user alone, with
    /// inheritance from the parent blocked. A directory's grant is inherited by what is
    /// created in it. Free with `LocalFree`.
    fn descriptor(directory: bool) -> io::Result<*mut c_void> {
        let inherit = if directory { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{inherit};FA;;;{})", current_user_sid()?)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut sd: *mut c_void = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sd)
    }

    pub fn open(path: &Path, append: bool) -> io::Result<File> {
        open_shared(path, append, FILE_SHARE_READ)
    }

    pub fn open_lock(path: &Path) -> io::Result<File> {
        open_shared(path, true, FILE_SHARE_READ | FILE_SHARE_WRITE)
    }

    pub fn clear_temporary(path: &Path) -> io::Result<()> {
        let wide_path = wide(path);
        if unsafe { SetFileAttributesW(wide_path.as_ptr(), FILE_ATTRIBUTE_NORMAL) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn open_shared(path: &Path, append: bool, share: u32) -> io::Result<File> {
        let sd = descriptor(false)?;
        let result = unsafe { open_with(path, append, share, sd) };
        unsafe { LocalFree(sd) };
        result
    }

    pub fn create_dir(path: &Path) -> io::Result<()> {
        let sd = descriptor(true)?;
        let wide_path = wide(path);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let created = unsafe { CreateDirectoryW(wide_path.as_ptr(), &attributes) };
        let result = if created == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        };
        unsafe { LocalFree(sd) };
        result
    }

    unsafe fn open_with(
        path: &Path,
        append: bool,
        share: u32,
        sd: *mut c_void,
    ) -> io::Result<File> {
        let wide_path = wide(path);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                GENERIC_WRITE,
                share,
                &attributes,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // OPEN_ALWAYS reports ERROR_ALREADY_EXISTS on success when the file was already there;
        // its DACL was not set by the creation attributes, so replace it.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mut file = unsafe { File::from_raw_handle(handle) };
        if existed {
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = null_mut();
            let ok =
                unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
            if ok == 0 || present == 0 {
                return Err(io::Error::last_os_error());
            }
            let status = unsafe {
                SetNamedSecurityInfoW(
                    wide_path.as_ptr().cast_mut(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    dacl,
                    null(),
                )
            };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
        }
        if append {
            file.seek(SeekFrom::End(0))?;
        } else {
            file.set_len(0)?;
        }
        Ok(file)
    }
}
