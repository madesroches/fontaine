//! The structured checks of the deterministic floor that are not rules: the protected-path check
//! for `edit`/`write`, plus the secret-path and instruction-file helpers. They work on resolved
//! paths, never on a pattern over raw arguments.

use std::path::{Component, Path, PathBuf};

use crate::tools::scope::Scope;
use serde_json::Value;

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

/// Hook-runner configuration by file name at any depth: editing one plants code that the next
/// commit or rebase step runs.
const HOOK_CONFIG_FILES: [&str; 5] = [
    ".pre-commit-config.yaml",
    "lefthook.yml",
    "lefthook.yaml",
    ".lefthook.yml",
    ".lefthook.yaml",
];

/// Whether `path` (relative to a root) is hook-runner configuration or sits in the conventional
/// `.githooks` directory. The directory a repository's `core.hooksPath` names is checked by the
/// caller, which has to read the repository's configuration.
pub fn is_hook_config_path(path: &Path, fold: bool) -> bool {
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
    HOOK_CONFIG_FILES.iter().any(|f| same(last, f, fold))
        || parents.iter().any(|n| same(n, ".githooks", fold))
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hook_runner_config_is_named_exactly() {
        let hit = |path: &str| is_hook_config_path(Path::new(path), false);
        for path in [
            ".pre-commit-config.yaml",
            "sub/.pre-commit-config.yaml",
            "lefthook.yml",
            "lefthook.yaml",
            ".lefthook.yml",
            ".lefthook.yaml",
            ".githooks/pre-commit",
            "a/.githooks/sub/commit-msg",
        ] {
            assert!(hit(path), "{path}");
        }
        for path in [
            "docs/pre-commit-config.yaml.md",
            "pre-commit-config.yaml",
            "lefthook.yml.bak",
            "githooks/pre-commit",
            "src/.githooks.rs",
            ".githooks",
        ] {
            assert!(!hit(path), "{path}");
        }
        assert!(is_hook_config_path(Path::new("LEFTHOOK.YML"), true));
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
