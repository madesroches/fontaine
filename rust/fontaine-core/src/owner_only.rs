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

/// The current user's SID in string form; what a current-user-only DACL must name.
#[cfg(windows)]
pub use imp::current_user_sid;

/// Replaces the content of `path`.
pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = open(path, false)?;
    file.write_all(contents)?;
    file.flush()
}

#[cfg(unix)]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;

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
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_ALWAYS,
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
                let mut buf = vec![0u8; len as usize];
                if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
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
    /// inheritance from the parent blocked. Free with `LocalFree`.
    fn descriptor() -> io::Result<*mut c_void> {
        let sddl: Vec<u16> = format!("D:P(A;;FA;;;{})", current_user_sid()?)
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
        let sd = descriptor()?;
        let result = unsafe { open_with(path, append, sd) };
        unsafe { LocalFree(sd) };
        result
    }

    unsafe fn open_with(path: &Path, append: bool, sd: *mut c_void) -> io::Result<File> {
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
                FILE_SHARE_READ,
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
