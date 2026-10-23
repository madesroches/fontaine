//! Git Bash support for the `shell` tool and hooks on Windows: discovery, and passing commands to
//! `sh` through the environment. Git for Windows is a prerequisite: its `usr\bin` and `bin`
//! directories go in front of the shell launcher's PATH so `sh` resolves.

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
            GitBashError::NotFound => write!(
                f,
                "Git for Windows was not found: the shell tool runs through Git Bash. Install Git for Windows, or set {OVERRIDE_VAR} to the path of its bash.exe"
            ),
        }
    }
}

impl std::error::Error for GitBashError {}

/// Where discovery looks, abstracted so it can be tested off Windows.
pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub is_file: &'a dyn Fn(&Path) -> bool,
}

/// The override when set, otherwise `git.exe` on PATH, otherwise the standard install
/// locations.
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
    for candidate in on_path.chain(installs) {
        if (probe.is_file)(&candidate)
            && let Some(found) = GitBash::from_executable(&candidate)
        {
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
    fn the_command_goes_on_the_command_line_unless_it_goes_through_the_environment() {
        let plain = sh_command(r"echo '\\a'", false);
        assert_eq!(plain.script, r"echo '\\a'");
        assert_eq!(plain.env, None);
        let wrapped = sh_command(r"echo '\\a'", true);
        assert_eq!(wrapped.script, RUN_COMMAND_SCRIPT);
        assert_eq!(wrapped.env, Some((COMMAND_VAR, r"echo '\\a'")));
    }

    #[test]
    fn the_wrapper_script_survives_the_windows_reparse() {
        assert!(!RUN_COMMAND_SCRIPT.contains('\\'));
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
}
