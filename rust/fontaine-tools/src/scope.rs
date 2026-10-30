//! Workspace containment, decided by code and never by pattern matching on arguments. A path is
//! in scope when rig's own confinement rule (`fs::place` with `confine`) accepts it for some
//! root, so Fontaine's check and the confined tool instances can never disagree.

use std::fmt;
use std::path::{Path, PathBuf};

use rig_tools::fs::{place, resolve};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeError {
    /// The arguments hold no path field this check can read, so the call is out of scope.
    Unreadable(String),
    /// The path, as the model wrote it, lies outside every root.
    OutOfScope(String),
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScopeError::Unreadable(what) => write!(f, "{what}"),
            ScopeError::OutOfScope(path) => {
                write!(f, "{path} is outside the workspace roots")
            }
        }
    }
}

impl std::error::Error for ScopeError {}

/// Where a call lands: the index of the root whose tool instance serves it, and the arguments
/// to run with. The arguments are rewritten to an absolute path when the instance's own base
/// would read the model's path differently.
#[derive(Clone, Debug, PartialEq)]
pub struct Located {
    pub root: usize,
    pub args: Value,
}

enum PathFields {
    Required(&'static str),
    Optional(&'static str),
    Glob,
}

fn fields_of(tool: &str) -> Option<PathFields> {
    match tool {
        "read" | "write" | "edit" | "notebook_edit" => Some(PathFields::Required("path")),
        "ls" | "grep" => Some(PathFields::Optional("path")),
        "glob" => Some(PathFields::Glob),
        name if crate::descriptor::is_shell_class(name) => Some(PathFields::Optional("cwd")),
        _ => None,
    }
}

/// The read-only file tools whose out-of-root calls the gate may approve into the unconfined
/// fallback instance. Writes and `shell` never take this route.
pub fn is_external_read(tool: &str) -> bool {
    matches!(tool, "read" | "ls" | "glob" | "grep")
}

/// Whether `args` hold the shape the pipeline's normalizer produces for an out-of-root read:
/// an absolute target string, the literal base and pattern both present for `glob`. This
/// checks no file system state: the fallback instance consumes exactly what the pipeline
/// already resolved and never re-resolves the path a second time.
pub fn external_shape(tool: &str, args: &Value) -> Result<(), ScopeError> {
    if !is_external_read(tool) {
        return Err(unreadable(&format!("`{tool}` has no external read route")));
    }
    let absolute = |path: &str| {
        if Path::new(path).is_absolute() {
            Ok(())
        } else {
            Err(unreadable("the path is not an absolute resolved path"))
        }
    };
    match tool {
        "glob" => {
            args.get("pattern")
                .and_then(Value::as_str)
                .ok_or_else(|| unreadable("`pattern` is missing or not a string"))?;
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| unreadable("`path` is missing or not a string"))?;
            absolute(path)?;
        }
        _ => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| unreadable("`path` is missing or not a string"))?;
            absolute(path)?;
        }
    }
    Ok(())
}

/// The ordered, canonicalized roots of a session. The first is the primary root, which relative
/// paths and defaults refer to. Nothing a tool does can add a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    roots: Vec<PathBuf>,
}

impl Scope {
    /// `primary` and `added` must already be canonical; duplicates are dropped.
    pub fn new(primary: PathBuf, added: impl IntoIterator<Item = PathBuf>) -> Scope {
        let mut roots = vec![primary];
        for root in added {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        Scope { roots }
    }

    pub fn primary(&self) -> &Path {
        &self.roots[0]
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// This scope with one more root; `root` must already be canonical.
    pub fn with_root(&self, root: PathBuf) -> Scope {
        Scope::new(
            self.roots[0].clone(),
            self.roots[1..].iter().cloned().chain([root]),
        )
    }

    /// `path` resolved the way the file tools open it (symlinks followed, `..` folded), whether
    /// or not it lies inside a root. A path with no meaning on this platform stays as written.
    pub fn resolved(&self, path: &str) -> PathBuf {
        match translate(path, cfg!(windows)) {
            Some(translated) => resolve(Some(self.primary()), &translated),
            None => PathBuf::from(path),
        }
    }

    /// `path` joined to the primary root and folded lexically, without following symlinks: the
    /// path as the model wrote it.
    pub fn written(&self, path: &str) -> PathBuf {
        let translated = translate(path, cfg!(windows)).unwrap_or_else(|| path.to_string());
        fold(&self.primary().join(translated))
    }

    /// The root that holds `path`, and the path resolved the way the file tools open it.
    pub fn find(&self, path: &str) -> Result<(usize, PathBuf), ScopeError> {
        let out = || ScopeError::OutOfScope(path.to_string());
        let translated = translate(path, cfg!(windows)).ok_or_else(out)?;
        let resolved = resolve(Some(self.primary()), &translated);
        let text = resolved.to_string_lossy();
        self.roots
            .iter()
            .position(|root| place(Some(root), true, &text).is_ok())
            .map(|root| (root, resolved))
            .ok_or_else(out)
    }

    /// Whether a call to `tool` with `args` stays inside the roots, and which root serves it.
    pub fn locate(&self, tool: &str, args: &Value) -> Result<Located, ScopeError> {
        let empty = Map::new();
        let map = match args {
            Value::Object(map) => map,
            Value::Null => &empty,
            _ => return Err(unreadable("the arguments are not an object")),
        };
        let fields = fields_of(tool)
            .ok_or_else(|| unreadable(&format!("`{tool}` names no path this check can read")))?;
        match fields {
            PathFields::Required(key) => {
                let path = string_field(map, key)?
                    .ok_or_else(|| unreadable(&format!("`{key}` is missing")))?;
                self.locate_path(args, key, &path)
            }
            PathFields::Optional(key) => match string_field(map, key)? {
                None => Ok(Located {
                    root: 0,
                    args: args.clone(),
                }),
                Some(path) => self.locate_path(args, key, &path),
            },
            PathFields::Glob => {
                let pattern = string_field(map, "pattern")?
                    .ok_or_else(|| unreadable("`pattern` is missing"))?;
                let path = string_field(map, "path")?;
                let (base, tail) = glob_target(path.as_deref(), &pattern);
                let (root, resolved) = self.find(&base)?;
                if root == 0 && !is_rewritten(&base) {
                    return Ok(Located {
                        root,
                        args: args.clone(),
                    });
                }
                Ok(Located {
                    root,
                    args: serde_json::json!({
                        "pattern": tail,
                        "path": resolved.to_string_lossy(),
                    }),
                })
            }
        }
    }

    /// The resolved absolute form of a path outside every root. A path with no meaning on
    /// this platform, or whose resolution is not an absolute path, is an argument error:
    /// it must never fall back to the raw string for another interpreter to read.
    fn external_resolve(&self, path: &str) -> Result<PathBuf, ScopeError> {
        let translated = translate(path, cfg!(windows))
            .ok_or_else(|| unreadable("the path has no meaning on this platform"))?;
        let resolved = resolve(Some(self.primary()), &translated);
        if !resolved.is_absolute() {
            return Err(unreadable("the path does not resolve to an absolute path"));
        }
        Ok(resolved)
    }

    /// `args` with the target rewritten to its resolved absolute form, for a read-only tool
    /// whose target lies outside every root. Unlike `locate`, never refuses for containment:
    /// the gate has already decided. Malformed arguments stay `Unreadable`; every non-path
    /// argument is preserved, and a `glob` call is rewritten to the resolved literal base
    /// and the remaining pattern separately, as strict `locate` does.
    pub fn external_read(&self, tool: &str, args: &Value) -> Result<Value, ScopeError> {
        if !is_external_read(tool) {
            return Err(unreadable(&format!("`{tool}` has no external read route")));
        }
        let empty = Map::new();
        let map = match args {
            Value::Object(map) => map,
            Value::Null => &empty,
            _ => return Err(unreadable("the arguments are not an object")),
        };
        match fields_of(tool) {
            Some(PathFields::Required(key)) => {
                let path = string_field(map, key)?
                    .ok_or_else(|| unreadable(&format!("`{key}` is missing")))?;
                let resolved = self.external_resolve(&path)?;
                Ok(Self::replaced(args, key, &resolved))
            }
            Some(PathFields::Optional(key)) => match string_field(map, key)? {
                None => Ok(args.clone()),
                Some(path) => {
                    let resolved = self.external_resolve(&path)?;
                    Ok(Self::replaced(args, key, &resolved))
                }
            },
            Some(PathFields::Glob) => {
                let pattern = string_field(map, "pattern")?
                    .ok_or_else(|| unreadable("`pattern` is missing"))?;
                let path = string_field(map, "path")?;
                let (base, tail) = glob_target(path.as_deref(), &pattern);
                let resolved = self.external_resolve(&base)?;
                Ok(serde_json::json!({
                    "pattern": tail,
                    "path": resolved.to_string_lossy(),
                }))
            }
            None => Err(unreadable(&format!(
                "`{tool}` names no path this check can read"
            ))),
        }
    }

    /// The arguments with one path field replaced by its resolved absolute form.
    fn replaced(args: &Value, key: &str, resolved: &Path) -> Value {
        let mut map = match args {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        };
        map.insert(
            key.to_string(),
            Value::String(resolved.to_string_lossy().into()),
        );
        Value::Object(map)
    }

    fn locate_path(&self, args: &Value, key: &str, path: &str) -> Result<Located, ScopeError> {
        let (root, resolved) = self.find(path)?;
        let args = if root == 0 && !is_rewritten(path) {
            args.clone()
        } else {
            let mut map = match args {
                Value::Object(map) => map.clone(),
                _ => Map::new(),
            };
            map.insert(
                key.to_string(),
                Value::String(resolved.to_string_lossy().into()),
            );
            Value::Object(map)
        };
        Ok(Located { root, args })
    }
}

/// `.` and `..` components folded away without touching the file system.
pub fn fold(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// A path the primary root's instance would read differently from the way it was resolved:
/// only an MSYS-style path rewritten for Windows.
fn is_rewritten(written: &str) -> bool {
    translate(written, cfg!(windows)).is_some_and(|translated| translated != written)
}

fn unreadable(what: &str) -> ScopeError {
    ScopeError::Unreadable(what.to_string())
}

fn string_field(map: &Map<String, Value>, key: &str) -> Result<Option<String>, ScopeError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(unreadable(&format!("`{key}` is not a string"))),
    }
}

/// MSYS-style `/c/...` paths name Windows drives; any other path rooted at `/` has no Windows
/// meaning here and is out of scope.
pub fn translate(path: &str, windows: bool) -> Option<String> {
    if !windows || path.starts_with("//") {
        return Some(path.to_string());
    }
    let Some(rest) = path.strip_prefix('/') else {
        return Some(path.to_string());
    };
    let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let mut letters = drive.chars();
    match (letters.next(), letters.next()) {
        (Some(letter), None) if letter.is_ascii_alphabetic() => Some(format!(
            "{}:\\{}",
            letter.to_ascii_uppercase(),
            tail.replace('/', "\\")
        )),
        _ => None,
    }
}

/// A glob pattern split into its literal leading directory and the rest. The pattern is cut,
/// never re-joined: a Windows verbatim path (`\\?\C:\...`) does not accept `/` separators.
fn split_glob(pattern: &str, windows: bool) -> (String, String) {
    let is_meta = |c: char| matches!(c, '*' | '?' | '[' | ']' | '{' | '}');
    let is_sep = |c: char| c == '/' || (windows && c == '\\');
    // Canonical roots on Windows are verbatim paths, whose prefix carries a literal `?`.
    let start = if windows && pattern.starts_with(r"\\?\") {
        4
    } else {
        0
    };
    let body = &pattern[start..];
    let literal_end = body.find(is_meta).unwrap_or(body.len());
    let Some(cut) = body[..literal_end].rfind(is_sep).map(|at| at + start) else {
        return (String::new(), pattern.to_string());
    };
    // `/` and `C:\` keep their separator: without it they name no directory or the drive's cwd.
    let keep_sep = cut == 0 || (windows && pattern[..cut].ends_with(':'));
    let dir = if keep_sep {
        &pattern[..=cut]
    } else {
        &pattern[..cut]
    };
    (dir.to_string(), pattern[cut + 1..].to_string())
}

/// Where a `glob` call searches and the pattern left to match there: the pattern's literal
/// leading directory under `path` (or the working root), and the non-literal tail.
pub fn glob_target(path: Option<&str>, pattern: &str) -> (String, String) {
    let (dir, tail) = split_glob(pattern, cfg!(windows));
    let base = match (path, dir.as_str()) {
        (None, "") => ".".to_string(),
        (None, dir) => dir.to_string(),
        (Some(path), "") => path.to_string(),
        (Some(path), dir) => Path::new(path).join(dir).to_string_lossy().into_owned(),
    };
    (base, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_split_at_the_first_non_literal_component() {
        assert_eq!(
            split_glob("src/**/*.rs", false),
            ("src".into(), "**/*.rs".into())
        );
        assert_eq!(split_glob("*.rs", false), (String::new(), "*.rs".into()));
        assert_eq!(
            split_glob("/abs/x/*.rs", false),
            ("/abs/x".into(), "*.rs".into())
        );
        assert_eq!(split_glob("/*.rs", false), ("/".into(), "*.rs".into()));
        assert_eq!(
            split_glob("a/b/c.txt", false),
            ("a/b".into(), "c.txt".into())
        );
        assert_eq!(split_glob("c.txt", false), (String::new(), "c.txt".into()));
        assert_eq!(split_glob("a/**", false), ("a".into(), "**".into()));
    }

    #[test]
    fn windows_globs_split_at_either_separator_and_keep_the_verbatim_prefix() {
        assert_eq!(
            split_glob(r"\\?\C:\work\extra/**/*.rs", true),
            (r"\\?\C:\work\extra".into(), "**/*.rs".into())
        );
        assert_eq!(
            split_glob(r"C:\work\src\*.rs", true),
            (r"C:\work\src".into(), "*.rs".into())
        );
        assert_eq!(
            split_glob(r"\\?\C:\*", true),
            (r"\\?\C:\".into(), "*".into())
        );
        assert_eq!(
            split_glob(r"C:\*.ini", true),
            (r"C:\".into(), "*.ini".into())
        );
        assert_eq!(
            split_glob(r"src\*.rs", false),
            (String::new(), r"src\*.rs".into())
        );
    }

    #[test]
    fn glob_targets_anchor_under_the_path_argument() {
        assert_eq!(glob_target(None, "*.rs"), (".".into(), "*.rs".into()));
        assert_eq!(
            glob_target(Some("lib"), "*.rs"),
            ("lib".into(), "*.rs".into())
        );
        assert_eq!(
            glob_target(Some("lib"), "src/*.rs"),
            (
                Path::new("lib").join("src").to_string_lossy().into(),
                "*.rs".into()
            )
        );
        assert_eq!(
            glob_target(Some("lib"), "/etc/*.conf"),
            ("/etc".into(), "*.conf".into())
        );
    }

    #[test]
    fn msys_drive_paths_translate_on_windows_only() {
        assert_eq!(
            translate("/c/Users/me", true).as_deref(),
            Some("C:\\Users\\me")
        );
        assert_eq!(translate("/d", true).as_deref(), Some("D:\\"));
        assert_eq!(translate("/etc/passwd", true), None);
        assert_eq!(translate("/usr", true), None);
        assert_eq!(
            translate("//server/share/x", true).as_deref(),
            Some("//server/share/x")
        );
        assert_eq!(translate("C:\\x", true).as_deref(), Some("C:\\x"));
        assert_eq!(translate("rel/x", true).as_deref(), Some("rel/x"));
        assert_eq!(
            translate("/etc/passwd", false).as_deref(),
            Some("/etc/passwd")
        );
    }
}
