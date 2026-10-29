//! `/memory` and `/init`.

use std::ffi::OsString;
use std::path::Path;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::instructions::MemoryKind;
use fontaine_tools::error_chain::chain;
use fontaine_tools::git_bash::{GitBash, base_path, sh_command};

use crate::commands::memory_number;
use crate::{App, Ui, reports};

/// How the editor string is turned into a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Launcher {
    /// `sh -c '<editor> "$1"'`, as git does, so `code --wait` works.
    Sh,
    /// The same through Git Bash, which keeps `C:\\Program Files\\...` paths intact.
    GitBash(GitBash),
    /// No shell to hand the string to: its words are the program and its arguments.
    Words,
}

impl Launcher {
    pub(crate) fn for_platform(git_bash: Option<GitBash>) -> Launcher {
        match (cfg!(windows), git_bash) {
            (false, _) => Launcher::Sh,
            (true, Some(bash)) => Launcher::GitBash(bash),
            (true, None) => Launcher::Words,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
}

/// `$VISUAL`, then `$EDITOR`, else `vi` (`notepad` on Windows). An empty variable is unset.
pub(crate) fn resolve_editor(env: &dyn Fn(&str) -> Option<OsString>, windows: bool) -> String {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(env)
        .filter_map(|value| value.into_string().ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| if windows { "notepad" } else { "vi" }.to_string())
}

pub(crate) fn editor_command(editor: &str, path: &Path, launcher: &Launcher) -> EditorCommand {
    match launcher {
        Launcher::Sh => EditorCommand {
            program: "sh".to_string(),
            args: vec![
                "-c".into(),
                format!("{editor} \"$1\"").into(),
                "editor".into(),
                path.as_os_str().to_owned(),
            ],
            env: Vec::new(),
        },
        Launcher::GitBash(bash) => {
            let command = format!("{editor} \"$1\"");
            let sh = sh_command(&command, true);
            let mut env: Vec<(OsString, OsString)> = sh
                .env
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect();
            env.push(("PATH".into(), bash.path_value(base_path(&[]))));
            EditorCommand {
                program: "sh".to_string(),
                args: vec![
                    "-c".into(),
                    sh.script.into(),
                    "editor".into(),
                    path.as_os_str().to_owned(),
                ],
                env,
            }
        }
        Launcher::Words => {
            let mut words = editor.split_whitespace();
            let program = words.next().unwrap_or("notepad").to_string();
            let mut args: Vec<OsString> = words.map(OsString::from).collect();
            args.push(path.as_os_str().to_owned());
            EditorCommand {
                program,
                args,
                env: Vec::new(),
            }
        }
    }
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/memory`: the list, or with a number the file opened in the user's editor, created
    /// first if it does not exist.
    pub(crate) fn memory(&mut self, runtime: &AgentRuntime, argument: &str) {
        let number = match memory_number(argument) {
            Ok(number) => number,
            Err(e) => {
                self.app.transcript.error(e);
                return;
            }
        };
        let entries = runtime.memory_entries();
        let Some(number) = number else {
            let dir = runtime.memory_dir();
            self.app.transcript.notice(reports::memory(
                &entries,
                &reports::AutoMemory {
                    on: runtime.auto_memory_active(),
                    dir: &dir,
                },
            ));
            return;
        };
        let Some(entry) = number.checked_sub(1).and_then(|at| entries.get(at)) else {
            self.app.transcript.error(format!(
                "there is no instruction file number {number}; /memory lists {}",
                entries.len()
            ));
            return;
        };
        if entry.kind == MemoryKind::NotCreated
            && let Err(e) = create_empty(&entry.path)
        {
            self.app.transcript.error(format!(
                "could not create {}: {}",
                entry.path.display(),
                chain(&e)
            ));
            return;
        }
        let editor = resolve_editor(&*self.env, cfg!(windows));
        let command = editor_command(&editor, &entry.path, &self.launcher);
        let mut process = std::process::Command::new(&command.program);
        process
            .args(&command.args)
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .current_dir(runtime.primary_root());
        let mut outcome = None;
        (self.suspend)(&mut || outcome = Some(process.status()));
        match outcome {
            Some(Ok(status)) if status.success() => {
                self.app.transcript.notice(format!(
                    "{} edits apply at the next model switch, output-style change, /add-dir or new session; the guardian keeps the files it read when the session started",
                    entry.path.display()
                ));
            }
            Some(Ok(status)) => self
                .app
                .transcript
                .error(format!("the editor `{editor}` ended with {status}")),
            Some(Err(e)) => self.app.transcript.error(format!(
                "could not start the editor `{editor}`: {}",
                chain(&e)
            )),
            None => {}
        }
    }
}

fn create_empty(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn visual_wins_over_editor_and_the_fallback_is_vi_or_notepad() {
        assert_eq!(
            resolve_editor(
                &lookup(&[("VISUAL", "code --wait"), ("EDITOR", "nano")]),
                false
            ),
            "code --wait"
        );
        assert_eq!(
            resolve_editor(&lookup(&[("EDITOR", "nano")]), false),
            "nano"
        );
        assert_eq!(
            resolve_editor(&lookup(&[("VISUAL", "  "), ("EDITOR", "nano")]), false),
            "nano"
        );
        assert_eq!(resolve_editor(&lookup(&[]), false), "vi");
        assert_eq!(resolve_editor(&lookup(&[]), true), "notepad");
    }

    #[test]
    fn sh_gets_the_editor_string_and_the_path_as_a_positional_argument() {
        let command = editor_command("code --wait", Path::new("/a b/FONTAINE.md"), &Launcher::Sh);
        assert_eq!(command.program, "sh");
        assert_eq!(
            command.args,
            ["-c", "code --wait \"$1\"", "editor", "/a b/FONTAINE.md"].map(OsString::from)
        );
        assert!(command.env.is_empty());
    }

    #[test]
    fn git_bash_carries_the_command_in_the_environment_and_prepends_its_directories() {
        let bash = GitBash {
            bin: "/git/bin".into(),
            usr_bin: "/git/usr/bin".into(),
        };
        let command = editor_command("vim", Path::new("C:\\x\\F.md"), &Launcher::GitBash(bash));
        assert_eq!(command.program, "sh");
        assert_eq!(command.args[2], "editor");
        assert_eq!(command.args[3], "C:\\x\\F.md");
        assert!(
            command
                .env
                .iter()
                .any(|(k, v)| k == "FONTAINE_SH_COMMAND" && v == "vim \"$1\"")
        );
        let path = command.env.iter().find(|(k, _)| k == "PATH").unwrap();
        let entries: Vec<PathBuf> = std::env::split_paths(&path.1).take(2).collect();
        assert_eq!(
            entries,
            [PathBuf::from("/git/usr/bin"), PathBuf::from("/git/bin")]
        );
    }

    #[test]
    fn without_a_shell_the_editor_splits_on_whitespace() {
        let command = editor_command(
            "notepad++ -multiInst",
            Path::new("C:\\F.md"),
            &Launcher::Words,
        );
        assert_eq!(command.program, "notepad++");
        assert_eq!(command.args, ["-multiInst", "C:\\F.md"].map(OsString::from));
    }

    #[test]
    fn a_missing_file_is_created_with_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/FONTAINE.md");
        create_empty(&path).unwrap();
        assert!(path.is_file());
        std::fs::write(&path, "kept").unwrap();
        create_empty(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept");
    }
}
