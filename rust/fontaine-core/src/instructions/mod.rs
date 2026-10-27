//! The instruction files that shape a session: `FONTAINE.md`, `CLAUDE.md` and `AGENTS.md` from
//! the user, from every directory above the primary root, and from the files they import.
//! `load` reads the set that applies at launch; [`on_demand::InstructionStage`] brings in
//! nested files and path-scoped rules as the agent reads and writes files.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use fontaine_tools::scope::Scope;

use crate::gate::SessionState;
use crate::permission_rules::Rule;

mod imports;
mod markdown;
pub mod on_demand;
mod rules;

pub use on_demand::InstructionStage;
pub use rules::PathRule;

/// Total launch text past which the build warns that the files are large.
const LARGE_TOTAL_CHARS: usize = 40_000;

/// Where an instruction file came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The user's own files: `~/.claude`, the config directory and the rules in both.
    User,
    /// `CLAUDE.md`, `FONTAINE.md` or their `.claude`/`.fontaine` twins in a directory.
    Walk,
    /// `CLAUDE.local.md` or `FONTAINE.local.md`.
    Local,
    /// `AGENTS.md`, read only when no `CLAUDE`/`FONTAINE` file exists.
    Agents,
    /// A file under a project `rules/` directory.
    Rule,
    /// A file named by an `@path` in `from`.
    Import { from: PathBuf },
}

impl Origin {
    pub fn label(&self) -> String {
        match self {
            Origin::User => "user".to_string(),
            Origin::Walk => "project".to_string(),
            Origin::Local => "local".to_string(),
            Origin::Agents => "AGENTS.md".to_string(),
            Origin::Rule => "rule".to_string(),
            Origin::Import { from } => format!("imported by {}", from.display()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstructionFile {
    pub path: PathBuf,
    pub text: String,
    pub origin: Origin,
}

/// Who controls a file, which decides how far it and its imports may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    /// The user's own configuration: unconfined.
    User,
    /// Inside the workspace roots or the enclosing git work tree: it must resolve inside them.
    Repo,
    /// A directory above the work tree, such as `~`: it loads as found, its imports are
    /// confined.
    Above,
}

/// What the loader needs to know about the session.
#[derive(Clone)]
pub struct Sources {
    pub config_dir: PathBuf,
    pub home: Option<PathBuf>,
    pub primary_root: PathBuf,
    pub roots: Vec<PathBuf>,
    /// The work-tree root of the git repository that holds the primary root.
    pub work_tree: Option<PathBuf>,
    /// The session's `deny` rules; one that denies `read` of a file keeps it out of the prompt.
    pub deny: Vec<Rule>,
    pub scope: Arc<Scope>,
    /// The workspace roots and the work tree, canonical.
    pub(crate) confinement: Vec<PathBuf>,
}

impl Sources {
    pub fn new(
        config_dir: PathBuf,
        home: Option<PathBuf>,
        scope: Arc<Scope>,
        deny: Vec<Rule>,
    ) -> Sources {
        let primary_root = scope.primary().to_path_buf();
        let work_tree = crate::trust::work_tree_root(&primary_root);
        let canonical =
            |path: &PathBuf| std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        let confinement = scope
            .roots()
            .iter()
            .chain(&work_tree)
            .map(canonical)
            .collect();
        Sources {
            config_dir,
            home,
            primary_root,
            roots: scope.roots().to_vec(),
            work_tree,
            deny,
            scope,
            confinement,
        }
    }

    /// The sources of a running session, with its rules as they stand now.
    pub fn for_state(state: &SessionState, config_dir: &Path, home: Option<&Path>) -> Sources {
        Sources::new(
            config_dir.to_path_buf(),
            home.map(Path::to_path_buf),
            state.scope(),
            state.rules().deny.into_iter().map(|r| r.rule).collect(),
        )
    }
}

/// The canonical paths and the digests of the effective text already loaded.
#[derive(Clone, Debug, Default)]
pub(crate) struct Seen {
    pub(crate) paths: HashSet<PathBuf>,
    pub(crate) digests: HashSet<[u8; 32]>,
}

impl Seen {
    pub(crate) fn union(&mut self, other: &Seen) {
        self.paths.extend(other.paths.iter().cloned());
        self.digests.extend(other.digests.iter().copied());
    }
}

/// The launch set.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    /// In load order: the user tier, then the directory walk from the filesystem root down, each
    /// file followed by its imports.
    pub files: Vec<InstructionFile>,
    /// Rules scoped by `paths`, which apply once a matching file is read or written.
    pub path_rules: Vec<PathRule>,
    /// Whether `AGENTS.md` was read because no `CLAUDE.md` or `FONTAINE.md` exists.
    pub agents_mode: bool,
    pub warnings: Vec<String>,
    pub(crate) seen: Seen,
}

impl Loaded {
    /// A warning when the files are large, naming the largest.
    fn size_warning(&self) -> Option<String> {
        let total: usize = self.files.iter().map(|f| f.text.chars().count()).sum();
        if total <= LARGE_TOTAL_CHARS {
            return None;
        }
        let mut sizes: Vec<(usize, &Path)> = self
            .files
            .iter()
            .map(|f| (f.text.chars().count(), f.path.as_path()))
            .collect();
        sizes.sort_by_key(|(size, _)| Reverse(*size));
        let largest = sizes
            .iter()
            .take(3)
            .map(|(size, path)| format!("{} ({size} characters)", path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "the instruction files add up to {total} characters, which every request carries; the largest are {largest}"
        ))
    }
}

const COUNTING_NAMES: [&str; 6] = [
    "CLAUDE.md",
    ".claude/CLAUDE.md",
    "CLAUDE.local.md",
    "FONTAINE.md",
    ".fontaine/FONTAINE.md",
    "FONTAINE.local.md",
];

/// Whether `dir` holds a file that keeps `AGENTS.md` from being read. The user's own
/// `~/.claude/CLAUDE.md` is not a project file, so it does not count.
pub(crate) fn has_counting_file(dir: &Path, home: Option<&Path>) -> bool {
    COUNTING_NAMES.iter().any(|name| {
        let user_file = home == Some(dir) && *name == ".claude/CLAUDE.md";
        !user_file && dir.join(name).is_file()
    })
}

/// The instruction files that apply at launch.
pub fn load(sources: &Sources) -> Loaded {
    let mut loader = imports::Loader::new(sources, Seen::default());
    loader.user_tier();
    let mut dirs: Vec<&Path> = sources.primary_root.ancestors().collect();
    dirs.reverse();
    let home = sources.home.as_deref();
    let agents_mode = !dirs.iter().any(|dir| has_counting_file(dir, home));
    for dir in dirs {
        loader.walk_dir(dir, agents_mode);
    }
    let (files, mut warnings, path_rules, seen) = loader.finish();
    let mut loaded = Loaded {
        files,
        path_rules,
        agents_mode,
        warnings: Vec::new(),
        seen,
    };
    warnings.extend(loaded.size_warning());
    loaded.warnings = warnings;
    loaded
}

/// The directory a file's level in the walk is named for: the directory that holds the file, or
/// that holds the `.claude`/`.fontaine` directory it sits in.
fn level_dir(file: &InstructionFile) -> Option<&Path> {
    let is_config = |dir: &Path| {
        dir.file_name()
            .is_some_and(|name| name == ".claude" || name == ".fontaine")
    };
    if file.origin == Origin::Rule {
        return file.path.ancestors().skip(1).find_map(|dir| {
            let parent = dir.parent()?;
            if dir.file_name()? == "rules" && is_config(parent) {
                parent.parent()
            } else {
                None
            }
        });
    }
    let parent = file.path.parent()?;
    if is_config(parent) {
        parent.parent()
    } else {
        Some(parent)
    }
}

/// The files as the guardian reads them: the primary root's level first, then each ancestor
/// toward the filesystem root, then the user tier, with imports kept after their importer. A
/// cap on the text then drops the broadest files, not the repository's own.
pub fn guardian_order(files: &[InstructionFile]) -> Vec<InstructionFile> {
    let mut units: Vec<(i64, Vec<&InstructionFile>)> = Vec::new();
    for file in files {
        if let (Origin::Import { .. }, Some((_, unit))) = (&file.origin, units.last_mut()) {
            unit.push(file);
            continue;
        }
        let depth = match file.origin {
            Origin::User => -1,
            _ => level_dir(file).map_or(0, |dir| dir.components().count() as i64),
        };
        units.push((depth, vec![file]));
    }
    units.sort_by_key(|(depth, _)| Reverse(*depth));
    units
        .into_iter()
        .flat_map(|(_, unit)| unit.into_iter().cloned())
        .collect()
}

/// The file `/init` writes or improves: the first existing project instruction file in the
/// primary root, otherwise a new `FONTAINE.md`. The flag says whether it exists.
pub fn init_target(root: &Path) -> (PathBuf, bool) {
    for name in ["FONTAINE.md", "CLAUDE.md", ".claude/CLAUDE.md", "AGENTS.md"] {
        let path = root.join(name);
        if path.is_file() {
            return (path, true);
        }
    }
    (root.join("FONTAINE.md"), false)
}

/// One line of the `/memory` list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntry {
    pub path: PathBuf,
    pub kind: MemoryKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// In the system prompt.
    Launch(Origin),
    /// Brought in by a read or write.
    OnDemand,
    /// A default file that does not exist yet; opening it creates it.
    NotCreated,
    /// A default file that exists but contributes nothing now (blank, or a repeat).
    Present,
    /// A file in the auto-memory store.
    Auto { modified: Option<SystemTime> },
}

/// What `/memory` lists: the launch files, the files loaded on demand, and the user and project
/// `FONTAINE.md`, which can be created from there, then the auto-memory store's files.
pub fn memory_entries(
    loaded: &Loaded,
    on_demand: &[PathBuf],
    config_dir: &Path,
    primary_root: &Path,
    memory_dir: Option<&Path>,
) -> Vec<MemoryEntry> {
    let mut entries: Vec<MemoryEntry> = loaded
        .files
        .iter()
        .map(|file| MemoryEntry {
            path: file.path.clone(),
            kind: MemoryKind::Launch(file.origin.clone()),
        })
        .collect();
    entries.extend(on_demand.iter().map(|path| MemoryEntry {
        path: path.clone(),
        kind: MemoryKind::OnDemand,
    }));
    for path in [
        config_dir.join("FONTAINE.md"),
        primary_root.join("FONTAINE.md"),
    ] {
        if entries.iter().any(|entry| entry.path == path) {
            continue;
        }
        let kind = if path.is_file() {
            MemoryKind::Present
        } else {
            MemoryKind::NotCreated
        };
        entries.push(MemoryEntry { path, kind });
    }
    if let Some(dir) = memory_dir {
        entries.extend(
            crate::memory::files(dir)
                .into_iter()
                .map(|file| MemoryEntry {
                    path: file.path,
                    kind: MemoryKind::Auto {
                        modified: file.modified,
                    },
                }),
        );
    }
    entries
}
