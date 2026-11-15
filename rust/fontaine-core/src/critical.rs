//! The deletions of critical places a call carries, as evidence for the guardian: the file system
//! root, a top-level directory, the home directory, a workspace root or a parent of one. Only a
//! deleter operand the walk can read as a literal path counts; whatever it cannot resolve (an
//! expanded command name or target, targets a launcher supplies at run time, a glob) adds nothing and is judged from the raw command.
//! The walk runs on parsed commands and resolved paths, never on a pattern over raw arguments.

use std::path::{Path, PathBuf};

use fontaine_tools::descriptor::is_shell_class;
use fontaine_tools::preclassify::{ShellWord, analyze, program_name};
use fontaine_tools::scope::{Scope, fold, translate};
use serde_json::Value;

mod powershell;

use crate::floor::under;

const DELETERS: [&str; 2] = ["rm", "rmdir"];

/// What the walk needs to know about the machine.
pub struct CriticalEnv<'a> {
    pub scope: &'a Scope,
    pub home: Option<&'a Path>,
    /// The session scratch directory: private to the session and removed when it ends, so
    /// deleting it loses nothing. Its parents still hold other sessions' scratch directories.
    pub scratch: Option<&'a Path>,
}

/// The critical places the call's deleters name, each as the resolved path followed by its kind
/// (`/home/me (home directory)`). Empty for any tool that is not a shell or PowerShell.
pub fn critical_deletions(tool: &str, args: &Value, env: &CriticalEnv<'_>) -> Vec<String> {
    let powershell = tool == "powershell";
    if !(powershell || is_shell_class(tool)) {
        return Vec::new();
    }
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return Vec::new();
    };
    let base = match args.get("cwd").and_then(Value::as_str) {
        Some(cwd) => env.scope.resolved(cwd),
        None => env.scope.primary().to_path_buf(),
    };
    if powershell {
        powershell::critical_powershell(command, &base, env)
    } else {
        critical_shell(command, &base, env)
    }
}

fn critical_shell(command: &str, base: &Path, env: &CriticalEnv<'_>) -> Vec<String> {
    let analysis = analyze(command);
    let mut found = Vec::new();
    for simple in &analysis.commands {
        let Some(at) = deleter_position(&simple.words) else {
            continue;
        };
        let mut options_ended = false;
        for operand in &simple.words[at + 1..] {
            let text = operand.literal.as_deref();
            if !options_ended && text == Some("--") {
                options_ended = true;
                continue;
            }
            if !options_ended && text.is_some_and(|t| t.starts_with('-')) {
                continue;
            }
            let target = match text {
                Some(text) => Some(text.to_string()),
                None => home_target(operand, analysis.assignments, env),
            };
            if let Some(place) = target.and_then(|t| critical_target(&t, base, env))
                && !found.contains(&place)
            {
                found.push(place);
            }
        }
    }
    found
}

/// Argv wrappers that run the command after their own options and arguments: the options that
/// take a value, and how many positional arguments precede the command.
const WRAPPERS: [(&str, &[&str], usize); 12] = [
    (
        "sudo",
        &["-u", "-g", "-h", "-p", "-C", "-T", "-r", "-t", "-U"],
        0,
    ),
    ("doas", &["-u", "-C"], 0),
    ("env", &["-u", "-C", "-S"], 0),
    ("timeout", &["-s", "-k"], 1),
    ("nice", &["-n"], 0),
    ("nohup", &[], 0),
    ("command", &[], 0),
    ("exec", &["-a"], 0),
    ("time", &["-f", "-o"], 0),
    ("stdbuf", &["-i", "-o", "-e"], 0),
    ("ionice", &["-c", "-n", "-p", "-t"], 0),
    ("chrt", &[], 1),
];

/// Value options of `xargs`, which runs its command with literal arguments of its own.
const XARGS_VALUE_OPTIONS: [&str; 8] = ["-a", "-d", "-E", "-I", "-L", "-n", "-P", "-s"];

/// The index of the word that is a deleter in command position: the first word, or the first one
/// after a chain of wrappers. A deleter's name anywhere else is an argument (`grep -w rm /etc`,
/// `git rm`).
fn deleter_position(words: &[ShellWord]) -> Option<usize> {
    let mut at = 0;
    loop {
        let name = program_name(words.get(at)?.literal.as_deref()?);
        if DELETERS.contains(&&*name) {
            return Some(at);
        }
        let (value_options, positionals): (&[&str], usize) = if name == "xargs" {
            (&XARGS_VALUE_OPTIONS, 0)
        } else {
            let (_, value_options, positionals) =
                WRAPPERS.iter().find(|(wrapper, ..)| *wrapper == &*name)?;
            (value_options, *positionals)
        };
        at += 1;
        let mut left = positionals;
        loop {
            let text = words.get(at)?.literal.as_deref()?;
            if text == "--" {
                at += 1;
                break;
            }
            if text.starts_with('-') && text.len() > 1 {
                at += if value_options.contains(&text) { 2 } else { 1 };
            } else if name == "env" && text.contains('=') {
                at += 1;
            } else if left > 0 {
                left -= 1;
                at += 1;
            } else {
                break;
            }
        }
    }
}

/// A word that starts with the home directory, as the path it names. `$HOME` can be reassigned
/// by the command itself, so it counts only when nothing in the command assigns anything.
fn home_target(word: &ShellWord, assignments: bool, env: &CriticalEnv<'_>) -> Option<String> {
    let rest = word.home_relative.as_deref()?;
    if assignments {
        return None;
    }
    let home = env.home?.to_string_lossy();
    Some(format!("{}{rest}", home.trim_end_matches(['/', '\\'])))
}

/// An rm operand as the Windows file system sees it: `/c/...` names a drive under MSYS.
fn native_operand(target: &str, windows: bool) -> String {
    translate(target, windows).unwrap_or_else(|| target.to_string())
}

fn critical_target(target: &str, base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    critical_place(&native_operand(target, cfg!(windows)), base, env)
}

/// The resolved path and kind of `target`, a path as the file system reads it, when it is a
/// critical place.
fn critical_place(target: &str, base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    let absolute = Path::new(target).is_absolute() || target.starts_with(['/', '\\']);
    let joined = if absolute {
        PathBuf::from(target)
    } else {
        base.join(target)
    };
    let lexical = fold(&joined);
    // A symlink to a critical place is as critical as the place.
    let resolved = std::fs::canonicalize(&lexical).ok();
    [Some(lexical), resolved]
        .into_iter()
        .flatten()
        .find_map(|path| kind(&path, env).map(|kind| format!("{} ({kind})", path.display())))
}

fn kind(path: &Path, env: &CriticalEnv<'_>) -> Option<&'static str> {
    let is_root = |p: &Path| p.parent().is_none();
    if is_root(path) {
        return Some("file system root");
    }
    if path.parent().is_some_and(is_root) {
        return Some("top-level directory");
    }
    if env.home.is_some_and(|home| same_path(path, home)) {
        return Some("home directory");
    }
    if env.scratch.is_some_and(|scratch| {
        same_path(path, scratch)
            || std::fs::canonicalize(scratch).is_ok_and(|real| same_path(path, &real))
    }) {
        return None;
    }
    let roots = env.scope.roots();
    if roots.iter().any(|root| same_path(path, root)) {
        return Some("workspace root");
    }
    roots
        .iter()
        .any(|root| starts_with(root, path))
        .then_some("parent of a workspace root")
}

fn same_path(a: &Path, b: &Path) -> bool {
    starts_with(a, b) && starts_with(b, a)
}

/// Whether `path` is `prefix` or lies below it.
fn starts_with(path: &Path, prefix: &Path) -> bool {
    under(path, prefix).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(scope: &Scope) -> CriticalEnv<'_> {
        CriticalEnv {
            scope,
            home: Some(Path::new(if cfg!(windows) {
                r"C:\Users\me"
            } else {
                "/home/me"
            })),
            scratch: None,
        }
    }

    fn deletions(command: &str, cwd: Option<&str>, env: &CriticalEnv<'_>) -> Vec<String> {
        let mut args = serde_json::json!({ "command": command });
        if let Some(cwd) = cwd {
            args["cwd"] = cwd.into();
        }
        critical_deletions("shell", &args, env)
    }

    #[cfg(windows)]
    #[test]
    fn critical_targets_on_windows() {
        let scope = Scope::new(PathBuf::from(r"C:\Users\me\proj"), []);
        let env = env(&scope);
        for cmd in [
            r"rm -rf 'C:\'",
            r"rm -rf 'C:\Windows'",
            r"rm -rf 'C:\Users\me'",
            r"rm -rf 'C:\Users\me\proj'",
            r"rm -rf 'C:\Users'",
            "rm -rf /c/Users/me",
            "rm -rf /c/Users/me/proj",
            "rm -rf /c/Users",
        ] {
            assert!(!deletions(cmd, None, &env).is_empty(), "{cmd}");
        }
        assert!(deletions(r"rm -rf 'C:\Users\me\proj\target'", None, &env).is_empty());
    }

    #[test]
    fn msys_operands_translate_only_when_asked() {
        assert_eq!(native_operand("/c/Users/me", true), r"C:\Users\me");
        assert_eq!(native_operand("/c/Users/me", false), "/c/Users/me");
        assert_eq!(native_operand("/etc", true), "/etc");
        assert_eq!(native_operand("rel/x", true), "rel/x");
    }

    #[cfg(unix)]
    #[test]
    fn recognized_targets_return_their_path_and_kind() {
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let env = env(&scope);
        for (cmd, expected) in [
            ("rm -rf /", "/ (file system root)"),
            ("rm -rf /usr", "/usr (top-level directory)"),
            ("rm -rf /home/me", "/home/me (home directory)"),
            ("rm -rf ~", "/home/me (home directory)"),
            ("rm -rf ~/", "/home/me (home directory)"),
            ("rm -rf $HOME", "/home/me (home directory)"),
            ("rm -rf \"${HOME}\"", "/home/me (home directory)"),
            ("rm -rf /home/me/proj", "/home/me/proj (workspace root)"),
            ("rm -rf /home/me/proj/", "/home/me/proj (workspace root)"),
            ("rm -rf ..", "/home/me (home directory)"),
            ("rm -rf -- /", "/ (file system root)"),
            ("rmdir /tmp", "/tmp (top-level directory)"),
            ("sudo rm -rf /", "/ (file system root)"),
            ("sudo env A=1 rm -rf /", "/ (file system root)"),
            ("timeout 5 rm -rf /", "/ (file system root)"),
            ("nice -n 5 rm -rf /", "/ (file system root)"),
            ("xargs rm -rf /", "/ (file system root)"),
            ("ls; rm -rf /", "/ (file system root)"),
            ("echo $(rm -rf /)", "/ (file system root)"),
            ("sh -c 'rm -rf /'", "/ (file system root)"),
            ("rm.exe -rf /usr", "/usr (top-level directory)"),
            ("RM.EXE -rf /usr", "/usr (top-level directory)"),
        ] {
            assert_eq!(deletions(cmd, None, &env), [expected], "{cmd}");
        }
        assert_eq!(
            deletions("rm -rf /home", None, &env),
            ["/home (top-level directory)"]
        );
        assert_eq!(
            deletions("rm -rf ~", None, &env),
            ["/home/me (home directory)"]
        );
        assert_eq!(
            deletions("rm -rf /home /home", None, &env).len(),
            1,
            "one entry per distinct place"
        );
        assert_eq!(
            deletions("rm -rf sub", Some("/"), &env),
            ["/sub (top-level directory)"]
        );
        assert_eq!(
            deletions("rm -rf .", Some("/"), &env),
            ["/ (file system root)"]
        );
        assert!(deletions("rm -rf sub", Some("/home/me/proj"), &env).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn what_cannot_be_read_as_a_literal_target_adds_nothing() {
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let env = env(&scope);
        for cmd in [
            "bin=$(ls -t target/debug/deps/agents-* | grep -v '\\.d$' | head -1); ($bin t)",
            "while pgrep -x rustc >/dev/null; do sleep 3; done",
            "BIN=$(ls -t x); ($BIN t --exact)",
            "X=rm; $X -rf ~",
            "${X}m -rf /",
            "ls;rm x",
            "find . -exec rm {} +",
            "find / -exec rm -rf '{}' +",
            "xargs -a list rm -rf",
            "ls | xargs rm -rf",
            "rm",
            "rm -rf",
            "rm -rf *",
            "rm -rf $X",
            "rm -rf {/,x}",
            "rm -rf ~other",
            "rm -rf ~/x",
            "rm -rf $HOME/x",
            "HOME=/tmp; rm -rf $HOME",
            "rm -r 'unterminated",
            "X=r; ${X}m -rf /",
            "$(printf r)m -rf ~",
            "HOME=/tmp/x; rm -rf ~",
            "grep -rw rm /etc",
            "echo rm /",
            "which rm /usr",
            "git rm -r --cached .",
            "rm x",
            "rm -rf build",
            "rm -rf /home/me/proj/target",
            "git rm src/a.rs",
            "echo format",
            "ls $HOME",
        ] {
            assert_eq!(deletions(cmd, None, &env), Vec::<String>::new(), "{cmd}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn home_targets_need_a_known_home() {
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let env = CriticalEnv {
            scope: &scope,
            home: None,
            scratch: None,
        };
        assert!(deletions("rm -rf ~", None, &env).is_empty());
        assert!(deletions("rm -rf $HOME", None, &env).is_empty());
        assert!(!deletions("rm -rf /", None, &env).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_parent_of_a_workspace_root_is_named_as_such() {
        let scope = Scope::new(PathBuf::from("/srv/work/proj"), []);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new("/home/me")),
            scratch: None,
        };
        assert_eq!(
            deletions("rm -rf /srv/work", None, &env),
            ["/srv/work (parent of a workspace root)"]
        );
        assert_eq!(
            deletions("rm -rf /srv/work/proj", None, &env),
            ["/srv/work/proj (workspace root)"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_scratch_directory_is_not_critical_but_its_parents_are() {
        let scratch = Path::new("/srv/work/scratch/session-1");
        let scope = Scope::new(PathBuf::from("/srv/work/proj"), [scratch.to_path_buf()]);
        let env = CriticalEnv {
            scope: &scope,
            home: Some(Path::new("/home/me")),
            scratch: Some(scratch),
        };
        assert!(deletions("rm -rf /srv/work/scratch/session-1", None, &env).is_empty());
        assert!(!deletions("rm -rf /srv/work/scratch", None, &env).is_empty());
        assert!(!deletions("rm -rf /srv/work/proj", None, &env).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_critical_place_is_named_by_where_it_leads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        std::os::unix::fs::symlink("/usr", base.join("usr-link")).expect("symlink");
        let scope = Scope::new(base, []);
        let env = env(&scope);
        assert_eq!(
            deletions("rm -rf usr-link", None, &env),
            ["/usr (top-level directory)"]
        );
        assert!(deletions("rm -rf other", None, &env).is_empty());
    }

    #[test]
    fn other_tools_carry_nothing() {
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let env = env(&scope);
        let args = serde_json::json!({ "command": "rm -rf /" });
        assert!(critical_deletions("read", &args, &env).is_empty());
        assert!(critical_deletions("shell", &serde_json::json!({}), &env).is_empty());
    }
}
