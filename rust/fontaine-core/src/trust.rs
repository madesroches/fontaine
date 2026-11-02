//! Workspace trust. The record lives in `<data dir>/trust.toml`, never in the repository, and
//! git state is read with `gix` only: Fontaine never spawns `git`, so repository-controlled
//! configuration (fsmonitor, filter drivers, hooks) cannot execute anything here.

use std::io;
use std::path::{Component, Path, PathBuf};

use gix::bstr::{BStr, BString};
use serde::Deserialize;

use crate::config::ConfigError;
use crate::owner_only;

pub const TRUST_FILE: &str = "trust.toml";

const LOCAL_SETTINGS: &str = ".fontaine/settings.local.toml";

pub fn trust_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TRUST_FILE)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    #[serde(default)]
    trusted: Vec<Trusted>,
    /// Repository-defined MCP servers the user approved to start.
    #[serde(default)]
    approved_servers: Vec<ApprovedServer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedServer {
    root: String,
    name: String,
    /// Of the server's definition, so a changed command or URL is asked about again.
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trusted {
    root: String,
}

/// Schema check for `trust.toml` text, used by the docs check.
pub fn validate_trust_toml(text: &str) -> Result<(), String> {
    toml::from_str::<TrustFile>(text)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// A TOML literal string (`'…'`) when possible, so Windows paths need no escaping.
pub fn toml_literal(value: &str) -> String {
    if value.contains('\'') || value.chars().any(char::is_control) {
        toml::Value::String(value.to_string()).to_string()
    } else {
        format!("'{value}'")
    }
}

/// The error is one line: toml's own rendering quotes the source line.
fn load(data_dir: &Path) -> Result<TrustFile, String> {
    let text = match crate::file_lock::read_shared(&trust_path(data_dir)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(TrustFile::default()),
        Err(e) => return Err(fontaine_tools::error_chain::chain(&e)),
    };
    toml::from_str(&text).map_err(|e: toml::de::Error| {
        crate::config::bare_toml_message(&text, e.span(), e.message())
    })
}

/// `None` when the file is unreadable or unparseable; a missing file is an empty record.
fn read_file(data_dir: &Path) -> Option<TrustFile> {
    load(data_dir).ok()
}

/// A one-line report when `trust.toml` exists but cannot be read or parsed, which leaves every
/// workspace untrusted and every MCP server unapproved. `None` for a missing or valid file.
pub fn unreadable(data_dir: &Path) -> Option<String> {
    let cause = load(data_dir).err()?;
    Some(format!(
        "{} cannot be used ({cause}): no workspace is trusted and no MCP server approved until it is fixed or removed",
        trust_path(data_dir).display(),
    ))
}

fn read_entries(data_dir: &Path) -> Option<Vec<String>> {
    Some(
        read_file(data_dir)?
            .trusted
            .into_iter()
            .map(|t| t.root)
            .collect(),
    )
}

fn canonical_root(root: &Path) -> Option<String> {
    Some(
        std::fs::canonicalize(root)
            .ok()?
            .to_string_lossy()
            .into_owned(),
    )
}

fn lock(path: &Path) -> Result<crate::file_lock::FileLock, ConfigError> {
    crate::file_lock::lock(path).map_err(|source| ConfigError::Write {
        path: path.with_extension("lock"),
        source,
    })
}

/// Whether the user approved the repository-defined MCP server `name` with this definition
/// for exactly this root. The repository cannot approve its own servers: the record is in the
/// data directory.
pub fn server_approved(data_dir: &Path, root: &Path, name: &str, digest: &str) -> bool {
    let (Some(file), Some(root)) = (read_file(data_dir), canonical_root(root)) else {
        return false;
    };
    file.approved_servers
        .iter()
        .any(|a| a.root == root && a.name == name && a.digest == digest)
}

/// Records the user's approval of an MCP server definition.
pub fn approve_server(
    data_dir: &Path,
    root: &Path,
    name: &str,
    digest: &str,
) -> Result<(), ConfigError> {
    let path = trust_path(data_dir);
    let read_err = |source| ConfigError::Read {
        path: path.clone(),
        source,
    };
    let write_err = |source| ConfigError::Write {
        path: path.clone(),
        source,
    };
    let root = canonical_root(root).ok_or_else(|| ConfigError::Read {
        path: root.to_path_buf(),
        source: io::Error::other("the root cannot be resolved"),
    })?;
    let _lock = lock(&path)?;
    let mut text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(read_err(e)),
    };
    let parsed: TrustFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        message: e.to_string(),
    })?;
    if parsed
        .approved_servers
        .iter()
        .any(|a| a.root == root && a.name == name && a.digest == digest)
    {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "[[approved_servers]]\nroot = {}\nname = {}\ndigest = {}\n",
        toml_literal(&root),
        toml_literal(name),
        toml_literal(digest)
    ));
    owner_only::write_atomic(&path, text.as_bytes()).map_err(write_err)
}

/// Records `root` (canonicalized) as trusted. Nothing but "trust" is persisted.
pub fn record(data_dir: &Path, root: &Path) -> Result<(), ConfigError> {
    let path = trust_path(data_dir);
    let read_err = |source| ConfigError::Read {
        path: path.clone(),
        source,
    };
    let write_err = |source| ConfigError::Write {
        path: path.clone(),
        source,
    };
    let root = std::fs::canonicalize(root).map_err(|source| ConfigError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let root = root.to_string_lossy().into_owned();

    let _lock = lock(&path)?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(read_err(e)),
    };
    let parsed: TrustFile = toml::from_str(&existing).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        message: e.to_string(),
    })?;
    if parsed.trusted.iter().any(|t| t.root == root) {
        return Ok(());
    }

    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("[[trusted]]\nroot = {}\n", toml_literal(&root)));

    owner_only::write_atomic(&path, text.as_bytes()).map_err(write_err)
}

enum Boundary {
    /// Inside a git work tree whose root is this canonical path.
    WorkTree(PathBuf),
    /// Not inside any repository.
    None,
    /// Discovery failed some other way, or found a repository without a work tree.
    Unknown,
}

fn open_options() -> gix::open::Options {
    // Isolated: only the repository's own config is read. Strict: an unparseable config is an
    // error, which callers treat as "cannot vouch for this repository".
    gix::open::Options::isolated().strict_config(true)
}

fn discover(dir: &Path) -> Result<(gix::Repository, PathBuf), Boundary> {
    match gix::discover_opts(
        dir,
        gix::discover::upwards::Options::default(),
        open_options(),
    ) {
        Ok(repo) => match repo.workdir().map(std::fs::canonicalize) {
            Some(Ok(workdir)) => Ok((repo, workdir)),
            _ => Err(Boundary::Unknown),
        },
        Err(err) => {
            use gix::discover::upwards::Error as Up;
            match err.downcast_any_ref::<Up>() {
                Some(
                    Up::NoGitRepository { .. }
                    | Up::NoGitRepositoryWithinCeiling { .. }
                    | Up::NoGitRepositoryWithinFs { .. },
                ) => Err(Boundary::None),
                _ => Err(Boundary::Unknown),
            }
        }
    }
}

fn boundary(dir: &Path) -> Boundary {
    match discover(dir) {
        Ok((_, workdir)) => Boundary::WorkTree(workdir),
        Err(b) => b,
    }
}

/// The work-tree root of the git repository that holds `dir`, when there is one.
pub(crate) fn work_tree_root(dir: &Path) -> Option<PathBuf> {
    match boundary(dir) {
        Boundary::WorkTree(top) => Some(top),
        _ => None,
    }
}

/// Like [`work_tree_root`], but a linked worktree resolves to the checkout that owns the
/// repository, so every worktree of one repository names the same directory.
pub(crate) fn main_work_tree_root(dir: &Path) -> Option<PathBuf> {
    let (repo, workdir) = discover(dir).ok()?;
    // A linked worktree's common dir is spelled `<repo>/.git/worktrees/<name>/../..`.
    let common = std::fs::canonicalize(repo.common_dir()).ok();
    match common.as_deref() {
        Some(common) if common.file_name().is_some_and(|name| name == ".git") => {
            common.parent().map(Path::to_path_buf).or(Some(workdir))
        }
        _ => Some(workdir),
    }
}

fn norm(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| match c {
            Component::Normal(s) => s.to_string_lossy().into_owned(),
            other => other.as_os_str().to_string_lossy().into_owned(),
        })
        .map(|s| if cfg!(windows) { s.to_lowercase() } else { s })
        .collect()
}

/// Component-wise `starts_with`, case-insensitive on Windows.
fn is_within(path: &Path, base: &Path) -> bool {
    let (path, base) = (norm(path), norm(base));
    path.len() >= base.len() && path[..base.len()] == base[..]
}

/// True when `root` or a recorded ancestor is trusted. The ancestor walk stops at the
/// enclosing git work-tree root (so a trusted `$HOME` does not trust every repo inside it) and
/// is unbounded only outside any repository. Every uncertainty answers false.
pub fn is_trusted(data_dir: &Path, root: &Path) -> bool {
    let Some(entries) = read_entries(data_dir) else {
        return false;
    };
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    let bound = match boundary(&root) {
        Boundary::WorkTree(top) => Some(top),
        Boundary::None => None,
        Boundary::Unknown => return false,
    };
    entries
        .iter()
        .filter_map(|entry| std::fs::canonicalize(entry).ok())
        .any(|entry| {
            is_within(&root, &entry) && bound.as_ref().is_none_or(|top| is_within(&entry, top))
        })
}

/// Why a project-local file loads as project scope.
#[derive(Debug, PartialEq, Eq)]
pub enum ProjectDemotion {
    /// No git work tree encloses the workspace.
    NoWorkTree,
    /// `gix` could not open the repository or read its index.
    GitError,
    /// The file resolves outside the work tree (a symlinked `.fontaine`).
    OutsideWorkTree,
    /// The file is in the index.
    Tracked,
    /// Untracked, but the workspace root is not trusted.
    UntrustedRoot(PathBuf),
    /// `trust.toml` exists but cannot be read or parsed; nothing is trusted until it is fixed.
    UnreadableTrustStore,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LocalClassification {
    UserTrust,
    Project(ProjectDemotion),
}

/// Whether git's index lists a file, as an edit without a guardian verdict needs to know it.
#[derive(Debug, PartialEq, Eq)]
pub enum Tracking {
    /// The index has an exact entry for the path.
    Tracked,
    /// The path is inside the work tree but the index has no entry for it.
    Untracked,
    /// No work tree encloses `root`, the path lies outside it, or git could not be read.
    Unknown,
}

/// Looks `resolved` up in the index of the work tree that encloses `root`. Unlike
/// `classify_project_local`, which treats an ancestor entry (submodule, sparse directory) and a
/// case variant as tracked because trust errs toward project scope, only an exact,
/// case-sensitive entry for the resolved path counts. A path that does not exist yet is looked
/// up through its canonical parent.
pub fn tracking(root: &Path, resolved: &Path) -> Tracking {
    let Ok((repo, workdir)) = discover(root) else {
        return Tracking::Unknown;
    };
    let Some(relative) = index_relative(&workdir, resolved) else {
        return Tracking::Unknown;
    };
    let Ok(index) = repo.index_or_empty() else {
        return Tracking::Unknown;
    };
    if !relative.is_empty() && index.entry_by_path(relative.as_ref()).is_some() {
        Tracking::Tracked
    } else {
        Tracking::Untracked
    }
}

/// Canonical `resolved` as an index key under `workdir`; a path that does not exist yet goes
/// through its canonical parent. `None` when it cannot be resolved or lies outside the work tree.
fn index_relative(workdir: &Path, resolved: &Path) -> Option<BString> {
    let canonical = match std::fs::canonicalize(resolved) {
        Ok(path) => path,
        Err(_) => {
            let (parent, name) = (resolved.parent()?, resolved.file_name()?);
            std::fs::canonicalize(parent).ok()?.join(name)
        }
    };
    let relative = gix::path::into_bstr(canonical.strip_prefix(workdir).ok()?).ok()?;
    Some(gix::path::to_unix_separators(relative).into_owned())
}

/// For each of `paths`, whether git's index has an exact file entry for it, in the work tree
/// that encloses `root`. A directory and the work-tree root are never tracked here: writing or
/// extracting into one can overwrite files git does not hold. Opens the repository and its index
/// once for all of them. `false` wherever [`tracking`] would say `Unknown`.
pub fn tracked_files(root: &Path, paths: &[PathBuf]) -> Vec<bool> {
    if paths.is_empty() {
        return Vec::new();
    }
    let none = || vec![false; paths.len()];
    let Ok((repo, workdir)) = discover(root) else {
        return none();
    };
    let Ok(index) = repo.index_or_empty() else {
        return none();
    };
    paths
        .iter()
        .map(|resolved| {
            index_relative(&workdir, resolved).is_some_and(|relative| {
                // `entry_by_path` ignores the mode: a submodule gitlink holds only a commit
                // pointer, and an intent-to-add entry has no content in git yet.
                !relative.is_empty()
                    && index.entry_by_path(relative.as_ref()).is_some_and(|entry| {
                        matches!(
                            entry.mode,
                            gix::index::entry::Mode::FILE
                                | gix::index::entry::Mode::FILE_EXECUTABLE
                                | gix::index::entry::Mode::SYMLINK
                        ) && !entry
                            .flags
                            .contains(gix::index::entry::Flags::INTENT_TO_ADD)
                    })
            })
        })
        .collect()
}

/// User trust is earned only by a file git does not know about (untracked or ignored — both
/// mean absent from the index) in a trusted root. Everything else is project scope.
pub fn classify_project_local(
    workspace: &Path,
    file: &Path,
    data_dir: &Path,
) -> LocalClassification {
    use LocalClassification::Project;
    use ProjectDemotion::*;

    let (repo, workdir) = match discover(workspace) {
        Ok(found) => found,
        Err(Boundary::None) => return Project(NoWorkTree),
        Err(_) => return Project(GitError),
    };
    let Ok(canonical) = std::fs::canonicalize(file) else {
        return Project(OutsideWorkTree);
    };
    let Ok(relative) = canonical.strip_prefix(&workdir) else {
        return Project(OutsideWorkTree);
    };
    let Ok(relative) = gix::path::into_bstr(relative) else {
        return Project(GitError);
    };
    let resolved = gix::path::to_unix_separators(relative).into_owned();

    // The path as git sees it before symlinks are followed: a tracked `.fontaine` or
    // `settings.local.toml` symlink must count even when its target is an ignored file.
    let literal = std::fs::canonicalize(workspace)
        .ok()
        .and_then(|ws| {
            let rel = ws.strip_prefix(&workdir).ok()?.join(LOCAL_SETTINGS);
            Some(gix::path::to_unix_separators(gix::path::into_bstr(rel).ok()?).into_owned())
        })
        .unwrap_or_default();

    let Ok(index) = repo.index_or_empty() else {
        return Project(GitError);
    };
    let lookup = index.prepare_icase_backing();
    // A submodule or sparse directory is one index entry for its whole subtree, so an ancestor
    // entry covers files the index does not list individually; sparse directories are stored
    // with a trailing `/`.
    let in_index = |path: &[u8]| {
        index
            .entry_by_path_icase(BStr::new(path), true, &lookup)
            .is_some()
    };
    let tracked = |bytes: &[u8]| {
        !bytes.is_empty()
            && (in_index(bytes)
                || bytes
                    .iter()
                    .enumerate()
                    .any(|(i, &b)| b == b'/' && (in_index(&bytes[..i]) || in_index(&bytes[..=i]))))
    };
    if tracked(resolved.as_ref()) || tracked(literal.as_ref()) {
        return Project(Tracked);
    }
    if read_entries(data_dir).is_none() {
        return Project(UnreadableTrustStore);
    }
    if !is_trusted(data_dir, workspace) {
        return Project(UntrustedRoot(workspace.to_path_buf()));
    }
    LocalClassification::UserTrust
}
