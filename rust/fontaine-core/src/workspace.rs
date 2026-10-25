//! The workspace roots of a session: the primary root plus every directory the user added.

use std::path::{Path, PathBuf};

pub struct Roots {
    /// Canonical, primary first, without duplicates.
    pub roots: Vec<PathBuf>,
    /// One line per entry that was left out and why.
    pub warnings: Vec<String>,
}

/// Where an added directory was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSource {
    Flag,
    Settings,
    /// `/add-dir` in the TUI.
    Command,
}

impl RootSource {
    fn label(self) -> &'static str {
        match self {
            RootSource::Flag => "--add-dir",
            RootSource::Settings => "[permissions] additional_directories in the settings",
            RootSource::Command => "/add-dir",
        }
    }
}

/// Resolves the added entries against `launch_dir`. An entry that does not exist, is not a
/// directory or cannot be canonicalized is left out with a warning: a bad entry never widens
/// scope and never stops the run.
pub fn resolve(
    primary: &Path,
    launch_dir: &Path,
    added: impl IntoIterator<Item = (RootSource, PathBuf)>,
) -> Roots {
    let mut roots = vec![primary.to_path_buf()];
    let mut warnings = Vec::new();
    for (source, entry) in added {
        let skip = |why: &str| {
            format!(
                "ignoring {} entry `{}`: {why}",
                source.label(),
                entry.display()
            )
        };
        let path = launch_dir.join(&entry);
        match std::fs::canonicalize(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warnings.push(skip("it does not exist"));
            }
            Err(e) => warnings.push(skip(&format!(
                "it cannot be resolved ({})",
                fontaine_tools::error_chain::chain(&e)
            ))),
            Ok(canonical) if !canonical.is_dir() => warnings.push(skip("it is not a directory")),
            Ok(canonical) => {
                if !roots.contains(&canonical) {
                    roots.push(canonical);
                }
            }
        }
    }
    Roots { roots, warnings }
}
