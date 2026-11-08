//! The shell a configured command string (a hook, the status line) runs in: `sh` where there is
//! one, Git Bash's on Windows, and PowerShell on Windows when Git for Windows is missing.

use std::ffi::OsString;

use crate::git_bash::{GitBash, base_path, sh_command};
use crate::powershell::{self, PowerShellSettings};

/// How a configured command string is run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandShell {
    /// `sh -c` with the command on the command line (Linux, macOS).
    Sh,
    /// Git Bash's `sh -c`, the command carried in `FONTAINE_SH_COMMAND`, its directories on PATH.
    GitBash(GitBash),
    /// PowerShell with the command `-EncodedCommand`ed (Windows without Git Bash).
    PowerShell(PowerShellSettings),
}

/// What to spawn: the program, its arguments and the environment pairs to set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl CommandShell {
    /// `Sh` off Windows, Git Bash when found, else PowerShell when found, else `None`.
    pub fn select(
        windows: bool,
        git_bash: Option<GitBash>,
        powershell: Option<PowerShellSettings>,
    ) -> Option<CommandShell> {
        if !windows {
            return Some(CommandShell::Sh);
        }
        git_bash
            .map(CommandShell::GitBash)
            .or_else(|| powershell.map(CommandShell::PowerShell))
    }

    /// Whether this is the PowerShell fallback.
    pub fn is_powershell(&self) -> bool {
        matches!(self, CommandShell::PowerShell(_))
    }

    /// The program, arguments and extra environment for `command`. `set_env` is the user's
    /// `[env]`, which decides the `PATH` Git Bash's directories go in front of.
    pub fn invocation(&self, command: &str, set_env: &[(OsString, OsString)]) -> Invocation {
        match self {
            CommandShell::Sh | CommandShell::GitBash(_) => {
                let git = match self {
                    CommandShell::GitBash(git) => Some(git),
                    _ => None,
                };
                let sh = sh_command(command, git.is_some());
                let mut env: Vec<(OsString, OsString)> = sh
                    .env
                    .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                    .into_iter()
                    .collect();
                if let Some(git) = git {
                    env.push((OsString::from("PATH"), git.path_value(base_path(set_env))));
                }
                Invocation {
                    program: OsString::from("sh"),
                    args: vec![OsString::from("-c"), OsString::from(sh.script)],
                    env,
                }
            }
            CommandShell::PowerShell(settings) => Invocation {
                program: settings.path.clone().into_os_string(),
                args: powershell::launch_args(command),
                env: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::git_bash;

    use super::*;

    fn git() -> GitBash {
        GitBash {
            bin: PathBuf::from("/g/bin"),
            usr_bin: PathBuf::from("/g/usr/bin"),
        }
    }

    fn ps() -> PowerShellSettings {
        PowerShellSettings {
            path: PathBuf::from("pwsh.exe"),
        }
    }

    #[test]
    fn select_prefers_git_bash_then_powershell_on_windows() {
        assert_eq!(
            CommandShell::select(true, Some(git()), Some(ps())),
            Some(CommandShell::GitBash(git()))
        );
        assert_eq!(
            CommandShell::select(true, Some(git()), None),
            Some(CommandShell::GitBash(git()))
        );
        assert_eq!(
            CommandShell::select(true, None, Some(ps())),
            Some(CommandShell::PowerShell(ps()))
        );
        assert_eq!(CommandShell::select(true, None, None), None);
    }

    #[test]
    fn select_is_plain_sh_off_windows() {
        for (git, powershell) in [(None, None), (Some(git()), Some(ps()))] {
            assert_eq!(
                CommandShell::select(false, git, powershell),
                Some(CommandShell::Sh)
            );
        }
    }

    #[test]
    fn sh_puts_the_command_on_the_command_line() {
        let run = CommandShell::Sh.invocation(r"echo '\a'", &[]);
        assert_eq!(run.program, "sh");
        assert_eq!(run.args, ["-c", r"echo '\a'"]);
        assert!(run.env.is_empty());
    }

    #[test]
    fn git_bash_carries_the_command_in_the_environment_with_its_directories_first() {
        let configured = [(OsString::from("PATH"), OsString::from("/configured"))];
        let run = CommandShell::GitBash(git()).invocation("echo hi", &configured);
        assert_eq!(run.program, "sh");
        assert_eq!(run.args, ["-c", git_bash::RUN_COMMAND_SCRIPT]);
        assert_eq!(
            run.env[0],
            (
                OsString::from(git_bash::COMMAND_VAR),
                OsString::from("echo hi")
            )
        );
        let (name, value) = &run.env[1];
        assert_eq!(name, "PATH");
        let entries: Vec<PathBuf> = std::env::split_paths(value).collect();
        assert_eq!(
            entries,
            [
                PathBuf::from("/g/usr/bin"),
                PathBuf::from("/g/bin"),
                PathBuf::from("/configured")
            ]
        );
    }

    #[test]
    fn powershell_gets_the_interpreter_and_the_encoded_command() {
        let run = CommandShell::PowerShell(ps()).invocation("Get-Date", &[]);
        assert_eq!(run.program, "pwsh.exe");
        assert_eq!(run.args, powershell::launch_args("Get-Date"));
        assert_eq!(run.args[run.args.len() - 2], "-EncodedCommand");
        assert!(run.env.is_empty());
        assert!(CommandShell::PowerShell(ps()).is_powershell());
        assert!(!CommandShell::Sh.is_powershell());
    }
}
