//! Workspace trust. The record lives in `<data dir>/trust.toml`, never in the repository, and
//! git state is read with `gix` only: Fontaine never spawns `git`, so repository-controlled
//! configuration (fsmonitor, filter drivers, hooks) cannot execute anything here.

use std::io;
use std::path::{Component, Path, PathBuf};

use gix::bstr::BStr;
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

/// `None` when the file is unreadable or unparseable; a missing file is an empty record.
fn read_file(data_dir: &Path) -> Option<TrustFile> {
    let text = match std::fs::read_to_string(trust_path(data_dir)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(TrustFile::default()),
        Err(_) => return None,
    };
    toml::from_str(&text).ok()
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
    let io_err = |source| ConfigError::Io {
        path: path.clone(),
        source,
    };
    let root = canonical_root(root).ok_or_else(|| ConfigError::Io {
        path: root.to_path_buf(),
        source: io::Error::other("the root cannot be resolved"),
    })?;
    if server_approved(data_dir, Path::new(&root), name, digest) {
        return Ok(());
    }
    let mut text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err(e)),
    };
    toml::from_str::<TrustFile>(&text).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        message: e.to_string(),
    })?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "[[approved_servers]]\nroot = {}\nname = {}\ndigest = {}\n",
        toml_literal(&root),
        toml_literal(name),
        toml_literal(digest)
    ));
    std::fs::create_dir_all(data_dir).map_err(io_err)?;
    owner_only::write(&path, text.as_bytes()).map_err(io_err)
}

/// Records `root` (canonicalized) as trusted. Nothing but "trust" is persisted.
pub fn record(data_dir: &Path, root: &Path) -> Result<(), ConfigError> {
    let path = trust_path(data_dir);
    let io_err = |source| ConfigError::Io {
        path: path.clone(),
        source,
    };
    let root = std::fs::canonicalize(root).map_err(|source| ConfigError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let root = root.to_string_lossy().into_owned();

    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err(e)),
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

    std::fs::create_dir_all(data_dir).map_err(io_err)?;
    owner_only::write(&path, text.as_bytes()).map_err(io_err)
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
