//! The second layer's input and its check: a bounded sample of the repository for the model scan,
//! and the validation of the scan's reply against that sample before the user sees it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use fontaine_guardian::env_scan::{Evidence, Finding, Reply};
use fontaine_tools::display::escape_controls;
use gix::bstr::ByteSlice;

use super::ScanFacts;
use super::discover::is_env_like;

/// Characters of the whole sample.
pub const SAMPLE_CAP: usize = 60_000;
/// Tracked paths the listing holds, and the characters they may take.
pub const LISTING_PATHS: usize = 2_000;
const LISTING_CHARS: usize = 20_000;
/// Characters of one file.
pub const FILE_CAP: usize = 6_000;
const FILES_CAP: usize = 40;
const INFRA_DIR_FILES: usize = 15;

/// Longest value of a proposal, in characters.
pub const VALUE_CAP: usize = 200;
/// Proposals kept per list.
pub const LIST_CAP: usize = 30;

pub struct Sample {
    /// What the model reads.
    pub text: String,
    /// The tracked paths the listing holds: the only paths an entry may cite.
    pub listing: BTreeSet<String>,
    /// Every tracked path, which a sensitive-location pattern must match.
    pub tracked: Vec<String>,
    /// The project-settings keys an entry may cite.
    pub settings: BTreeMap<String, String>,
}

/// Tracked paths relative to `root`: git's index when `root` is in a repository, otherwise the
/// files an ignore-aware walk finds.
fn tracked_paths(root: &Path) -> Vec<String> {
    let options = gix::open::Options::isolated().strict_config(true);
    if let Ok(repo) = gix::discover_opts(root, gix::discover::upwards::Options::default(), options)
        && let Ok(index) = repo.index_or_empty()
    {
        let paths: BTreeSet<String> = index
            .entries()
            .iter()
            .map(|entry| entry.path(&index).to_str_lossy().into_owned())
            .collect();
        return paths.into_iter().collect();
    }
    let mut paths: Vec<String> = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_global(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .take(50_000)
        .collect();
    paths.sort();
    paths
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

/// The files whose contents are sent, most useful first. `.env`-like files are never among
/// them: they appear in the listing by name only.
fn candidates(tracked: &[String]) -> Vec<&str> {
    const OTHER_CI: &[&str] = &[
        ".gitlab-ci.yml",
        "Jenkinsfile",
        ".circleci/config.yml",
        "azure-pipelines.yml",
        "bitbucket-pipelines.yml",
    ];
    const TASKS: &[&str] = &["Makefile", "justfile", "Taskfile.yml", "package.json"];
    const INFRA_ROOT: &[&str] = &["fly.toml", "vercel.json", "netlify.toml"];
    const INFRA_DIRS: &[&str] = &[
        "k8s",
        "kubernetes",
        "helm",
        "charts",
        "manifests",
        "deploy",
        "deployment",
        "infra",
    ];
    let usable = |path: &&String| !is_env_like(path);
    let all = || tracked.iter().filter(usable);
    let mut out: Vec<&str> = Vec::new();
    out.extend(
        all()
            .filter(|p| OTHER_CI.contains(&p.as_str()))
            .map(|p| p.as_str()),
    );
    out.extend(
        all()
            .filter(|p| TASKS.contains(&p.as_str()))
            .map(|p| p.as_str()),
    );
    out.extend(
        all()
            .filter(|p| p.ends_with(".tf") && depth(p) <= 3)
            .map(|p| p.as_str()),
    );
    out.extend(
        all()
            .filter(|p| {
                depth(p) <= 2
                    && basename(p).starts_with("docker-compose")
                    && (p.ends_with(".yml") || p.ends_with(".yaml"))
            })
            .map(|p| p.as_str()),
    );
    out.extend(
        all()
            .filter(|p| INFRA_ROOT.contains(&p.as_str()))
            .map(|p| p.as_str()),
    );
    out.extend(
        all()
            .filter(|p| {
                (p.ends_with(".yml") || p.ends_with(".yaml"))
                    && p.split('/')
                        .take(depth(p))
                        .any(|dir| INFRA_DIRS.contains(&dir))
            })
            .map(|p| p.as_str())
            .take(INFRA_DIR_FILES),
    );
    out.extend(
        all()
            .filter(|p| {
                depth(p) == 0 && {
                    let upper = p.to_uppercase();
                    upper.starts_with("README") || upper.starts_with("CONTRIBUTING")
                }
            })
            .map(|p| p.as_str()),
    );
    let mut seen = BTreeSet::new();
    out.retain(|path| seen.insert(*path));
    out.truncate(FILES_CAP);
    out
}

fn truncate_chars(text: &str, cap: usize) -> String {
    match text.char_indices().nth(cap) {
        Some((at, _)) => format!("{}\n[truncated]", &text[..at]),
        None => text.to_string(),
    }
}

fn read_file(root: &Path, relative: &str) -> Option<String> {
    let path = root.join(relative);
    // A link could lead anywhere; only a plain file of the repository is read.
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take((FILE_CAP * 4) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if basename(relative) == "package.json" {
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let scripts = value.get("scripts")?.as_object()?;
        let lines: Vec<String> = scripts
            .iter()
            .filter_map(|(name, command)| Some(format!("{name}: {}", command.as_str()?)))
            .collect();
        return Some(truncate_chars(&lines.join("\n"), FILE_CAP));
    }
    Some(truncate_chars(&text, FILE_CAP))
}

/// Builds the sample of the repository at `repo_root`, with the first layer's `settings`.
pub fn gather(repo_root: &Path, settings: &BTreeMap<String, String>) -> Sample {
    let tracked = tracked_paths(repo_root);
    let mut text = String::from("project_settings:\n");
    for (key, value) in settings {
        text.push_str(&format!("{key}: {}\n", one_line(value)));
    }
    if settings.is_empty() {
        text.push_str("(none)\n");
    }
    text.push_str("\ntracked_paths:\n");
    let mut listing = BTreeSet::new();
    let mut listed = 0;
    for path in tracked.iter().take(LISTING_PATHS) {
        if listed + path.len() + 1 > LISTING_CHARS {
            break;
        }
        listed += path.len() + 1;
        text.push_str(&one_line(path));
        text.push('\n');
        listing.insert(path.clone());
    }
    for path in candidates(&tracked) {
        let Some(body) = read_file(repo_root, path) else {
            continue;
        };
        let section = format!("\nfile {path}:\n{body}\n");
        if text.chars().count() + section.chars().count() > SAMPLE_CAP {
            break;
        }
        text.push_str(&section);
    }
    Sample {
        text,
        listing,
        tracked,
        settings: settings.clone(),
    }
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The proposals that survived the checks, and a line for each that did not.
pub struct Validated {
    pub scan: ScanFacts,
    pub dropped: Vec<String>,
}

pub fn validate(reply: Reply, sample: &Sample, model: &str) -> Validated {
    let mut dropped = Vec::new();
    let mut keep = |list: &'static str, findings: Vec<Finding>| -> Vec<Finding> {
        let mut kept: Vec<Finding> = Vec::new();
        for finding in findings {
            if let Err(reason) = check(list, &finding, sample) {
                dropped.push(format!("{list} {:?}: {reason}", shown(&finding.value)));
            } else if kept.iter().any(|k| k.value == finding.value) {
                dropped.push(format!("{list} {:?}: listed twice", shown(&finding.value)));
            } else if kept.len() == LIST_CAP {
                dropped.push(format!(
                    "{list} {:?}: more than {LIST_CAP} entries",
                    shown(&finding.value)
                ));
            } else {
                kept.push(finding);
            }
        }
        kept
    };
    let scan = ScanFacts {
        model: model.to_string(),
        sensitive_locations: keep("sensitive_locations", reply.sensitive_locations),
        deploy_targets: keep("deploy_targets", reply.deploy_targets),
        protected_targets: keep("protected_targets", reply.protected_targets),
        trusted_destinations: keep("trusted_destinations", reply.trusted_destinations),
    };
    Validated { scan, dropped }
}

/// A value as the report shows it: cut short and on one line.
fn shown(value: &str) -> String {
    truncate_chars(&one_line(value), 60)
}

fn check(list: &str, finding: &Finding, sample: &Sample) -> Result<(), &'static str> {
    let value = &finding.value;
    if value.trim().is_empty() {
        return Err("empty");
    }
    if value.chars().count() > VALUE_CAP {
        return Err("longer than 200 characters");
    }
    if value.chars().any(char::is_control) {
        return Err("not a single plain line");
    }
    let cited = match &finding.evidence {
        Evidence::File(text) | Evidence::Setting(text) => text,
    };
    if cited.chars().any(char::is_control) {
        return Err("cites evidence that is not a single plain line");
    }
    match &finding.evidence {
        Evidence::File(path) if !sample.listing.contains(path) => {
            return Err("cites a file that is not in the tracked-path listing");
        }
        Evidence::Setting(key) if !sample.settings.contains_key(key) => {
            return Err("cites a project setting that does not exist");
        }
        _ => {}
    }
    match list {
        "sensitive_locations" if !matches_a_tracked_path(value, &sample.tracked) => {
            Err("matches no tracked path")
        }
        "trusted_destinations" if !is_destination(value) => {
            Err("not a host, bucket or registry URL, or wildcards too broadly")
        }
        _ => Ok(()),
    }
}

fn matches_a_tracked_path(value: &str, tracked: &[String]) -> bool {
    let value = value.trim();
    let pattern = if value.ends_with('/') {
        format!("{value}**")
    } else {
        value.to_string()
    };
    let Ok(glob) = globset::Glob::new(&pattern) else {
        return false;
    };
    let matcher = glob.compile_matcher();
    let directory = format!("{}/", value.trim_end_matches('/'));
    tracked
        .iter()
        .any(|path| matcher.is_match(path) || path.starts_with(&directory))
}

/// A host, a bucket URL or a registry URL, with no userinfo, query or fragment, and no wildcard
/// broader than the end of one label that already carries an org-identifying prefix.
pub fn is_destination(value: &str) -> bool {
    const SCHEMES: &[&str] = &[
        "http",
        "https",
        "s3",
        "gs",
        "gcs",
        "az",
        "abfs",
        "oci",
        "sparse+https",
        "git+https",
        "ssh",
    ];
    if value.chars().any(|c| c.is_whitespace() || c.is_control())
        || value.contains(['@', '?', '#', '\\'])
    {
        return false;
    }
    let (scheme, rest) = match value.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, value),
    };
    if scheme.is_some_and(|s| !SCHEMES.contains(&s)) {
        return false;
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if scheme.is_none() && !path.is_empty() {
        return false;
    }
    if path.contains('*') {
        return false;
    }
    let host = authority.split_once(':').map_or(authority, |(host, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            host
        } else {
            ""
        }
    });
    // A bucket scheme names a bucket, which has no dot; every other form needs a dotted host.
    let bucket = matches!(scheme, Some("s3" | "gs" | "gcs" | "az" | "abfs" | "oci"));
    if host.is_empty() || host.starts_with(['.', '-']) || host.ends_with(['.', '-']) {
        return false;
    }
    if !bucket && !host.contains('.') {
        return false;
    }
    if host.matches('*').count() > 1 {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let last = labels.len() - 1;
    for (i, label) in labels.iter().enumerate() {
        if label.is_empty()
            || !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '*'))
        {
            return false;
        }
        if let Some(stem) = label.strip_suffix('*') {
            let named = stem.chars().filter(|c| c.is_ascii_alphanumeric()).count();
            if named < 3 || stem.contains('*') || (i == last && labels.len() > 1) {
                return false;
            }
        } else if label.contains('*') {
            return false;
        }
    }
    true
}

/// The proposals as the user reviews them. Entries that relax the guardian come first, under
/// their own heading.
pub fn review_text(scan: &ScanFacts) -> String {
    fn section(out: &mut String, heading: &str, findings: &[Finding]) {
        if findings.is_empty() {
            return;
        }
        out.push_str(&format!("{heading}\n"));
        for finding in findings {
            let evidence = match &finding.evidence {
                Evidence::File(path) => format!("file {}", escape_controls(path)),
                Evidence::Setting(key) => format!("setting {}", escape_controls(key)),
            };
            out.push_str(&format!("  - {}  (evidence: {evidence})\n", finding.value));
        }
        out.push('\n');
    }
    let mut out = String::new();
    section(
        &mut out,
        "Trusted destinations (these RELAX the guardian: sending the project's data there stops counting as exfiltration):",
        &scan.trusted_destinations,
    );
    section(
        &mut out,
        "Sensitive locations (tighten):",
        &scan.sensitive_locations,
    );
    section(&mut out, "Deploy targets (tighten):", &scan.deploy_targets);
    section(
        &mut out,
        "Protected targets (tighten):",
        &scan.protected_targets,
    );
    out.trim_end().to_string()
}
