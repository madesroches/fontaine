//! Git Bash support for the `shell` tool and hooks on Windows: discovery, and passing commands to
//! `sh` through the environment. Git for Windows is preferred but optional: when it is found, its
//! `usr\bin` and `bin` directories go in front of the shell launcher's PATH so `sh` resolves;
//! when it is not, PowerShell takes its place.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// Overrides discovery with the path of Git Bash's `bash.exe`.
pub const OVERRIDE_VAR: &str = "FONTAINE_GIT_BASH_PATH";

/// Carries the command to Git Bash's `sh`, which would otherwise re-parse it off the Windows
/// command line and halve every `\\`.
pub const COMMAND_VAR: &str = "FONTAINE_SH_COMMAND";

/// What `sh -c` runs in place of the command. The variable is expanded before `eval` runs, so
/// the command itself never sees it.
pub const RUN_COMMAND_SCRIPT: &str = "eval \"unset FONTAINE_SH_COMMAND\n$FONTAINE_SH_COMMAND\"";

/// Carries the file a foreground call's final directory is written to, for the same reason.
pub const CWD_FILE_VAR: &str = "FONTAINE_SH_CWD_FILE";

/// What a foreground call runs in place of `RUN_COMMAND_SCRIPT`: the same environment
/// hand-over, then the command, then the directory it ended in. `pwd -W` is a builtin that
/// prints the native `C:/...` path, which the scope check reads; `-P` would print a `/tmp/...`
/// path it cannot map. Both variables are read-only copies and gone from the environment before
/// the command runs.
pub const RUN_TRACKED_SCRIPT: &str = "readonly __fnt_f=$FONTAINE_SH_CWD_FILE __fnt_c=$FONTAINE_SH_COMMAND; \
unset FONTAINE_SH_COMMAND FONTAINE_SH_CWD_FILE; eval \"$__fnt_c\"; __fnt_rc=$?; \
command pwd -W >|\"$__fnt_f\" 2>/dev/null; exit \"$__fnt_rc\"";

/// The `sh -c` script and the variable to set so `sh` runs `command` as written.
pub struct ShCommand<'a> {
    pub script: &'a str,
    pub env: Option<(&'static str, &'a str)>,
}

/// `command` itself when it can go on the command line; through `COMMAND_VAR` for Git Bash.
pub fn sh_command(command: &str, through_env: bool) -> ShCommand<'_> {
    if through_env {
        ShCommand {
            script: RUN_COMMAND_SCRIPT,
            env: Some((COMMAND_VAR, command)),
        }
    } else {
        ShCommand {
            script: command,
            env: None,
        }
    }
}

/// Whether the POSIX `shell` tool exists: always off Windows, with Git Bash on it. The prompt
/// wording and the registered tools both ask this, so they cannot disagree.
pub fn posix_shell_available(windows: bool, git_bash: Option<&GitBash>) -> bool {
    !windows || git_bash.is_some()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBash {
    pub bin: PathBuf,
    pub usr_bin: PathBuf,
}

impl GitBash {
    /// The Git for Windows installation `exe` belongs to, for a `git.exe` or a `bash.exe` in
    /// any of the places Git for Windows puts them.
    pub fn from_executable(exe: &Path) -> Option<GitBash> {
        let name = exe.file_name()?.to_str()?.to_ascii_lowercase();
        let dir = exe.parent()?;
        let folder = |dir: &Path| {
            dir.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
        };
        let root = match (name.as_str(), folder(dir).as_deref()) {
            ("bash.exe" | "git.exe" | "sh.exe", Some("bin")) => {
                let above = dir.parent()?;
                match folder(above).as_deref() {
                    // <Git>\usr\bin or <Git>\mingw64\bin
                    Some("usr" | "mingw64" | "mingw32" | "clangarm64") => above.parent()?,
                    _ => above,
                }
            }
            ("git.exe", Some("cmd")) => dir.parent()?,
            _ => return None,
        };
        Some(GitBash {
            bin: root.join("bin"),
            usr_bin: root.join("usr").join("bin"),
        })
    }

    /// `existing` with both directories prepended, `usr\bin` first.
    pub fn path_value(&self, existing: Option<OsString>) -> OsString {
        let mut entries = vec![self.usr_bin.clone(), self.bin.clone()];
        if let Some(existing) = existing {
            entries.extend(std::env::split_paths(&existing));
        }
        std::env::join_paths(entries).unwrap_or_default()
    }
}

/// The `PATH` Git Bash's directories go in front of: the one `[env]` sets when it sets one,
/// otherwise the process's own.
pub fn base_path(set_env: &[(OsString, OsString)]) -> Option<OsString> {
    set_env
        .iter()
        .rev()
        .find(|(name, _)| {
            if cfg!(windows) {
                name.to_string_lossy().eq_ignore_ascii_case("PATH")
            } else {
                name == "PATH"
            }
        })
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("PATH"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum GitBashError {
    /// `FONTAINE_GIT_BASH_PATH` names something that is not a Git Bash installation.
    BadOverride(PathBuf),
    NotFound,
}

impl fmt::Display for GitBashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitBashError::BadOverride(path) => write!(
                f,
                "{OVERRIDE_VAR} is `{}`, which is not a bash.exe inside a Git for Windows installation",
                path.display()
            ),
            GitBashError::NotFound => f.write_str("Git for Windows was not found"),
        }
    }
}

impl std::error::Error for GitBashError {}

/// Where discovery looks, abstracted so it can be tested off Windows.
pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub is_file: &'a dyn Fn(&Path) -> bool,
    /// Full file contents, for shim following. `None` when unreadable or not a file.
    pub read_file: &'a dyn Fn(&Path) -> Option<String>,
}

/// The `path` value of a scoop `git.shim`; every other line is ignored.
fn shim_target(contents: &str) -> Option<PathBuf> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "path" {
            return None;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        (!value.is_empty()).then(|| PathBuf::from(value))
    })
}

/// A candidate `git.exe` that is a Git install, or, with `follow_shim`, whose sibling scoop
/// `git.shim` names one. The shim is followed once: a chain is not recursed into.
fn resolve_candidate(probe: &Probe<'_>, candidate: &Path, follow_shim: bool) -> Option<GitBash> {
    if (probe.is_file)(candidate)
        && let Some(found) = GitBash::from_executable(candidate)
    {
        return Some(found);
    }
    if !follow_shim {
        return None;
    }
    let contents = (probe.read_file)(&candidate.with_file_name("git.shim"))?;
    let target = shim_target(&contents)?;
    if (probe.is_file)(&target) {
        GitBash::from_executable(&target)
    } else {
        None
    }
}

/// The override when set, otherwise `git.exe` on PATH (following a scoop shim), otherwise the
/// standard install locations, otherwise scoop's `current` app junctions.
pub fn discover(probe: &Probe<'_>) -> Result<GitBash, GitBashError> {
    if let Some(value) = (probe.env)(OVERRIDE_VAR).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        return match GitBash::from_executable(&path) {
            Some(found) if (probe.is_file)(&path) => Ok(found),
            _ => Err(GitBashError::BadOverride(path)),
        };
    }
    let on_path = (probe.env)("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join("git.exe"));
    let installs = [
        ("ProgramFiles", "Git"),
        ("ProgramFiles(x86)", "Git"),
        ("LocalAppData", "Programs/Git"),
    ]
    .into_iter()
    .filter_map(|(var, git)| (probe.env)(var).map(|base| PathBuf::from(base).join(git)))
    .map(|git| git.join("cmd").join("git.exe"));
    let scoop_root = |var: &str, default: (&str, &str)| {
        (probe.env)(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| (probe.env)(default.0).map(|base| PathBuf::from(base).join(default.1)))
    };
    let scoop = [
        scoop_root("SCOOP", ("USERPROFILE", "scoop")),
        scoop_root("SCOOP_GLOBAL", ("ProgramData", "scoop")),
    ]
    .into_iter()
    .flatten()
    .map(|root| {
        root.join("apps")
            .join("git")
            .join("current")
            .join("cmd")
            .join("git.exe")
    });
    for candidate in on_path {
        if let Some(found) = resolve_candidate(probe, &candidate, true) {
            return Ok(found);
        }
    }
    for candidate in installs.chain(scoop) {
        if let Some(found) = resolve_candidate(probe, &candidate, false) {
            return Ok(found);
        }
    }
    Err(GitBashError::NotFound)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn a_configured_path_is_the_base_for_git_bashs_directories() {
        let configured = [(OsString::from("PATH"), OsString::from("/configured/bin"))];
        let base = base_path(&configured);
        assert_eq!(base, Some(OsString::from("/configured/bin")));
        let git = GitBash {
            bin: PathBuf::from("/git/bin"),
            usr_bin: PathBuf::from("/git/usr/bin"),
        };
        let joined = git.path_value(base);
        let entries: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            entries,
            [
                PathBuf::from("/git/usr/bin"),
                PathBuf::from("/git/bin"),
                PathBuf::from("/configured/bin"),
            ]
        );
    }

    #[test]
    fn without_a_configured_path_the_process_path_is_the_base() {
        let other = [(OsString::from("RUST_LOG"), OsString::from("warn"))];
        assert_eq!(base_path(&other), std::env::var_os("PATH"));
        assert_eq!(base_path(&[]), std::env::var_os("PATH"));
    }

    #[cfg(windows)]
    #[test]
    fn the_path_name_is_matched_case_insensitively_on_windows() {
        let configured = [(OsString::from("Path"), OsString::from("C:\\configured"))];
        assert_eq!(
            base_path(&configured),
            Some(OsString::from("C:\\configured"))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_path_name_is_matched_exactly_elsewhere() {
        let configured = [(OsString::from("Path"), OsString::from("/not/path"))];
        assert_eq!(base_path(&configured), std::env::var_os("PATH"));
    }

    #[test]
    fn the_command_goes_on_the_command_line_unless_it_goes_through_the_environment() {
        let plain = sh_command(r"echo '\\a'", false);
        assert_eq!(plain.script, r"echo '\\a'");
        assert_eq!(plain.env, None);
        let wrapped = sh_command(r"echo '\\a'", true);
        assert_eq!(wrapped.script, RUN_COMMAND_SCRIPT);
        assert_eq!(wrapped.env, Some((COMMAND_VAR, r"echo '\\a'")));
    }

    #[test]
    fn the_wrapper_scripts_survive_the_windows_reparse() {
        assert!(!RUN_COMMAND_SCRIPT.contains('\\'));
        assert!(!RUN_TRACKED_SCRIPT.contains('\\'));
        assert!(RUN_TRACKED_SCRIPT.contains(COMMAND_VAR));
        assert!(RUN_TRACKED_SCRIPT.contains(CWD_FILE_VAR));
        assert_eq!(
            RUN_COMMAND_SCRIPT,
            format!("eval \"unset {COMMAND_VAR}\n${COMMAND_VAR}\"")
        );
    }

    fn bash(root: &str) -> GitBash {
        GitBash {
            bin: Path::new(root).join("bin"),
            usr_bin: Path::new(root).join("usr").join("bin"),
        }
    }

    #[test]
    fn directories_derive_from_every_executable_git_for_windows_ships() {
        let root = "/Program Files/Git";
        for exe in [
            "/Program Files/Git/cmd/git.exe",
            "/Program Files/Git/bin/git.exe",
            "/Program Files/Git/bin/bash.exe",
            "/Program Files/Git/usr/bin/bash.exe",
            "/Program Files/Git/mingw64/bin/git.exe",
        ] {
            assert_eq!(
                GitBash::from_executable(Path::new(exe)),
                Some(bash(root)),
                "{exe}"
            );
        }
        assert_eq!(
            GitBash::from_executable(Path::new("/tools/python.exe")),
            None
        );
        assert_eq!(GitBash::from_executable(Path::new("/usr/bin/bash")), None);
    }

    #[test]
    fn both_directories_lead_the_path() {
        let value = bash("/g").path_value(Some(std::env::join_paths(["/a", "/b"]).unwrap()));
        let entries: Vec<PathBuf> = std::env::split_paths(&value).collect();
        assert_eq!(
            entries,
            [
                PathBuf::from("/g/usr/bin"),
                PathBuf::from("/g/bin"),
                PathBuf::from("/a"),
                PathBuf::from("/b")
            ]
        );
    }

    fn probe_with(files: &[&str], env: &[(&str, &str)]) -> Result<GitBash, GitBashError> {
        probe_with_contents(files, &[], env)
    }

    fn probe_with_contents(
        files: &[&str],
        contents: &[(&str, &str)],
        env: &[(&str, &str)],
    ) -> Result<GitBash, GitBashError> {
        let contents: Vec<(PathBuf, String)> = contents
            .iter()
            .map(|(p, c)| (PathBuf::from(p), c.to_string()))
            .collect();
        let read_file = move |path: &Path| {
            contents
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, c)| c.clone())
        };
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        let env: Vec<(String, OsString)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        let lookup = move |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let is_file = move |path: &Path| files.contains(path);
        discover(&Probe {
            env: &lookup,
            is_file: &is_file,
            read_file: &read_file,
        })
    }

    #[test]
    fn the_override_wins_and_must_be_a_git_bash() {
        let found = probe_with(
            &["/x/Git/bin/bash.exe", "/p/Git/cmd/git.exe"],
            &[
                (OVERRIDE_VAR, "/x/Git/bin/bash.exe"),
                ("PATH", "/p/Git/cmd"),
            ],
        );
        assert_eq!(found, Ok(bash("/x/Git")));
        let bad = probe_with(&[], &[(OVERRIDE_VAR, "/x/Git/bin/bash.exe")]);
        assert_eq!(
            bad,
            Err(GitBashError::BadOverride(PathBuf::from(
                "/x/Git/bin/bash.exe"
            )))
        );
    }

    #[test]
    fn git_on_the_path_is_found_before_the_install_locations() {
        let path = std::env::join_paths(["/elsewhere", "/p/Git/cmd"]).unwrap();
        let found = probe_with(
            &["/p/Git/cmd/git.exe", "/pf/Git/cmd/git.exe"],
            &[("PATH", path.to_str().unwrap()), ("ProgramFiles", "/pf")],
        );
        assert_eq!(found, Ok(bash("/p/Git")));
    }

    #[test]
    fn the_standard_install_locations_are_searched() {
        let found = probe_with(&["/pf/Git/cmd/git.exe"], &[("ProgramFiles", "/pf")]);
        assert_eq!(found, Ok(bash("/pf/Git")));
        assert_eq!(probe_with(&[], &[]), Err(GitBashError::NotFound));
    }

    #[test]
    fn the_per_user_install_location_is_searched() {
        let found = probe_with(
            &["/local/Programs/Git/cmd/git.exe"],
            &[("LocalAppData", "/local")],
        );
        assert_eq!(found, Ok(bash("/local/Programs/Git")));
    }

    const SHIM_EXE: &str = "/s/shims/git.exe";
    const SHIM_FILE: &str = "/s/shims/git.shim";
    const TARGET: &str = "/s/apps/git/current/cmd/git.exe";

    #[test]
    fn a_scoop_shim_on_the_path_is_followed_to_its_target() {
        let found = probe_with_contents(
            &[SHIM_EXE, TARGET],
            &[(SHIM_FILE, &format!("path = \"{TARGET}\"\r\nargs =\r\n"))],
            &[("PATH", "/s/shims")],
        );
        assert_eq!(found, Ok(bash("/s/apps/git/current")));
    }

    #[test]
    fn a_recognized_path_git_is_used_without_reading_its_shim() {
        let found = probe_with_contents(
            &["/p/Git/cmd/git.exe", TARGET],
            &[("/p/Git/cmd/git.shim", &format!("path = {TARGET}"))],
            &[("PATH", "/p/Git/cmd")],
        );
        assert_eq!(found, Ok(bash("/p/Git")));
    }

    #[test]
    fn a_malformed_shim_falls_through_to_the_install_locations() {
        for contents in ["garbage", "args = x", "path =", "path = \"\""] {
            let found = probe_with_contents(
                &[SHIM_EXE, TARGET, "/pf/Git/cmd/git.exe"],
                &[(SHIM_FILE, contents)],
                &[("PATH", "/s/shims"), ("ProgramFiles", "/pf")],
            );
            assert_eq!(found, Ok(bash("/pf/Git")), "{contents}");
        }
    }

    #[test]
    fn a_dangling_shim_falls_through() {
        let found = probe_with_contents(
            &[SHIM_EXE],
            &[(SHIM_FILE, &format!("path = \"{TARGET}\""))],
            &[("PATH", "/s/shims")],
        );
        assert_eq!(found, Err(GitBashError::NotFound));
    }

    #[test]
    fn a_shim_chain_is_not_followed() {
        let found = probe_with_contents(
            &[SHIM_EXE, "/t/shims/git.exe", TARGET],
            &[
                (SHIM_FILE, "path = \"/t/shims/git.exe\""),
                ("/t/shims/git.shim", &format!("path = \"{TARGET}\"")),
            ],
            &[("PATH", "/s/shims")],
        );
        assert_eq!(found, Err(GitBashError::NotFound));
    }

    #[test]
    fn the_scoop_root_is_searched() {
        let found = probe_with(&[TARGET], &[("SCOOP", "/s")]);
        assert_eq!(found, Ok(bash("/s/apps/git/current")));
    }

    #[test]
    fn the_scoop_roots_default_when_their_variables_are_unset() {
        let found = probe_with(
            &["/u/scoop/apps/git/current/cmd/git.exe"],
            &[("USERPROFILE", "/u")],
        );
        assert_eq!(found, Ok(bash("/u/scoop/apps/git/current")));
        let global = probe_with(
            &["/pd/scoop/apps/git/current/cmd/git.exe"],
            &[("ProgramData", "/pd")],
        );
        assert_eq!(global, Ok(bash("/pd/scoop/apps/git/current")));
    }

    #[test]
    fn scoop_roots_come_after_the_standard_locations() {
        let found = probe_with(
            &[TARGET, "/pf/Git/cmd/git.exe"],
            &[("SCOOP", "/s"), ("ProgramFiles", "/pf")],
        );
        assert_eq!(found, Ok(bash("/pf/Git")));
    }
}
