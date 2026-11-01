//! The structured checks of the deterministic floor that are not rules: the critical-path check
//! for `rm`/`rmdir` and the protected-path check for `edit`/`write`. Both work on parsed
//! commands and resolved paths, never on a pattern over raw arguments.

use std::path::{Component, Path, PathBuf};

use fontaine_tools::preclassify::{
    ShellAnalysis, ShellWord, analyze, program_name, raw_mentions_word,
};
use fontaine_tools::scope::{Scope, fold, translate};
use serde_json::Value;

mod powershell;

pub use powershell::critical_powershell;

/// Directories whose contents `edit` and `write` never touch without a prompt.
pub const PROTECTED_DIRS: [&str; 6] =
    [".git", ".vscode", ".idea", ".husky", ".fontaine", ".claude"];

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

/// Credential files by exact name, wherever they sit.
const SECRET_FILES: [&str; 13] = [
    ".env",
    ".netrc",
    "_netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".pgpass",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_ecdsa_sk",
    "id_ed25519_sk",
];

/// Templates that share the `.env.` prefix but hold no secrets.
const ENV_TEMPLATES: [&str; 3] = [".env.example", ".env.sample", ".env.template"];

/// Extensions of key and certificate containers.
const SECRET_SUFFIXES: [(&str, &str); 6] = [
    (".pem", "*.pem"),
    (".key", "*.key"),
    (".p12", "*.p12"),
    (".pfx", "*.pfx"),
    (".jks", "*.jks"),
    (".keystore", "*.keystore"),
];

/// The pattern a file name matches when it marks a credential file. Always case-folded:
/// whether a name marks a secret must not depend on the file system.
pub fn secret_file_name(name: &std::ffi::OsStr) -> Option<&'static str> {
    let name = name.to_string_lossy().to_ascii_lowercase();
    if let Some(exact) = SECRET_FILES.iter().find(|f| name == **f) {
        return Some(exact);
    }
    if name.starts_with(".env.") && !ENV_TEMPLATES.contains(&name.as_str()) {
        return Some(".env.*");
    }
    if name.ends_with(".env") {
        return Some("*.env");
    }
    SECRET_SUFFIXES
        .iter()
        .find(|(suffix, _)| name.ends_with(suffix))
        .map(|(_, pattern)| *pattern)
}

/// The credential-file pattern `given` names, as written or as resolved (so a symlink to
/// `.env` is caught by where it leads).
pub fn secret_path(scope: &Scope, given: &str) -> Option<String> {
    [scope.written(given), scope.resolved(given)]
        .iter()
        .find_map(|path| secret_file_name(path.file_name()?).map(str::to_string))
}

/// Instruction files at any depth: an edit to one changes what later sessions are told to do.
const INSTRUCTION_FILES: [&str; 5] = [
    "FONTAINE.md",
    "FONTAINE.local.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    "AGENTS.md",
];

/// Whether `path` (resolved) is an instruction file, a file under a `rules` directory or inside
/// `.github`: places where an edit changes later security decisions or what CI runs, so the
/// guardian keeps judging them.
pub fn is_instruction_or_ci_path(path: &Path, fold: bool) -> bool {
    let names: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let Some((last, parents)) = names.split_last() else {
        return false;
    };
    INSTRUCTION_FILES.iter().any(|f| same(last, f, fold))
        || parents
            .iter()
            .any(|n| same(n, "rules", fold) || same(n, ".github", fold))
        || same(last, ".github", fold)
}

const DELETERS: [&str; 2] = ["rm", "rmdir"];

/// Windows and macOS file systems are case-insensitive by default, so `.MCP.json` names the
/// protected file there even before it exists.
pub(crate) const FOLD_CASE: bool = cfg!(any(windows, target_os = "macos"));

fn same(a: &std::ffi::OsStr, b: &str, fold: bool) -> bool {
    let a = a.to_string_lossy();
    if fold {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

pub(crate) fn under(path: &Path, root: &Path) -> Option<PathBuf> {
    under_with(path, root, FOLD_CASE)
}

fn under_with(path: &Path, root: &Path, fold: bool) -> Option<PathBuf> {
    if fold {
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
/// any workspace root or the home directory, or under one of Fontaine's own `owned` directories
/// (config and data), which a session rooted at or above them could otherwise rewrite.
pub fn protected_path(
    scope: &Scope,
    home: Option<&Path>,
    owned: &[PathBuf],
    args: &Value,
) -> Option<String> {
    protected_path_with(scope, home, owned, args, FOLD_CASE)
}

fn protected_path_with(
    scope: &Scope,
    home: Option<&Path>,
    owned: &[PathBuf],
    args: &Value,
    fold: bool,
) -> Option<String> {
    let given = args.get("path")?.as_str()?;
    for path in [scope.written(given), scope.resolved(given)] {
        if owned
            .iter()
            .any(|dir| under_with(&path, dir, fold).is_some())
        {
            return Some("Fontaine's own config or data directory".to_string());
        }
        let relative = scope
            .roots()
            .iter()
            .map(PathBuf::as_path)
            .chain(home)
            .find_map(|root| under_with(&path, root, fold))
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
            .find_map(|n| PROTECTED_DIRS.iter().find(|d| same(n, d, fold)))
        {
            return Some(format!("protected directory {dir}"));
        }
        if let Some(file) = names
            .last()
            .and_then(|n| PROTECTED_FILES.iter().find(|f| same(n, f, fold)))
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
                let launched = launches_targets(&command.words[..at]);
                if let Some(found) = check_targets(&command.words[at + 1..], launched, base, env) {
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
    word.literal
        .as_deref()
        .is_some_and(|text| DELETERS.contains(&&*program_name(text)))
}

/// Whether a launcher before the deleter supplies its operands at run time.
fn launches_targets(before: &[ShellWord]) -> bool {
    let names: Vec<Option<String>> = before
        .iter()
        .map(|w| w.literal.as_deref().map(|t| program_name(t).into_owned()))
        .collect();
    let has = |n: &str| names.iter().any(|name| name.as_deref() == Some(n));
    has("xargs")
        || has("parallel")
        || (has("find")
            && ["-exec", "-execdir", "-ok", "-okdir"]
                .iter()
                .any(|flag| has(flag)))
}

fn check_targets(
    rest: &[ShellWord],
    launched: bool,
    base: &Path,
    env: &CriticalEnv<'_>,
) -> Option<String> {
    if launched {
        return Some("rm run by a launcher that supplies its targets".to_string());
    }
    let mut operands = 0;
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
        operands += 1;
        if text == "{}" || is_critical_target(text, base, env) {
            return Some(format!("rm target `{text}`"));
        }
    }
    if operands == 0 {
        return Some("rm with no target word; its targets are unknown".to_string());
    }
    None
}

/// An rm operand as the Windows file system sees it: `/c/...` names a drive under MSYS.
fn native_operand(target: &str, windows: bool) -> String {
    translate(target, windows).unwrap_or_else(|| target.to_string())
}

fn is_critical_target(target: &str, base: &Path, env: &CriticalEnv<'_>) -> bool {
    is_critical_text(&native_operand(target, cfg!(windows)), base, env)
}

/// Whether `target`, a path as the file system reads it, is a critical place.
fn is_critical_text(target: &str, base: &Path, env: &CriticalEnv<'_>) -> bool {
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
            "rm -rf /c/Users/me",
            "rm -rf /c/Users/me/proj",
            "rm -rf /c/Users",
        ] {
            assert!(critical_shell(cmd, None, &env).is_some(), "{cmd}");
        }
        assert!(critical_shell(r"rm -rf 'C:\Users\me\proj\target'", None, &env).is_none());
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
            "find / -exec rm -rf '{}' +",
            "find . -exec rm {} \\;",
            "xargs -a list rm -rf",
            "ls | xargs rm -rf",
            "rm",
            "rm -rf",
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
            "rm.exe -rf ~",
            "RM.EXE -rf ~",
            r"'C:\Program Files\Git\usr\bin\rm.exe' -rf ~",
            "bash.exe -c 'rm -rf ~'",
            "xargs.exe rm -rf",
        ] {
            assert!(critical_shell(cmd, None, &env).is_some(), "{cmd}");
        }
        for cmd in [
            "rm x",
            "rm -rf build",
            "rm -rf /home/me/proj/target",
            "rm -rf ./a/b",
            "ls /",
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
            protected_path(&scope, None, &[], &serde_json::json!({ "path": path })).is_some()
        };
        for path in [
            ".git/config",
            "sub/.git/hooks/pre-commit",
            ".fontaine/settings.toml",
            ".claude/settings.json",
            ".claude/rules/x.md",
            ".claude/CLAUDE.md",
            ".vscode/x.json",
            ".bashrc",
            "a/b/.zshrc",
            ".mcp.json",
            ".gitmodules",
        ] {
            assert!(hit(path), "{path}");
        }
        for path in [
            "src/main.rs",
            "gitconfig",
            ".github/ci.yml",
            "x/.gitignore",
            "CLAUDE.md",
            "AGENTS.md",
            "FONTAINE.md",
        ] {
            assert!(!hit(path), "{path}");
        }
    }

    #[test]
    fn case_folding_protects_differently_cased_names() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let scope = Scope::new(root.clone(), []);
        let hit = |path: &str, fold: bool| {
            protected_path_with(
                &scope,
                None,
                &[],
                &serde_json::json!({ "path": path }),
                fold,
            )
            .is_some()
        };
        for path in [".MCP.json", ".VSCODE/tasks.json", ".Husky/pre-commit"] {
            assert!(hit(path, true), "{path}");
        }
        assert!(!hit(".MCP.json", false));
    }

    #[test]
    fn owned_directories_are_protected() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let scope = Scope::new(root.clone(), []);
        let owned = [root.join("cfg/fontaine"), root.join("data/fontaine")];
        let hit = |path: &str| {
            protected_path(&scope, None, &owned, &serde_json::json!({ "path": path })).is_some()
        };
        assert!(hit("cfg/fontaine/settings.toml"));
        assert!(hit("data/fontaine/audit/x.jsonl"));
        assert!(hit(root.join("cfg/fontaine/trust.toml").to_str().unwrap()));
        assert!(!hit("cfg/other/settings.toml"));
    }

    #[test]
    fn instruction_and_ci_paths_are_named_exactly() {
        let hit = |path: &str, fold: bool| is_instruction_or_ci_path(Path::new(path), fold);
        for path in [
            "FONTAINE.md",
            "FONTAINE.local.md",
            "CLAUDE.md",
            "CLAUDE.local.md",
            "AGENTS.md",
            "sub/dir/AGENTS.md",
            "docs/rules/style.md",
            ".github/workflows/ci.yml",
            "a/.github/CODEOWNERS",
        ] {
            assert!(hit(path, false), "{path}");
        }
        for path in [
            "AGENTS.md.bak",
            "notes/CLAUDE.md.txt",
            "src/main.rs",
            "rules.md",
            "github/ci.yml",
        ] {
            assert!(!hit(path, false), "{path}");
        }
        assert!(!hit("agents.md", false));
        assert!(hit("agents.md", true));
        assert!(hit("Docs/RULES/x.md", true));
    }

    #[test]
    fn secret_names() {
        let hit = |n: &str| secret_file_name(std::ffi::OsStr::new(n)).is_some();
        for n in [
            ".env",
            ".env.local",
            ".env.production",
            "prod.env",
            ".netrc",
            "_netrc",
            ".npmrc",
            ".pypirc",
            ".git-credentials",
            ".pgpass",
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "id_ecdsa_sk",
            "id_ed25519_sk",
            "a.pem",
            "a.key",
            "a.p12",
            "a.pfx",
            "a.jks",
            "a.keystore",
            "cert.PEM",
            ".ENV",
            "PROD.ENV",
        ] {
            assert!(hit(n), "{n}");
        }
        for n in [
            ".env.example",
            ".env.sample",
            ".env.template",
            "README.md",
            "id_rsa.pub",
            "environment.rs",
            "env",
            ".envrc",
            "keyboard.rs",
        ] {
            assert!(!hit(n), "{n}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn secret_symlink_is_caught_by_its_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        std::fs::write(root.join(".env"), "K=1").expect("write");
        std::os::unix::fs::symlink(root.join(".env"), root.join("config")).expect("symlink");
        std::fs::write(root.join("plain"), "x").expect("write");
        let scope = Scope::new(root, []);
        assert_eq!(secret_path(&scope, "config").as_deref(), Some(".env"));
        assert_eq!(secret_path(&scope, "sub/../.env").as_deref(), Some(".env"));
        assert!(secret_path(&scope, "plain").is_none());
    }
}
