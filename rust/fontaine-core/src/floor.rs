//! The structured checks of the deterministic floor that are not rules: the critical-path check
//! for `rm`/`rmdir` and the protected-path check for `edit`/`write`. Both work on parsed
//! commands and resolved paths, never on a pattern over raw arguments.

use std::path::{Component, Path, PathBuf};

use fontaine_tools::preclassify::{ShellAnalysis, ShellWord, analyze, raw_mentions_word};
use fontaine_tools::scope::{Scope, fold};
use serde_json::Value;

/// Directories whose contents `edit` and `write` never touch without a prompt.
pub const PROTECTED_DIRS: [&str; 5] = [".git", ".vscode", ".idea", ".husky", ".fontaine"];

/// Files `edit` and `write` never touch without a prompt, wherever they sit.
pub const PROTECTED_FILES: [&str; 9] = [
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
];

const DELETERS: [&str; 2] = ["rm", "rmdir"];

fn same(a: &std::ffi::OsStr, b: &str) -> bool {
    let a = a.to_string_lossy();
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn under(path: &Path, root: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        let mut p = path.components();
        for r in root.components() {
            let c = p.next()?;
            if !c
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&r.as_os_str().to_string_lossy())
            {
                return None;
            }
        }
        Some(p.as_path().to_path_buf())
    } else {
        path.strip_prefix(root).ok().map(Path::to_path_buf)
    }
}

/// Whether a write to `path` (as written and as resolved) lands in a protected location, in
/// any workspace root or the home directory.
pub fn protected_path(scope: &Scope, home: Option<&Path>, args: &Value) -> Option<String> {
    let given = args.get("path")?.as_str()?;
    for path in [scope.written(given), scope.resolved(given)] {
        let relative = scope
            .roots()
            .iter()
            .map(PathBuf::as_path)
            .chain(home)
            .find_map(|root| under(&path, root))
            .unwrap_or_else(|| path.clone());
        let names: Vec<&std::ffi::OsStr> = relative
            .components()
            .filter_map(|c| match c {
                Component::Normal(name) => Some(name),
                _ => None,
            })
            .collect();
        if let Some(dir) = names
            .iter()
            .find_map(|n| PROTECTED_DIRS.iter().find(|d| same(n, d)))
        {
            return Some(format!("protected directory {dir}"));
        }
        if let Some(file) = names
            .last()
            .and_then(|n| PROTECTED_FILES.iter().find(|f| same(n, f)))
        {
            return Some(format!("protected file {file}"));
        }
    }
    None
}

/// What the critical-path check needs to know about the machine.
pub struct CriticalEnv<'a> {
    pub scope: &'a Scope,
    pub home: Option<&'a Path>,
}

/// Why a shell command is critical: it deletes the file system root, a top-level directory, the
/// home directory, a workspace root or a parent of one, or it cannot be shown not to.
pub fn critical_shell(command: &str, cwd: Option<&str>, env: &CriticalEnv<'_>) -> Option<String> {
    let base = match cwd {
        Some(cwd) => env.scope.resolved(cwd),
        None => env.scope.primary().to_path_buf(),
    };
    scan(&analyze(command), &base, env)
}

fn scan(analysis: &ShellAnalysis, base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    if (!analysis.parsed || analysis.unanalyzed) && raw_mentions_word(&analysis.raw, &DELETERS) {
        return Some("a command that cannot be read mentions rm".to_string());
    }
    let mut mentions_deleter = false;
    for command in &analysis.commands {
        // An expanded command name (`${X}m -rf /`) could be rm.
        if command.words.first().is_some_and(|w| w.literal.is_none()) {
            return Some(format!(
                "command name `{}` cannot be resolved",
                command.words[0].raw
            ));
        }
        for (at, word) in command.words.iter().enumerate() {
            if is_deleter(word) {
                mentions_deleter = true;
                if let Some(found) = check_targets(&command.words[at + 1..], base, env) {
                    return Some(found);
                }
            }
        }
    }
    // Chains, pipes and lists hide what runs when; an rm among them is critical.
    if mentions_deleter && (analysis.simple.is_none() || analysis.nested_chain) {
        return Some("rm inside a command that is not one simple command".to_string());
    }
    None
}

fn is_deleter(word: &ShellWord) -> bool {
    word.literal.as_deref().is_some_and(|text| {
        let name = text.rsplit(['/', '\\']).next().unwrap_or(text);
        DELETERS.contains(&name)
    })
}

fn check_targets(rest: &[ShellWord], base: &Path, env: &CriticalEnv<'_>) -> Option<String> {
    let mut options_ended = false;
    for word in rest {
        let Some(text) = &word.literal else {
            return Some(format!("rm target `{}` cannot be resolved", word.raw));
        };
        if !options_ended && text == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && text.starts_with('-') {
            continue;
        }
        if is_critical_target(text, base, env) {
            return Some(format!("rm target `{text}`"));
        }
    }
    None
}

fn is_critical_target(target: &str, base: &Path, env: &CriticalEnv<'_>) -> bool {
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
        .any(|path| is_critical(&path, env))
}

fn is_critical(path: &Path, env: &CriticalEnv<'_>) -> bool {
    let is_root = |p: &Path| p.parent().is_none();
    if is_root(path) {
        return true;
    }
    if path.parent().is_some_and(is_root) {
        return true;
    }
    if env.home.is_some_and(|home| same_path(path, home)) {
        return true;
    }
    env.scope
        .roots()
        .iter()
        .any(|root| same_path(path, root) || starts_with(root, path))
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
        }
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
        ] {
            assert!(critical_shell(cmd, None, &env).is_some(), "{cmd}");
        }
        assert!(critical_shell(r"rm -rf 'C:\Users\me\proj\target'", None, &env).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn critical_targets_on_unix() {
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let env = env(&scope);
        for cmd in [
            "rm -rf /",
            "rm -rf /usr",
            "rm -rf ~",
            "rm -rf /home/me",
            "rm -rf /home/me/proj",
            "rm -rf /home",
            "rm -rf .",
            "rm -rf ..",
            "rm -rf *",
            "rm -rf $X",
            "rm -rf {/,x}",
            "rmdir /tmp",
            "sudo rm -rf /",
            "ls; rm -rf /",
            "echo $(rm -rf /)",
            "sh -c 'rm -rf /'",
            "rm -rf -- /",
            "rm -r 'unterminated",
            "su -c 'rm -rf /'",
            "watch 'rm -rf ~'",
            "flock /tmp/l -c 'rm -rf /'",
            "script -c 'rm -rf /' /dev/null",
            "env -S 'rm -rf /'",
            "runuser -c 'rm -rf /' bob",
            "ssh host 'rm -rf /'",
            "X=r; ${X}m -rf /",
            "$(printf r)m -rf ~",
            "bash -c 'cd / && rm -rf usr'",
            "sh -c 'ls; rm x'",
        ] {
            assert!(critical_shell(cmd, None, &env).is_some(), "{cmd}");
        }
        for cmd in [
            "rm x",
            "rm -rf build",
            "rm -rf /home/me/proj/target",
            "rm -rf ./a/b",
            "ls /",
            "rm",
            "echo format",
            "git rm src/a.rs",
            "git commit -m \"don't rm this\"",
            "git commit -m \"it's a format fix\"",
            "ls $HOME",
            "echo $(date)",
            "bash -c 'rm x'",
        ] {
            assert!(critical_shell(cmd, None, &env).is_none(), "{cmd}");
        }
        assert!(critical_shell("rm -rf sub", Some("/home/me/proj"), &env).is_none());
        assert!(critical_shell("rm -rf sub", Some("/"), &env).is_some());
        assert!(critical_shell("rm -rf .", Some("/"), &env).is_some());
    }

    #[test]
    fn protected_locations() {
        let root = std::env::temp_dir();
        let root = std::fs::canonicalize(root).unwrap();
        let scope = Scope::new(root.clone(), []);
        let hit = |path: &str| {
            protected_path(&scope, None, &serde_json::json!({ "path": path })).is_some()
        };
        for path in [
            ".git/config",
            "sub/.git/hooks/pre-commit",
            ".fontaine/settings.toml",
            ".vscode/x.json",
            ".bashrc",
            "a/b/.zshrc",
            ".mcp.json",
            ".gitmodules",
        ] {
            assert!(hit(path), "{path}");
        }
        for path in ["src/main.rs", "gitconfig", ".github/ci.yml", "x/.gitignore"] {
            assert!(!hit(path), "{path}");
        }
    }
}
