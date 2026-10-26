//! Rule files: markdown under a `rules/` directory, optionally scoped to paths by a `paths`
//! frontmatter key.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use super::Tier;
use super::markdown::strip_comments;
use crate::floor::under;
use crate::frontmatter;

/// A rule that applies only to files its patterns match, kept until a read or write brings one
/// into view.
#[derive(Clone, Debug)]
pub struct PathRule {
    /// The directory the patterns are relative to.
    pub base: PathBuf,
    pub file: PathBuf,
    pub(crate) globs: GlobSet,
    pub(crate) tier: Tier,
    /// The rule's effective text, read when the rule was registered.
    pub(crate) text: String,
}

impl PathRule {
    pub fn matches(&self, path: &Path) -> bool {
        under(path, &self.base).is_some_and(|relative| self.globs.is_match(relative))
    }
}

/// A rule file's effective text and the patterns it is scoped to, if any.
pub(crate) struct Parsed {
    pub(crate) text: String,
    pub(crate) paths: Option<Vec<String>>,
}

/// Frontmatter that does not parse means no `paths`: the whole file loads as an unconditional
/// rule.
pub(crate) fn parse(raw: &str) -> Parsed {
    match frontmatter::parse(raw) {
        Ok(doc) => Parsed {
            paths: doc.list("paths").filter(|paths| !paths.is_empty()),
            text: strip_comments(&doc.body),
        },
        Err(_) => Parsed {
            paths: None,
            text: strip_comments(raw),
        },
    }
}

/// The compiled patterns, and a message for each pattern that is not a valid glob.
pub(crate) fn compile(patterns: &[String]) -> (Option<GlobSet>, Vec<String>) {
    let mut set = GlobSetBuilder::new();
    let mut problems = Vec::new();
    let mut valid = 0;
    for pattern in patterns {
        match glob(pattern) {
            Ok(glob) => {
                set.add(glob);
                valid += 1;
            }
            Err(e) => problems.push(format!("the pattern `{pattern}` is not a valid glob: {e}")),
        }
    }
    let set = (valid > 0).then(|| set.build().ok()).flatten();
    (set, problems)
}

fn glob(pattern: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(pattern.trim_start_matches("./"))
        .literal_separator(true)
        .build()
}

/// The `.md` files under `dir`, following symlinks, ordered by relative path so the prompt is
/// the same from one run to the next.
pub(crate) fn rule_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(dir, &mut HashSet::new(), &mut found);
    found.sort_by_key(|file| {
        file.strip_prefix(dir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/")
    });
    found
}

fn walk(dir: &Path, visited: &mut HashSet<PathBuf>, found: &mut Vec<PathBuf>) {
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, visited, found);
        } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(patterns: &[&str], base: &Path) -> PathRule {
        let patterns: Vec<String> = patterns.iter().map(ToString::to_string).collect();
        PathRule {
            base: base.to_path_buf(),
            file: base.join("r.md"),
            globs: compile(&patterns).0.expect("valid"),
            tier: Tier::Repo,
            text: String::new(),
        }
    }

    #[test]
    fn paths_as_a_list_or_a_comma_string() {
        let list = parse("---\npaths:\n  - src/**/*.rs\n  - docs/*.md\n---\nbody\n");
        assert_eq!(list.paths.unwrap(), ["src/**/*.rs", "docs/*.md"]);
        assert_eq!(list.text.trim(), "body");
        let comma = parse("---\npaths: a/**, b/*.md\n---\nbody\n");
        assert_eq!(comma.paths.unwrap(), ["a/**", "b/*.md"]);
        assert!(parse("no header\n").paths.is_none());
        assert!(parse("---\ndescription: x\n---\nbody\n").paths.is_none());
    }

    #[test]
    fn a_header_that_does_not_parse_loads_unconditionally() {
        let parsed = parse("---\npaths: [a\nnever closed\n");
        assert!(parsed.paths.is_none());
        assert!(parsed.text.contains("never closed"));
    }

    #[test]
    fn star_matches_only_at_the_base_and_double_star_at_depth() {
        let base = PathBuf::from("/repo");
        let top = rule(&["*.md"], &base);
        assert!(top.matches(Path::new("/repo/a.md")));
        assert!(!top.matches(Path::new("/repo/sub/a.md")));
        let deep = rule(&["src/**/*.{rs,toml}"], &base);
        assert!(deep.matches(Path::new("/repo/src/a/b/c.rs")));
        assert!(deep.matches(Path::new("/repo/src/Cargo.toml")));
        assert!(!deep.matches(Path::new("/repo/lib/c.rs")));
        assert!(!deep.matches(Path::new("/elsewhere/src/c.rs")));
    }

    #[test]
    fn an_invalid_glob_is_dropped_and_the_others_keep_working() {
        let (set, problems) = compile(&["[".to_string(), "ok/*.md".to_string()]);
        assert_eq!(problems.len(), 1);
        assert!(set.unwrap().is_match("ok/a.md"));
        assert!(compile(&["[".to_string()]).0.is_none());
    }

    #[test]
    fn files_are_sorted_by_relative_path_and_a_symlink_cycle_ends() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("z.md"), "z").unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("b/c.md"), "c").unwrap();
        std::fs::write(root.join("b/skip.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root, root.join("b/loop")).unwrap();
        let names: Vec<String> = rule_files(root)
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.md", "b/c.md", "z.md"]);
    }
}
