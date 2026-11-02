//! The shell working directory that persists between an agent's calls. Only the directory a
//! foreground command ended in is kept, and only while it stays inside the workspace roots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::lock::lock;
use crate::scope::Scope;

/// The POSIX wrapper a foreground call runs under, given the command and the file the final
/// directory goes to as positional arguments, so the command text is data and never spliced in.
/// The inputs move into read-only variables and the positional parameters are cleared before
/// `eval`, so the command sees the same `$#`, `$@` and `shift` as under `sh -c <command>` and
/// cannot disturb the trailer by assigning a variable of its own. A command that `exit`s, is
/// `exec`ed over or fails under `set -e` never reaches the trailer, which leaves the directory
/// unchanged.
pub const TRACKED_SCRIPT: &str = "readonly __fnt_f=\"$2\" __fnt_c=\"$1\"; set --; eval \"$__fnt_c\"; \
     __fnt_rc=$?; command pwd -P >|\"$__fnt_f\" 2>/dev/null; exit \"$__fnt_rc\"";

/// A tracked directory the call moved out of the workspace, or that is no longer a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reset {
    pub from: PathBuf,
    /// Whether `from` lies in a root but is not a directory (any longer).
    pub missing: bool,
}

impl Reset {
    /// What the model is told so it learns why its `cd` did not stick.
    pub fn notice(&self, root: &Path) -> String {
        let why = if self.missing {
            "is not a directory"
        } else {
            "is outside the workspace"
        };
        format!(
            "[shell working directory reset to {}: {} {why}]",
            root.display(),
            self.from.display()
        )
    }
}

/// The current shell directory of every agent that moved it. There is one set of shell tools
/// for all agents, told apart by their id, so one tracker serves them all; an agent without an
/// entry is in the primary root, which is also how a subagent starts.
#[derive(Clone, Debug, Default)]
pub struct ShellCwd(Arc<Mutex<HashMap<String, PathBuf>>>);

impl ShellCwd {
    /// Where `agent`'s next call starts when it names no `cwd`.
    pub fn current(&self, agent: &str, scope: &Scope) -> PathBuf {
        self.tracked(agent)
            .unwrap_or_else(|| scope.primary().to_path_buf())
    }

    /// The directory `agent` moved to, if it is not in the primary root.
    pub fn tracked(&self, agent: &str) -> Option<PathBuf> {
        // Poisoning panics: this value decides which directory a gated call is judged in.
        lock(&self.0, "shell directory").get(agent).cloned()
    }

    /// Puts `agent` back in the primary root.
    pub fn reset(&self, agent: &str) {
        lock(&self.0, "shell directory").remove(agent);
    }

    /// Records the directory a command ended in. It is kept when it is a directory inside a
    /// root (symlinks resolved); otherwise `agent` is put back in the primary root.
    pub fn observe(&self, agent: &str, reported: &Path, scope: &Scope) -> Option<Reset> {
        let inside = scope
            .find(&reported.to_string_lossy())
            .ok()
            .map(|(_, resolved)| resolved);
        match inside {
            Some(resolved) if resolved.is_dir() => {
                let mut map = lock(&self.0, "shell directory");
                if resolved == scope.primary() {
                    map.remove(agent);
                } else {
                    map.insert(agent.to_string(), resolved);
                }
                None
            }
            other => {
                self.reset(agent);
                Some(Reset {
                    from: reported.to_path_buf(),
                    missing: other.is_some(),
                })
            }
        }
    }

    /// Makes the starting directory of a shell call explicit: `args` with `cwd` set to the
    /// tracked directory when the call names none. The rewrite depends only on the tracker, so
    /// the gate, the hooks and the launch all see the directory the command will run in. A
    /// tracked directory that was deleted meanwhile is dropped, and the returned note says so.
    pub fn start_in(&self, agent: &str, scope: &Scope, args: Value) -> (Value, Option<String>) {
        let Value::Object(mut map) = args else {
            return (args, None);
        };
        if map.get("cwd").is_some_and(|cwd| !cwd.is_null()) {
            return (Value::Object(map), None);
        }
        let Some(tracked) = self.tracked(agent) else {
            return (Value::Object(map), None);
        };
        if !tracked.is_dir() {
            self.reset(agent);
            let note = Reset {
                from: tracked,
                missing: true,
            }
            .notice(scope.primary());
            return (Value::Object(map), Some(note));
        }
        map.insert(
            "cwd".to_string(),
            Value::String(tracked.to_string_lossy().into_owned()),
        );
        (Value::Object(map), None)
    }
}

/// The scratch file one foreground call's final directory is written to; removed when dropped.
#[derive(Debug)]
pub struct CwdFile(PathBuf);

impl CwdFile {
    pub fn new(scratch: &Path) -> CwdFile {
        CwdFile(scratch.join(format!("cwd-{}.txt", uuid::Uuid::new_v4().simple())))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The directory the command reported; `None` when it did not get to report one.
    pub fn reported(&self) -> Option<PathBuf> {
        let text = std::fs::read_to_string(&self.0).ok()?;
        let line = text.trim_end_matches(['\r', '\n']);
        (!line.is_empty()).then(|| PathBuf::from(line))
    }
}

impl Drop for CwdFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Dirs {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        outside: PathBuf,
    }

    fn dirs() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("work");
        let outside = base.join("elsewhere");
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Dirs {
            _tmp: tmp,
            root,
            outside,
        }
    }

    fn scope(d: &Dirs) -> Scope {
        Scope::new(d.root.clone(), [])
    }

    #[test]
    fn a_subdirectory_is_kept_and_the_primary_root_is_the_default() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        assert_eq!(cwd.current("root", &scope), d.root);
        assert_eq!(cwd.observe("root", &d.root.join("sub"), &scope), None);
        assert_eq!(cwd.current("root", &scope), d.root.join("sub"));
        assert_eq!(cwd.observe("root", &d.root.join("sub"), &scope), None);
        assert_eq!(cwd.observe("root", &d.root, &scope), None);
        assert_eq!(cwd.tracked("root"), None);
    }

    #[test]
    fn leaving_the_roots_resets_to_the_primary_root() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        cwd.observe("root", &d.root.join("sub"), &scope);
        let reset = cwd.observe("root", &d.outside, &scope).unwrap();
        assert_eq!(reset.from, d.outside);
        assert!(!reset.missing);
        assert_eq!(cwd.current("root", &scope), d.root);
        assert!(reset.notice(&d.root).contains("is outside the workspace]"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_roots_is_refused() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        std::os::unix::fs::symlink(&d.outside, d.root.join("escape")).unwrap();
        assert!(
            cwd.observe("root", &d.root.join("escape"), &scope)
                .is_some()
        );
        assert_eq!(cwd.tracked("root"), None);
    }

    #[test]
    fn a_removed_directory_resets() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        let gone = d.root.join("sub/deeper");
        cwd.observe("root", &gone, &scope);
        std::fs::remove_dir(&gone).unwrap();
        let reset = cwd.observe("root", &gone, &scope).unwrap();
        assert!(reset.missing);
        assert_eq!(cwd.tracked("root"), None);
    }

    #[test]
    fn each_agent_has_its_own_directory() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        cwd.observe("child", &d.root.join("sub"), &scope);
        assert_eq!(cwd.current("root", &scope), d.root);
        assert_eq!(cwd.current("child", &scope), d.root.join("sub"));
        cwd.reset("child");
        assert_eq!(cwd.current("child", &scope), d.root);
    }

    #[test]
    fn a_call_without_cwd_starts_in_the_tracked_directory() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        let call = json!({"command": "pwd"});
        assert_eq!(cwd.start_in("root", &scope, call.clone()), (call, None));
        cwd.observe("root", &d.root.join("sub"), &scope);
        let sub = d.root.join("sub").to_string_lossy().into_owned();
        for absent in [
            json!({"command": "pwd"}),
            json!({"command": "pwd", "cwd": null}),
        ] {
            let (args, note) = cwd.start_in("root", &scope, absent);
            assert_eq!(args, json!({"command": "pwd", "cwd": sub}));
            assert_eq!(note, None);
        }
        let named = json!({"command": "pwd", "cwd": "sub/deeper"});
        assert_eq!(cwd.start_in("root", &scope, named.clone()), (named, None));
    }

    #[test]
    fn a_deleted_tracked_directory_resets_with_a_note() {
        let d = dirs();
        let (cwd, scope) = (ShellCwd::default(), scope(&d));
        let doomed = d.root.join("sub/deeper");
        cwd.observe("root", &doomed, &scope);
        std::fs::remove_dir(&doomed).unwrap();
        let call = json!({"command": "pwd"});
        let (args, note) = cwd.start_in("root", &scope, call.clone());
        assert_eq!(args, call);
        assert!(note.unwrap().contains("is not a directory"));
        assert_eq!(cwd.tracked("root"), None);
    }

    #[test]
    fn the_reported_directory_is_read_and_the_file_removed() {
        let d = dirs();
        let file = CwdFile::new(&d.root);
        assert_eq!(file.reported(), None);
        std::fs::write(file.path(), "/some/dir\n").unwrap();
        assert_eq!(file.reported(), Some(PathBuf::from("/some/dir")));
        std::fs::write(file.path(), "").unwrap();
        assert_eq!(file.reported(), None);
        let path = file.path().to_path_buf();
        drop(file);
        assert!(!path.exists());
    }
}
