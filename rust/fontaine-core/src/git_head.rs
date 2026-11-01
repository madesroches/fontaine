//! The branch a directory's repository has checked out, read from `HEAD` without spawning
//! `git`: a spawned `git` in an untrusted repository can run helpers its config names.

use std::path::{Path, PathBuf};

/// What `HEAD` points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// The first seven hex digits of the commit.
    Detached(String),
}

impl Head {
    pub fn label(&self) -> &str {
        match self {
            Head::Branch(name) | Head::Detached(name) => name,
        }
    }
}

/// The `HEAD` file of the repository that holds `dir`, found the way `git` finds it (walking up,
/// following the `gitdir:` file of a linked worktree or submodule).
pub fn discover(dir: &Path) -> Option<PathBuf> {
    let options = gix::open::Options::isolated();
    let repo = gix::discover_opts(dir, gix::discover::upwards::Options::default(), options).ok()?;
    Some(repo.git_dir().join("HEAD"))
}

/// What the `HEAD` file at `head_file` says; `None` when it cannot be read or names neither a
/// branch nor a commit.
pub fn parse(head_file: &Path) -> Option<Head> {
    let text = std::fs::read_to_string(head_file).ok()?;
    let text = text.trim();
    if let Some(target) = text.strip_prefix("ref:") {
        let branch = target.trim().strip_prefix("refs/heads/")?;
        return (!branch.is_empty()).then(|| Head::Branch(branch.to_string()));
    }
    let hex = text.len() >= 7 && text.bytes().all(|b| b.is_ascii_hexdigit());
    hex.then(|| Head::Detached(text[..7].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn repo(dir: &Path, head: &str) -> PathBuf {
        let git = dir.join(".git");
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::create_dir_all(git.join("refs")).unwrap();
        std::fs::write(git.join("HEAD"), head).unwrap();
        git.join("HEAD")
    }

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        (tmp, base)
    }

    #[test]
    fn a_branch_is_read_from_head() {
        let (_tmp, base) = tmp();
        let head = repo(&base, "ref: refs/heads/feature/x\n");
        assert_eq!(discover(&base), Some(head.clone()));
        assert_eq!(parse(&head), Some(Head::Branch("feature/x".to_string())));
    }

    #[test]
    fn a_detached_head_shows_seven_digits() {
        let (_tmp, base) = tmp();
        let head = repo(&base, &format!("{HASH}\n"));
        let parsed = parse(&head).unwrap();
        assert_eq!(parsed, Head::Detached("0123456".to_string()));
        assert_eq!(parsed.label(), "0123456");
    }

    #[test]
    fn a_nested_directory_finds_the_repository_above_it() {
        let (_tmp, base) = tmp();
        let head = repo(&base, "ref: refs/heads/main\n");
        let nested = base.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(discover(&nested), Some(head));
    }

    #[test]
    fn a_linked_worktree_reads_its_own_head() {
        let (_tmp, base) = tmp();
        repo(&base.join("main"), "ref: refs/heads/main\n");
        let admin = base.join("main/.git/worktrees/wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let work = base.join("wt");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", work.join(".git").display()),
        )
        .unwrap();
        let head = discover(&work).expect("the worktree's repository is found");
        assert_eq!(parse(&head), Some(Head::Branch("topic".to_string())));
    }

    #[test]
    fn no_repository_and_an_unreadable_head_give_nothing() {
        let (_tmp, base) = tmp();
        assert_eq!(discover(&base), None);
        assert_eq!(parse(&base.join("missing")), None);
        let head = repo(&base, "garbage\n");
        assert_eq!(parse(&head), None);
        std::fs::write(&head, "ref: refs/remotes/origin/x\n").unwrap();
        assert_eq!(parse(&head), None);
    }

    #[test]
    fn a_rewritten_head_is_seen_through_the_cached_path() {
        let (_tmp, base) = tmp();
        let head = repo(&base, "ref: refs/heads/main\n");
        let cached = discover(&base).unwrap();
        std::fs::write(&head, "ref: refs/heads/scratch\n").unwrap();
        assert_eq!(parse(&cached), Some(Head::Branch("scratch".to_string())));
    }
}
