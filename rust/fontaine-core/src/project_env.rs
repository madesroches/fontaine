//! What `fontaine auto-mode setup` learns about a project, stored once per project in the data
//! directory and read at session start as facts for the guardian. Nothing here runs at session
//! start except a file read: discovery is an explicit, user-run command.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeDelta, Utc};
use fontaine_guardian::{
    EnvFileFacts, ProjectEnvironmentFacts, Protection, RemoteEnvironment, ScanFindings, Visibility,
};
use serde::{Deserialize, Serialize};

use crate::config::{AutoMode, PermissionMode};
use crate::notice::Notice;
use crate::owner_only;

pub mod discover;
pub mod sample;

pub use fontaine_guardian::env_scan::{self, Evidence, Finding};

pub const FILE_NAME: &str = "environment.json";
pub const VERSION: u32 = 1;

/// A `public` visibility older than this renders as unknown: a repository made private since
/// would otherwise keep a relaxation it no longer earns.
pub const PUBLIC_EXPIRY_DAYS: i64 = 30;

pub const SETUP_COMMAND: &str = "fontaine auto-mode setup";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEnvironment {
    pub version: u32,
    /// RFC 3339, from `audit::now_rfc3339`.
    pub discovered_at: String,
    pub remotes: Vec<RemoteFacts>,
    /// Workflow paths triggered by a tag push or a release, read from the repository files.
    pub tag_release_workflows: Vec<String>,
    /// `jobs.*.environment` names of the workflows.
    pub workflow_environments: Vec<String>,
    pub package_registries: Vec<Registry>,
    pub env_files: Vec<EnvFileFacts>,
    pub stack: Vec<String>,
    /// The model scan's proposals the user accepted. None when it was skipped, failed or
    /// declined.
    pub scan: Option<ScanFacts>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanFacts {
    /// `<account>/<model>` that produced the proposals.
    pub model: String,
    pub sensitive_locations: Vec<Finding>,
    pub deploy_targets: Vec<Finding>,
    pub protected_targets: Vec<Finding>,
    pub trusted_destinations: Vec<Finding>,
}

impl ScanFacts {
    pub fn count(&self) -> usize {
        self.sensitive_locations.len()
            + self.deploy_targets.len()
            + self.protected_targets.len()
            + self.trusted_destinations.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFacts {
    /// The fetch URL as `ledger::git_remotes` returns it.
    pub url: String,
    /// `owner/repo` for a github.com remote.
    pub slug: Option<String>,
    pub visibility: Visibility,
    pub default_branch: Option<String>,
    pub default_branch_protection: Protection,
    /// Rule types of `rules/branches/<default>`.
    pub branch_rules: Vec<String>,
    pub deploy_environments: Vec<String>,
}

/// A package registry a repository file names: the host only, never userinfo, a token or a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub host: String,
    /// The file that names it, relative to the directory it was read from.
    pub file: String,
}

pub fn path(data_dir: &Path, primary_root: &Path) -> PathBuf {
    data_dir
        .join("auto_mode")
        .join(crate::memory::project_key(primary_root))
        .join(FILE_NAME)
}

/// Writes the record owner-only, atomically, and returns where it went.
pub fn store(
    data_dir: &Path,
    primary_root: &Path,
    record: &ProjectEnvironment,
) -> io::Result<PathBuf> {
    let file = path(data_dir, primary_root);
    if let Some(dir) = file.parent() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match owner_only::create_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    let text = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    owner_only::write_atomic(&file, &text)?;
    Ok(file)
}

/// The stored record, if there is a usable one, and a one-line warning when there is a file
/// that is not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    pub record: Option<ProjectEnvironment>,
    pub warning: Option<String>,
}

pub fn load(data_dir: &Path, primary_root: &Path) -> Loaded {
    let file = path(data_dir, primary_root);
    let text = match crate::file_lock::read_shared(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => {
            return Loaded {
                record: None,
                warning: Some(format!(
                    "cannot read the project environment {}: {}",
                    file.display(),
                    fontaine_tools::error_chain::chain(&e)
                )),
            };
        }
    };
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let version = serde_json::from_str::<Versioned>(&text).map(|v| v.version);
    match version {
        Ok(found) if found > VERSION => {
            return Loaded {
                record: None,
                warning: Some(format!(
                    "{} is version {found}, newer than this build understands ({VERSION}); ignoring it",
                    file.display()
                )),
            };
        }
        Ok(_) => {}
        Err(e) => return corrupt(&file, &e),
    }
    match serde_json::from_str::<ProjectEnvironment>(&text) {
        Ok(record) => Loaded {
            record: Some(record),
            warning: None,
        },
        Err(e) => corrupt(&file, &e),
    }
}

fn corrupt(file: &Path, error: &serde_json::Error) -> Loaded {
    Loaded {
        record: None,
        warning: Some(format!(
            "ignoring the project environment {}: {}",
            file.display(),
            fontaine_tools::error_chain::chain(error)
        )),
    }
}

/// The record with the facts of remotes that have gone removed, so a remote that changed since
/// setup gets none of its old facts.
pub fn filter_to_remotes(
    mut record: ProjectEnvironment,
    git_remotes: &[String],
) -> ProjectEnvironment {
    record
        .remotes
        .retain(|remote| git_remotes.contains(&remote.url));
    record
}

fn collected_at(record: &ProjectEnvironment) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&record.discovered_at)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// The day the facts were collected; the stored text when it does not parse.
fn collected_on(record: &ProjectEnvironment) -> String {
    collected_at(record).map_or_else(
        || record.discovered_at.clone(),
        |at| at.format("%Y-%m-%d").to_string(),
    )
}

/// Whether the record's public visibility is too old to rely on. A date that does not parse
/// counts as old: the relaxation is the side to be wrong on.
fn public_expired(record: &ProjectEnvironment, now: DateTime<Utc>) -> bool {
    collected_at(record).is_none_or(|at| now - at > TimeDelta::days(PUBLIC_EXPIRY_DAYS))
}

/// The record as a session uses it: a `public` visibility that has outlived
/// [`PUBLIC_EXPIRY_DAYS`] downgraded to its expired form. Facts that only tighten, and the
/// entries the user accepted, do not expire.
pub fn expire(mut record: ProjectEnvironment, now: DateTime<Utc>) -> ProjectEnvironment {
    if public_expired(&record, now) {
        let day = collected_on(&record);
        for remote in &mut record.remotes {
            if remote.visibility == Visibility::Public {
                remote.visibility = Visibility::Expired(day.clone());
            }
        }
    }
    record
}

/// [`expire`] as of now.
pub fn expire_now(record: ProjectEnvironment) -> ProjectEnvironment {
    expire(record, Utc::now())
}

/// What the guardian is told of the record.
pub fn to_facts(record: &ProjectEnvironment) -> ProjectEnvironmentFacts {
    let values = |findings: &[Finding]| findings.iter().map(|f| f.value.clone()).collect();
    ProjectEnvironmentFacts {
        collected_on: collected_on(record),
        remotes: record
            .remotes
            .iter()
            .map(|remote| RemoteEnvironment {
                url: remote.url.clone(),
                visibility: remote.visibility.clone(),
                default_branch: remote.default_branch.clone(),
                protection: remote.default_branch_protection,
                branch_rules: remote.branch_rules.clone(),
                deploy_environments: remote.deploy_environments.clone(),
            })
            .collect(),
        tag_release_workflows: record.tag_release_workflows.clone(),
        workflow_environments: record.workflow_environments.clone(),
        package_registries: record
            .package_registries
            .iter()
            .map(|registry| registry.host.clone())
            .collect(),
        env_files: record.env_files.clone(),
        stack: record.stack.clone(),
        scan: record.scan.as_ref().map(|scan| ScanFindings {
            sensitive_locations: values(&scan.sensitive_locations),
            deploy_targets: values(&scan.deploy_targets),
            protected_targets: values(&scan.protected_targets),
            trusted_destinations: values(&scan.trusted_destinations),
        }),
    }
}

/// The warning an `auto` session raises when the project's facts are missing or stale. The
/// caller decides whether the guardian is on and whether this is the root session.
pub fn setup_notice(
    record: Option<&ProjectEnvironment>,
    git_remotes: &[String],
    mode: PermissionMode,
    auto_mode: &AutoMode,
    now: DateTime<Utc>,
) -> Option<Notice> {
    if mode != PermissionMode::Auto || !auto_mode.setup_reminder {
        return None;
    }
    let Some(record) = record else {
        return Some(Notice::warning(
            "auto mode doesn't know this project's environment (repo visibility, protected branches, release triggers)",
            Some(format!(
                "run `{SETUP_COMMAND}` (or /auto-mode-setup) to collect it once for this project"
            )),
        ));
    };
    let stale = |what: String| {
        Some(Notice::warning(
            what,
            Some(format!("rerun `{SETUP_COMMAND}`")),
        ))
    };
    if let Some(missing) = git_remotes
        .iter()
        .find(|url| !record.remotes.iter().any(|remote| &remote.url == *url))
    {
        return stale(format!(
            "auto mode has no environment facts for the remote {missing}"
        ));
    }
    if public_expired(record, now)
        && let Some(remote) = record
            .remotes
            .iter()
            .find(|remote| remote.visibility == Visibility::Public)
    {
        return stale(format!(
            "auto mode's record that {} is public is over {PUBLIC_EXPIRY_DAYS} days old and no longer counts",
            remote.url
        ));
    }
    None
}

/// Why the model scan does not run, when it does not. A scan sends repository files to the
/// guardian's provider and proposes entries the user must review, so it needs a trusted
/// workspace and a terminal to review in.
pub fn scan_gate(no_model: bool, terminal: bool, trusted: bool) -> Result<(), String> {
    if no_model {
        return Err("--no-model".to_string());
    }
    if !terminal {
        return Err(
            "no terminal to review proposals in; the model scan needs an interactive run"
                .to_string(),
        );
    }
    if !trusted {
        return Err(
            "this workspace is not trusted, so its files are not sent to a model".to_string(),
        );
    }
    Ok(())
}

/// What the model scan proposed, after validation.
pub struct Proposals {
    pub scan: ScanFacts,
    /// A line for each proposal the checks dropped.
    pub dropped: Vec<String>,
    pub usage: rig_core::completion::Usage,
}

/// Runs the model scan over `sample` and validates its reply. Nothing is saved: the caller
/// shows the proposals to the user first.
pub async fn propose(
    model: &rig_core::DynModel<rig_core::operation::Completion>,
    label: &str,
    settings: &env_scan::Settings,
    sample: &sample::Sample,
) -> Result<Proposals, env_scan::ScanError> {
    let (reply, usage) = env_scan::scan(model, settings, &sample.text).await?;
    let validated = sample::validate(reply, sample, label);
    Ok(Proposals {
        scan: validated.scan,
        dropped: validated.dropped,
        usage,
    })
}

/// How the model scan ended, for the setup report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanStatus {
    /// It did not run, and why.
    Skipped(String),
    /// It ran and failed, with the cause.
    Failed(String),
    /// The user declined its proposals.
    Declined(usize),
    /// The user accepted its proposals and they were saved.
    Accepted(usize),
}

impl ScanStatus {
    pub fn line(&self) -> String {
        match self {
            ScanStatus::Skipped(why) => format!("model scan: skipped ({why})"),
            ScanStatus::Failed(why) => {
                format!("model scan: failed ({why}); nothing from it was saved")
            }
            ScanStatus::Declined(n) => {
                format!("model scan: {n} proposals declined; nothing from it was saved")
            }
            ScanStatus::Accepted(n) => format!("model scan: {n} proposals accepted and saved"),
        }
    }
}

fn joined(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

/// The setup report: each fact with where it came from, then each step that fell back to
/// unknown and why.
pub fn report(record: &ProjectEnvironment, fallbacks: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    for remote in &record.remotes {
        lines.push(format!("remote {}", remote.url));
        let visibility = match &remote.visibility {
            Visibility::Public => "public (GitHub API)".to_string(),
            Visibility::Private => "private (GitHub API)".to_string(),
            Visibility::Internal => "internal (GitHub API)".to_string(),
            Visibility::Unverified => "unverified: an org policy barred the check".to_string(),
            Visibility::Unknown => "unknown".to_string(),
            Visibility::Expired(day) => format!("unknown (last seen public on {day})"),
        };
        lines.push(format!("  visibility {visibility}"));
        if let Some(branch) = &remote.default_branch {
            let protection = match remote.default_branch_protection {
                Protection::Protected => "protected",
                Protection::Unprotected => "unprotected",
                Protection::Unknown => "protection unknown",
            };
            let rules = if remote.branch_rules.is_empty() {
                String::new()
            } else {
                format!(" (rules: {})", remote.branch_rules.join(", "))
            };
            lines.push(format!(
                "  default branch {branch}, {protection}{rules} (GitHub API)"
            ));
        }
        if !remote.deploy_environments.is_empty() {
            lines.push(format!(
                "  deploy environments: {} (GitHub API)",
                remote.deploy_environments.join(", ")
            ));
        }
    }
    if record.remotes.is_empty() {
        lines.push("no git remotes".to_string());
    }
    lines.push(format!(
        "tag-push release workflows: {} (repository files)",
        joined(&record.tag_release_workflows)
    ));
    lines.push(format!(
        "workflow environments: {} (repository files)",
        joined(&record.workflow_environments)
    ));
    let registries: Vec<String> = record
        .package_registries
        .iter()
        .map(|r| format!("{} ({})", r.host, r.file))
        .collect();
    lines.push(format!(
        "package registries: {} (repository files)",
        joined(&registries)
    ));
    let env_files: Vec<String> = record
        .env_files
        .iter()
        .map(|f| {
            format!(
                "{} ({})",
                f.name,
                if f.gitignored {
                    "gitignored"
                } else {
                    "not ignored"
                }
            )
        })
        .collect();
    lines.push(format!(
        "env files: {} (repository files)",
        joined(&env_files)
    ));
    lines.push(format!(
        "stack: {} (repository files)",
        joined(&record.stack)
    ));
    if let Some(scan) = &record.scan {
        let values = |findings: &[Finding]| {
            joined(&findings.iter().map(|f| f.value.clone()).collect::<Vec<_>>())
        };
        lines.push(format!(
            "accepted from the model scan ({}): sensitive locations: {}; deploy targets: {}; protected targets: {}; trusted destinations: {}",
            scan.model,
            values(&scan.sensitive_locations),
            values(&scan.deploy_targets),
            values(&scan.protected_targets),
            values(&scan.trusted_destinations),
        ));
    }
    for fallback in fallbacks {
        lines.push(format!("fell back to unknown: {fallback}"));
    }
    lines
}

#[cfg(test)]
mod tests;
