//! What a run writes as it goes: the check that `--out` can be written, and the JSON Lines
//! sidecar a live run appends each scored case to, so an interrupted run keeps what it paid for
//! and `--resume` can pick it up.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::runner::CaseResult;

#[derive(Debug)]
pub struct OutputError {
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl OutputError {
    fn new(message: String) -> OutputError {
        OutputError {
            message,
            source: None,
        }
    }

    fn caused(message: String, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        OutputError {
            message,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// Creates `out`'s parent directory and proves `out` can be opened for writing, so a bad path
/// fails before any case runs rather than after the whole run. A file that did not exist is not
/// left behind.
pub fn prepare(out: &Path) -> Result<(), OutputError> {
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| OutputError::caused(format!("cannot create {}", parent.display()), e))?;
    }
    let existed = out.exists();
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(out)
        .map_err(|e| OutputError::caused(format!("cannot write {}", out.display()), e))?;
    if !existed {
        let _ = std::fs::remove_file(out);
    }
    Ok(())
}

/// `<out>.partial.jsonl`, next to the report.
pub fn sidecar_path(out: &Path) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".partial.jsonl");
    out.with_file_name(name)
}

/// The sidecar's first line: what a resumed run must match for the union of the two runs to be
/// one measurement.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Header {
    pub dataset_version: String,
    pub guardian: String,
    pub profile: String,
    pub filter: Option<String>,
}

pub struct Sidecar {
    pub path: PathBuf,
    file: File,
}

impl Sidecar {
    /// Starts the sidecar at `path` with `header` and the `kept` records of a resumed run.
    /// An existing sidecar holds a run that was interrupted, so it is only replaced when it is the
    /// one being resumed. The new content is written aside and renamed into place, so a failure
    /// here never loses the sidecar being resumed.
    pub fn start(
        path: &Path,
        header: &Header,
        kept: &[CaseResult],
        resuming: Option<&Path>,
    ) -> Result<Sidecar, OutputError> {
        if path.exists() && !resuming.is_some_and(|from| same_file(from, path)) {
            return Err(OutputError::new(format!(
                "{} holds the cases of an interrupted run; pass `--resume {}` to continue it, or delete it",
                path.display(),
                path.display()
            )));
        }
        let mut text = serde_json::to_string(header).expect("the header serializes") + "\n";
        for record in kept {
            text.push_str(&serde_json::to_string(record).expect("a case result serializes"));
            text.push('\n');
        }
        let mut aside = path.as_os_str().to_os_string();
        aside.push(".tmp");
        let aside = PathBuf::from(aside);
        std::fs::write(&aside, text)
            .map_err(|e| OutputError::caused(format!("cannot write {}", aside.display()), e))?;
        std::fs::rename(&aside, path)
            .map_err(|e| OutputError::caused(format!("cannot write {}", path.display()), e))?;
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| OutputError::caused(format!("cannot write {}", path.display()), e))?;
        Ok(Sidecar {
            path: path.to_path_buf(),
            file,
        })
    }

    /// One whole line per write, so a run killed mid-case leaves at most the line it was writing
    /// incomplete.
    pub fn append(&mut self, record: &CaseResult) -> std::io::Result<()> {
        let line = serde_json::to_string(record).expect("a case result serializes") + "\n";
        self.file.write_all(line.as_bytes())?;
        self.file.flush()
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The records a resumed run keeps from the sidecar at `path`, which must have been written for
/// `expected`. An infrastructure failure is not a result, so it is dropped and its case runs
/// again. Returns the kept records and the number dropped.
pub fn resume(path: &Path, expected: &Header) -> Result<(Vec<CaseResult>, usize), OutputError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| OutputError::caused(format!("cannot read {}", path.display()), e))?;
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    // A run killed while writing a record leaves a final line without its newline.
    if lines.last().is_some_and(|l| !l.ends_with('\n')) {
        lines.pop();
    }
    let bad = |n: usize, e: serde_json::Error| {
        OutputError::caused(format!("{}:{n}: not a sidecar record", path.display()), e)
    };
    let (first, records) = lines
        .split_first()
        .ok_or_else(|| OutputError::new(format!("{} is empty", path.display())))?;
    let header: Header = serde_json::from_str(first).map_err(|e| bad(1, e))?;
    check_header(path, &header, expected)?;
    let (mut kept, mut dropped) = (Vec::new(), 0);
    for (i, line) in records.iter().enumerate() {
        let record: CaseResult = serde_json::from_str(line).map_err(|e| bad(i + 2, e))?;
        if kept.iter().any(|k: &CaseResult| k.id == record.id) {
            return Err(OutputError::new(format!(
                "{}: case {} is recorded twice",
                path.display(),
                record.id
            )));
        }
        if record.infrastructure_failure() {
            dropped += 1;
        } else {
            kept.push(record);
        }
    }
    Ok((kept, dropped))
}

fn check_header(path: &Path, found: &Header, expected: &Header) -> Result<(), OutputError> {
    let show = |filter: &Option<String>| filter.clone().unwrap_or_else(|| "none".to_string());
    let mismatch = [
        (
            "dataset version",
            found.dataset_version.clone(),
            expected.dataset_version.clone(),
        ),
        (
            "guardian",
            found.guardian.clone(),
            expected.guardian.clone(),
        ),
        ("profile", found.profile.clone(), expected.profile.clone()),
        ("filter", show(&found.filter), show(&expected.filter)),
    ]
    .into_iter()
    .find(|(_, found, expected)| found != expected);
    match mismatch {
        Some((what, found, expected)) => Err(OutputError::new(format!(
            "cannot resume from {}: it was written for {what} `{found}`, this run is `{expected}`",
            path.display()
        ))),
        None => Ok(()),
    }
}
