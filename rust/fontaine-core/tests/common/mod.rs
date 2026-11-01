#![allow(dead_code)]

pub mod pipeline;
pub mod runtime;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use fontaine_core::config::{self, Config, ConfigError, LoadParams};
use tempfile::TempDir;

/// A scratch world: a launch dir, a config dir and a data dir, all canonical and separate.
pub struct World {
    _tmp: TempDir,
    pub root: PathBuf,
    pub work: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
}

impl World {
    pub fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let work = root.join("work");
        let config = root.join("config");
        let data = root.join("data");
        for dir in [&work, &config, &data] {
            std::fs::create_dir_all(dir).unwrap();
        }
        World {
            _tmp: tmp,
            root,
            work,
            config,
            data,
        }
    }

    /// Where a runtime built in this world keeps auto memory, once `enable_memory` made it.
    pub fn memory_dir(&self) -> PathBuf {
        self.data.join("memory").join("test-project")
    }

    /// Makes runtimes built in this world run with auto memory on, as a session whose
    /// `[memory] auto` is on and whose store could be created.
    pub fn enable_memory(&self) -> PathBuf {
        let dir = self.memory_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    pub fn load(&self) -> Result<Config, ConfigError> {
        self.load_from(&self.work, None, &no_env)
    }

    pub fn load_with_settings(&self, settings: &Path) -> Result<Config, ConfigError> {
        self.load_from(&self.work, Some(settings), &no_env)
    }

    pub fn load_from(
        &self,
        launch_dir: &Path,
        settings: Option<&Path>,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Config, ConfigError> {
        config::load(&LoadParams {
            launch_dir,
            config_dir: &self.config,
            data_dir: &self.data,
            settings_file: settings,
            cli_rules: Default::default(),
            env,
        })
    }

    pub fn user_file(&self, text: &str) {
        write(&self.config.join("config.toml"), text);
    }

    /// A file next to the user's settings, such as a composition script.
    pub fn config_file(&self, name: &str, text: &str) {
        write(&self.config.join(name), text);
    }

    pub fn project_file(&self, text: &str) {
        write(&self.work.join(".fontaine/settings.toml"), text);
    }

    pub fn local_file(&self, text: &str) {
        write(&self.work.join(".fontaine/settings.local.toml"), text);
    }

    pub fn cli_file(&self, text: &str) -> PathBuf {
        let path = self.root.join("cli-settings.toml");
        write(&path, text);
        path
    }

    /// Makes `work` a git work tree whose root is recorded as trusted.
    pub fn trusted_repo(&self) {
        git_init(&self.work);
        fontaine_core::trust::record(&self.data, &self.work).unwrap();
    }
}

/// A decision record for audit-log tests that need one.
pub fn audit_record(session_id: &str) -> fontaine_core::audit::DecisionRecord {
    use fontaine_core::audit::{Decision, DecisionRecord};
    DecisionRecord {
        ts: fontaine_core::audit::now_rfc3339(),
        session_id: session_id.to_string(),
        agent_id: "root".to_string(),
        tool_use_id: None,
        tool: "read".to_string(),
        args_digest: String::new(),
        original_args_digest: None,
        tier: fontaine_core::tier::Tier::ReadOnlyFastPath,
        decision: Decision::Allow,
        rule: None,
        confidence: None,
        guardian_model: None,
        latency_ms: 0,
        guardian_profile: None,
        guardian_verdict: None,
        guardian_cache_hit: None,
        verdict_transport: None,
        guardian_latency_ms: None,
        fail_closed_reason: None,
        screen_risk: None,
        screen_allow_risk: None,
        screen_steering: None,
        screen_scores: None,
        screen_model: None,
        screen_latency_ms: None,
        screen_outcome: None,
        guardian_dropped_input_tokens: None,
        guardian_dropped_provider: None,
        guardian_dropped_model: None,
        guardian_dropped_priced: None,
        permission_mode: "manual".to_string(),
        headless: false,
        explanation: None,
    }
}

/// Git Bash, which the shell and the hooks run through on Windows; nothing elsewhere.
#[cfg(windows)]
pub fn git_bash() -> Option<fontaine_tools::git_bash::GitBash> {
    let probe = fontaine_tools::git_bash::Probe {
        env: &|name| std::env::var_os(name),
        is_file: &|path| path.is_file(),
    };
    fontaine_tools::git_bash::discover(&probe).ok()
}

#[cfg(not(windows))]
pub fn git_bash() -> Option<fontaine_tools::git_bash::GitBash> {
    None
}

pub fn no_env(_: &str) -> Option<OsString> {
    None
}

pub fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// Test fixtures use the git CLI; the code under test never does.
pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(dir)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env(
            "GIT_CONFIG_SYSTEM",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .expect("git is installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
}

/// Asserts the file carries a protected DACL with a single ACE naming the current user.
#[cfg(windows)]
pub fn assert_current_user_only(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        SE_DACL_PROTECTED,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut dacl: *mut ACL = null_mut();
    let mut sd = null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    assert_eq!(status, 0, "GetNamedSecurityInfoW failed");

    unsafe {
        let mut control = 0u16;
        let mut revision = 0u32;
        assert_ne!(
            GetSecurityDescriptorControl(sd, &mut control, &mut revision),
            0
        );
        assert!(
            control & SE_DACL_PROTECTED != 0,
            "DACL inherits from the parent"
        );

        let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
        let ok = GetAclInformation(
            dacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        );
        assert_ne!(ok, 0);
        assert_eq!(info.AceCount, 1, "exactly one ACE");

        let mut ace = null_mut();
        assert_ne!(GetAce(dacl, 0, &mut ace), 0);
        let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
        let sid = (&ace.SidStart as *const u32 as *mut core::ffi::c_void).cast();
        let mut sid_str: *mut u16 = null_mut();
        assert_ne!(ConvertSidToStringSidW(sid, &mut sid_str), 0);
        let mut n = 0;
        while *sid_str.add(n) != 0 {
            n += 1;
        }
        let ace_sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, n));
        LocalFree(sid_str.cast());
        LocalFree(sd);

        assert_eq!(
            ace_sid,
            fontaine_core::owner_only::current_user_sid().unwrap()
        );
    }
}
