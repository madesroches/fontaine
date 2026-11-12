//! Safe read-only git command execution through `gix` with isolated, strict configuration.
//!
//! This module implements the runner for the shell pre-classifier's git fast path. It never
//! spawns the `git` binary; instead it opens the repository with isolated options and strict
//! config parsing, so repository-controlled helpers cannot execute.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fontaine_tools::preclassify::GitCommand;

/// Errors that can occur while executing a git fast-path command through `gix`.
#[derive(Debug)]
pub enum GitFastPathError {
    Open(gix::Error),
    Index(gix::Error),
    RevParse {
        spec: String,
        source: gix::Error,
    },
    Object {
        id: String,
        source: gix::Error,
    },
    NotACommit {
        spec: String,
    },
    NotATreeOrBlob {
        spec: String,
    },
    OutsideWorkTree {
        path: String,
    },
    WorktreeFile {
        path: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for GitFastPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitFastPathError::Open(_) => write!(f, "could not open the git repository"),
            GitFastPathError::Index(_) => write!(f, "could not read the repository index"),
            GitFastPathError::RevParse { spec, .. } => write!(f, "could not resolve {spec}"),
            GitFastPathError::Object { id, .. } => write!(f, "could not read object {id}"),
            GitFastPathError::NotACommit { spec } => write!(f, "{spec} is not a commit"),
            GitFastPathError::NotATreeOrBlob { spec } => {
                write!(f, "{spec} is not a tree or blob")
            }
            GitFastPathError::OutsideWorkTree { path } => {
                write!(f, "path {path} is outside the repository work tree")
            }
            GitFastPathError::WorktreeFile { path, .. } => {
                write!(f, "could not read worktree file {path}")
            }
        }
    }
}

impl std::error::Error for GitFastPathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitFastPathError::Open(e)
            | GitFastPathError::Index(e)
            | GitFastPathError::RevParse { source: e, .. }
            | GitFastPathError::Object { source: e, .. } => Some(e),
            GitFastPathError::WorktreeFile { source: e, .. } => Some(e),
            _ => None,
        }
    }
}

/// Open a repository with isolated, strict configuration.
fn open_repo(dir: &Path) -> Result<gix::Repository, GitFastPathError> {
    let options = gix::open::Options::isolated().strict_config(true);
    gix::discover_opts(dir, gix::discover::upwards::Options::default(), options)
        .map_err(GitFastPathError::Open)
}

/// Resolve `cwd` against `repo_root` and verify the result lies under the work tree.
fn resolve_cwd(repo_root: &Path, cwd: Option<&Path>) -> PathBuf {
    match cwd {
        None => repo_root.to_path_buf(),
        Some(cwd) if cwd.is_absolute() => cwd.to_path_buf(),
        Some(cwd) => repo_root.join(cwd),
    }
}

/// Make `path` absolute using `cwd` and verify it lies under `repo_root`.
fn resolve_path(repo_root: &Path, cwd: &Path, path: &str) -> Result<PathBuf, GitFastPathError> {
    let absolute = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let canonical = absolute.canonicalize().unwrap_or_else(|_| absolute.clone());
    if !canonical.starts_with(repo_root) {
        return Err(GitFastPathError::OutsideWorkTree {
            path: path.to_string(),
        });
    }
    Ok(canonical)
}

/// Convert an absolute worktree path to a repo-root-relative slash-separated string.
fn repo_relative(repo_root: &Path, path: &Path) -> Result<String, GitFastPathError> {
    let rel = path
        .strip_prefix(repo_root)
        .map_err(|_| GitFastPathError::OutsideWorkTree {
            path: path.display().to_string(),
        })?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Run a parsed git command through `gix` and return the rendered output.
pub fn run(
    cmd: GitCommand,
    repo_root: &Path,
    cwd: Option<&Path>,
) -> Result<String, GitFastPathError> {
    let repo = open_repo(repo_root)?;
    let cwd = resolve_cwd(repo_root, cwd);
    match cmd {
        GitCommand::Status => run_status(&repo, repo_root),
        GitCommand::Diff { cached, paths } => run_diff(&repo, repo_root, &cwd, cached, &paths),
        GitCommand::Log { max_count } => run_log(&repo, max_count),
        GitCommand::Show { rev, path, stat } => run_show(&repo, &rev, path.as_deref(), stat),
        GitCommand::LsFiles => run_ls_files(&repo),
        GitCommand::Branch {
            all,
            remotes,
            verbose,
        } => run_branch(&repo, all, remotes, verbose),
        GitCommand::RevParseHead => run_rev_parse_head(&repo),
        GitCommand::Tag { list, lines } => run_tag(&repo, list, lines),
    }
}

fn run_status(repo: &gix::Repository, repo_root: &Path) -> Result<String, GitFastPathError> {
    let index = repo.index_or_empty().map_err(GitFastPathError::Index)?;
    let head_tree = repo
        .head_tree_id_or_empty()
        .map_err(|e| GitFastPathError::Object {
            id: "HEAD".to_string(),
            source: e,
        })?;
    let head_tree = repo
        .find_tree(head_tree)
        .map_err(|e| GitFastPathError::Object {
            id: "HEAD".to_string(),
            source: e,
        })?;

    let workdir = repo.workdir().unwrap_or(repo_root);
    let backing = index.path_backing();
    let mut index_paths = BTreeSet::new();
    let mut changed = Vec::new();
    let mut staged = Vec::new();

    for entry in index.entries() {
        let path = entry.path_in(backing);
        let path_str = String::from_utf8_lossy(path).to_string();
        index_paths.insert(path_str.clone());

        let work_path = workdir.join(&path_str);
        let work_content = std::fs::read(&work_path).ok();
        let work_exists = work_content.is_some();

        let index_blob_id = entry.id;
        let head_entry = head_tree.lookup_entry_by_path(&path_str).ok().flatten();
        let head_id = head_entry.as_ref().map(|e| e.object_id());

        if !work_exists {
            changed.push(format!(" D {path_str}"));
        } else {
            let work_content = work_content.unwrap();
            let index_content = repo.find_blob(index_blob_id).map(|b| b.data.to_vec()).ok();
            if Some(work_content) != index_content {
                changed.push(format!(" M {path_str}"));
            }
        }

        if head_id != Some(index_blob_id) {
            staged.push(format!("M  {path_str}"));
        }
    }

    // Untracked files: walk the worktree and report files not in the index.
    let mut untracked = Vec::new();
    if let Ok(entries) = std::fs::read_dir(workdir) {
        collect_untracked(entries, &index_paths, "", &mut untracked)?;
    }

    let mut out = String::new();
    if !staged.is_empty() {
        out.push_str("Changes to be committed:\n");
        for line in staged {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    if !changed.is_empty() {
        out.push_str("Changes not staged for commit:\n");
        for line in changed {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    if !untracked.is_empty() {
        out.push_str("Untracked files:\n");
        for line in untracked {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("nothing to commit, working tree clean\n");
    }
    Ok(out)
}

fn collect_untracked(
    entries: std::fs::ReadDir,
    index_paths: &BTreeSet<String>,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<(), GitFastPathError> {
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            if index_paths
                .iter()
                .any(|p| p.starts_with(&format!("{rel}/")))
            {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    collect_untracked(entries, index_paths, &rel, out)?;
                }
            } else {
                out.push(format!("?? {rel}/"));
            }
        } else if !index_paths.contains(&rel) {
            out.push(format!("?? {rel}"));
        }
    }
    Ok(())
}

fn run_diff(
    repo: &gix::Repository,
    repo_root: &Path,
    cwd: &Path,
    cached: bool,
    paths: &[String],
) -> Result<String, GitFastPathError> {
    let index = repo.index_or_empty().map_err(GitFastPathError::Index)?;
    let head_tree_id = repo
        .head_tree_id_or_empty()
        .map_err(|e| GitFastPathError::Object {
            id: "HEAD".to_string(),
            source: e,
        })?;
    let head_tree = repo
        .find_tree(head_tree_id)
        .map_err(|e| GitFastPathError::Object {
            id: "HEAD".to_string(),
            source: e,
        })?;

    let workdir = repo.workdir().unwrap_or(repo_root);
    let backing = index.path_backing();
    let resolved_paths: Vec<String> = if paths.is_empty() {
        index
            .entries()
            .iter()
            .map(|e| String::from_utf8_lossy(e.path_in(backing)).to_string())
            .collect()
    } else {
        paths
            .iter()
            .map(|p| {
                let abs = resolve_path(repo_root, cwd, p)?;
                repo_relative(repo_root, &abs)
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut out = String::new();
    for rel in resolved_paths {
        let index_entry = index
            .entries()
            .iter()
            .find(|e| String::from_utf8_lossy(e.path_in(backing)) == rel);

        let old_content = if cached {
            head_tree
                .lookup_entry_by_path(&rel)
                .ok()
                .flatten()
                .and_then(|e| repo.find_blob(e.object_id()).ok().map(|b| b.data.to_vec()))
        } else {
            index_entry.as_ref().and_then(|e| {
                repo.find_blob(e.id.to_owned())
                    .ok()
                    .map(|b| b.data.to_vec())
            })
        };

        let new_content = if cached {
            index_entry.as_ref().and_then(|e| {
                repo.find_blob(e.id.to_owned())
                    .ok()
                    .map(|b| b.data.to_vec())
            })
        } else {
            let work_path = workdir.join(&rel);
            std::fs::read(&work_path).ok()
        };

        if old_content == new_content {
            continue;
        }

        let old_label = format!("a/{rel}");
        let new_label = format!("b/{rel}");
        out.push_str(&format!("diff --git {old_label} {new_label}\n"));
        out.push_str(&format!("--- {old_label}\n"));
        out.push_str(&format!("+++ {new_label}\n"));

        let old_lines = old_content.as_deref().unwrap_or_default();
        let new_lines = new_content.as_deref().unwrap_or_default();
        let diff = text_diff(old_lines, new_lines);
        out.push_str(&diff);
    }
    Ok(out)
}

fn text_diff(old: &[u8], new: &[u8]) -> String {
    let old_str = String::from_utf8_lossy(old);
    let new_str = String::from_utf8_lossy(new);
    let old_lines: Vec<&str> = old_str.split('\n').collect();
    let new_lines: Vec<&str> = new_str.split('\n').collect();

    // Simple line-by-line diff: report changed hunk for the whole file.
    if old_lines == new_lines {
        return String::new();
    }

    let old_len = old_lines.len();
    let new_len = new_lines.len();
    let mut out = format!("@@ -1,{old_len} +1,{new_len} @@\n");
    for line in &old_lines {
        out.push_str(&format!("-{line}\n"));
    }
    for line in &new_lines {
        out.push_str(&format!("+{line}\n"));
    }
    out
}

fn run_log(repo: &gix::Repository, max_count: Option<usize>) -> Result<String, GitFastPathError> {
    let head_id = repo.head_id().map_err(|e| GitFastPathError::Object {
        id: "HEAD".to_string(),
        source: e,
    })?;
    let max = max_count.unwrap_or(usize::MAX);
    let mut out = String::new();
    let walk = repo
        .rev_walk([head_id.detach()])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()
        .map_err(|e| GitFastPathError::Object {
            id: "HEAD".to_string(),
            source: e,
        })?;

    for info in walk.take(max) {
        let info = info.map_err(|e| GitFastPathError::Object {
            id: "HEAD".to_string(),
            source: e,
        })?;
        let commit = info.object().map_err(|e| GitFastPathError::Object {
            id: info.id.to_string(),
            source: e,
        })?;
        let short_id = commit.short_id().map_err(|e| GitFastPathError::Object {
            id: info.id.to_string(),
            source: e,
        })?;
        let author = commit.author().map_err(|e| GitFastPathError::Object {
            id: info.id.to_string(),
            source: e,
        })?;
        let time = commit.time().map_err(|e| GitFastPathError::Object {
            id: info.id.to_string(),
            source: e,
        })?;
        let message = commit
            .message_raw()
            .map_err(|e| GitFastPathError::Object {
                id: info.id.to_string(),
                source: e,
            })?
            .to_string();
        out.push_str(&format!(
            "commit {short_id}\nAuthor: {} <{}>\nDate:   {}\n\n{message}\n",
            author.name,
            author.email,
            time.format(gix::date::time::Format::Custom(
                gix::date::time::format::RFC2822
            ))
            .unwrap_or_else(|_| time
                .format(gix::date::time::Format::Unix)
                .unwrap_or_default())
        ));
    }
    Ok(out)
}

fn run_show(
    repo: &gix::Repository,
    rev: &str,
    path: Option<&str>,
    stat: bool,
) -> Result<String, GitFastPathError> {
    let id = repo
        .rev_parse_single(rev)
        .map_err(|e| GitFastPathError::RevParse {
            spec: rev.to_string(),
            source: e,
        })?;

    if let Some(path) = path {
        let commit = id
            .object()
            .map_err(|e| GitFastPathError::Object {
                id: rev.to_string(),
                source: e,
            })?
            .into_commit();
        let tree = commit.tree().map_err(|e| GitFastPathError::Object {
            id: rev.to_string(),
            source: e,
        })?;
        let entry = tree
            .lookup_entry_by_path(path)
            .map_err(|e| GitFastPathError::Object {
                id: path.to_string(),
                source: e,
            })?
            .ok_or_else(|| GitFastPathError::Object {
                id: format!("{rev}:{path}"),
                source: gix::Error::from_error(gix::error::message!("path not found in tree")),
            })?;
        let blob = repo
            .find_blob(entry.object_id())
            .map_err(|e| GitFastPathError::Object {
                id: format!("{rev}:{path}"),
                source: e,
            })?;
        return Ok(String::from_utf8_lossy(&blob.data).to_string());
    }

    let object = id.object().map_err(|e| GitFastPathError::Object {
        id: rev.to_string(),
        source: e,
    })?;
    match object.kind {
        gix::objs::Kind::Commit => {
            let commit = object.into_commit();
            let mut out = run_log(repo, Some(1))?;
            if stat {
                let head_tree_id = commit.tree_id().map_err(|e| GitFastPathError::Object {
                    id: rev.to_string(),
                    source: e,
                })?;
                let tree = repo
                    .find_tree(head_tree_id)
                    .map_err(|e| GitFastPathError::Object {
                        id: rev.to_string(),
                        source: e,
                    })?;
                out.push_str(&format!("\n {} | 0\n", tree.iter().count()));
                out.push_str(&format!(" {} files changed\n", tree.iter().count()));
            }
            Ok(out)
        }
        gix::objs::Kind::Tree => {
            let tree = object.into_tree();
            let mut out = String::new();
            for entry in tree.iter().flatten() {
                out.push_str(&format!(
                    "{}\t{}\n",
                    entry.mode().as_str(),
                    entry.filename()
                ));
            }
            Ok(out)
        }
        gix::objs::Kind::Blob => {
            let blob = object.into_blob();
            Ok(String::from_utf8_lossy(&blob.data).to_string())
        }
        _ => Err(GitFastPathError::NotATreeOrBlob {
            spec: rev.to_string(),
        }),
    }
}

fn run_ls_files(repo: &gix::Repository) -> Result<String, GitFastPathError> {
    let index = repo.index_or_empty().map_err(GitFastPathError::Index)?;
    let backing = index.path_backing();
    let mut out = String::new();
    for entry in index.entries() {
        let path = String::from_utf8_lossy(entry.path_in(backing));
        out.push_str(&path);
        out.push('\n');
    }
    Ok(out)
}

fn run_branch(
    repo: &gix::Repository,
    all: bool,
    remotes: bool,
    verbose: u8,
) -> Result<String, GitFastPathError> {
    let head = repo.head().map_err(|e| GitFastPathError::Object {
        id: "HEAD".to_string(),
        source: e,
    })?;
    let head_name = head
        .referent_name()
        .map(|n| n.shorten().to_string())
        .unwrap_or_default();

    let refs = repo.references().map_err(|e| GitFastPathError::Object {
        id: "refs".to_string(),
        source: e,
    })?;

    let mut lines: Vec<String> = Vec::new();

    if !remotes {
        let local = refs
            .local_branches()
            .map_err(|e| GitFastPathError::Object {
                id: "refs/heads".to_string(),
                source: e,
            })?;
        for r in local {
            let r = r.map_err(|e| GitFastPathError::Object {
                id: "refs/heads".to_string(),
                source: e,
            })?;
            let name = r.name().shorten().to_string();
            let current = name == head_name;
            lines.push(format_branch(current, &name, &r, verbose));
        }
    }

    if all || remotes {
        let remote = refs
            .remote_branches()
            .map_err(|e| GitFastPathError::Object {
                id: "refs/remotes".to_string(),
                source: e,
            })?;
        for r in remote {
            let r = r.map_err(|e| GitFastPathError::Object {
                id: "refs/remotes".to_string(),
                source: e,
            })?;
            let name = r.name().shorten().to_string();
            lines.push(format_branch(false, &name, &r, verbose));
        }
    }

    Ok(lines.join("\n") + "\n")
}

fn format_branch(current: bool, name: &str, r: &gix::Reference<'_>, verbose: u8) -> String {
    let prefix = if current { "* " } else { "  " };
    if verbose == 0 {
        format!("{prefix}{name}")
    } else {
        let id = r.target().id().to_string();
        let short = id.get(..7).unwrap_or(&id);
        if verbose >= 2 {
            format!("{prefix}{name} {short} {}", r.name())
        } else {
            format!("{prefix}{name} {short}")
        }
    }
}

fn run_rev_parse_head(repo: &gix::Repository) -> Result<String, GitFastPathError> {
    let id = repo.head_id().map_err(|e| GitFastPathError::Object {
        id: "HEAD".to_string(),
        source: e,
    })?;
    Ok(format!("{}\n", id))
}

fn run_tag(
    repo: &gix::Repository,
    list: bool,
    lines: Option<usize>,
) -> Result<String, GitFastPathError> {
    let refs = repo.references().map_err(|e| GitFastPathError::Object {
        id: "refs".to_string(),
        source: e,
    })?;
    let mut out = String::new();
    for r in refs.tags().map_err(|e| GitFastPathError::Object {
        id: "refs/tags".to_string(),
        source: e,
    })? {
        let r = r.map_err(|e| GitFastPathError::Object {
            id: "refs/tags".to_string(),
            source: e,
        })?;
        let name = r.name().shorten().to_string();
        if list && lines.is_some() {
            let annotation = tag_annotation(repo, &r).unwrap_or_default();
            let limit = lines.unwrap_or(0);
            let limited: Vec<&str> = annotation.lines().take(limit).collect();
            out.push_str(&format!("{}\t{}\n", name, limited.join("\n")));
        } else {
            out.push_str(&name);
            out.push('\n');
        }
    }
    Ok(out)
}

fn tag_annotation(repo: &gix::Repository, r: &gix::Reference<'_>) -> Option<String> {
    let target = r.target();
    let id = target.id();
    let object = repo.find_object(id).ok()?;
    if object.kind == gix::objs::Kind::Tag {
        let tag = object.into_tag();
        Some(tag.decode().ok()?.message.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) -> gix::Repository {
        gix::init(dir).expect("init repo")
    }

    fn commit(repo: &gix::Repository, message: &str) {
        let signature = gix::actor::SignatureRef {
            name: "Test".into(),
            email: "test@example.com".into(),
            time: "0 +0000",
        };
        let tree = repo.empty_tree();
        let parents: Vec<gix::hash::ObjectId> = match repo.head_id() {
            Ok(id) => vec![id.detach()],
            Err(_) => vec![],
        };
        let _ = repo
            .commit_as(signature, signature, "HEAD", message, tree.id, parents)
            .expect("commit");
    }

    #[test]
    fn rev_parse_head_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        commit(&repo, "first\n");
        let out = run_rev_parse_head(&repo).unwrap();
        assert_eq!(out.trim().len(), 40);
    }

    #[test]
    fn ls_files_lists_index() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "hello\n").unwrap();
        let id = repo.write_blob(std::fs::read(&path).unwrap()).unwrap();
        let mut index = gix::index::File::at_or_default(
            repo.index_path(),
            repo.object_hash(),
            false,
            Default::default(),
        )
        .unwrap();
        index.dangerously_push_entry(
            Default::default(),
            id.detach(),
            gix::index::entry::Flags::empty(),
            gix::index::entry::Mode::FILE,
            gix::bstr::BStr::new("a.txt".as_bytes()),
        );
        index.sort_entries();
        index.write(Default::default()).unwrap();

        let out = run_ls_files(&repo).unwrap();
        assert!(out.contains("a.txt"));
    }
}
