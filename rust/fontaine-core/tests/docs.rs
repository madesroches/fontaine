//! Validates the documentation tree: every ```toml block parses against the schema it names,
//! every relative link resolves, and no placeholder markers are left behind.

use std::path::{Path, PathBuf};

use fontaine_core::config::{Column, validate_settings};
use fontaine_core::credentials::validate_credentials_toml;
use fontaine_core::trust::validate_trust_toml;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn docs_pages(root: &Path) -> Vec<PathBuf> {
    let mut pages: Vec<PathBuf> = std::fs::read_dir(root.join("docs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    pages.sort();
    pages
}

/// Docs pages plus the other markdown files whose links are checked.
fn checked_files(root: &Path) -> Vec<PathBuf> {
    let mut files = docs_pages(root);
    files.push(root.join("README.md"));
    files.push(root.join("evals/README.md"));
    files
}

struct Fence {
    info: String,
    body: String,
    line: usize,
}

fn fences(text: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut open: Option<Fence> = None;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            match open.take() {
                Some(fence) => out.push(fence),
                None => {
                    open = Some(Fence {
                        info: rest.trim().to_string(),
                        body: String::new(),
                        line: i + 1,
                    })
                }
            }
        } else if let Some(fence) = open.as_mut() {
            fence.body.push_str(line);
            fence.body.push('\n');
        }
    }
    assert!(open.is_none(), "unterminated code fence");
    out
}

fn prose_without_fences(text: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn every_toml_block_validates() {
    let root = repo_root();
    let mut checked = 0;
    for page in docs_pages(&root) {
        let text = std::fs::read_to_string(&page).unwrap();
        for fence in fences(&text) {
            let mut words = fence.info.split_whitespace();
            if words.next() != Some("toml") {
                continue;
            }
            let schema = words.next();
            let result = match schema {
                Some("trust.toml") => validate_trust_toml(&fence.body),
                Some("credentials.toml") => validate_credentials_toml(&fence.body),
                None => {
                    validate_settings(&fence.body, &page, Column::User).map_err(|e| e.to_string())
                }
                Some(other) => panic!(
                    "{}:{}: unknown toml block target `{other}`",
                    page.display(),
                    fence.line
                ),
            };
            if let Err(e) = result {
                panic!(
                    "{}:{}: toml block does not validate: {e}\n{}",
                    page.display(),
                    fence.line,
                    fence.body
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "found no toml blocks to check");
}

#[test]
fn every_relative_link_resolves() {
    let root = repo_root();
    for file in checked_files(&root) {
        let text = std::fs::read_to_string(&file).unwrap();
        let prose = prose_without_fences(&text);
        let mut rest = prose.as_str();
        while let Some(start) = rest.find("](") {
            let after = &rest[start + 2..];
            let Some(end) = after.find(')') else { break };
            let target = &after[..end];
            rest = &after[end..];

            if target.is_empty()
                || target.starts_with('#')
                || target.contains("://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let path_part = target.split('#').next().unwrap();
            let resolved = file.parent().unwrap().join(path_part);
            assert!(
                resolved.exists(),
                "{}: link `{target}` does not resolve",
                file.display()
            );
        }
    }
}

#[test]
fn no_placeholder_markers() {
    let root = repo_root();
    for file in checked_files(&root) {
        let text = std::fs::read_to_string(&file).unwrap();
        for (i, line) in text.lines().enumerate() {
            for marker in ["TODO", "TBD"] {
                let hit = line.match_indices(marker).any(|(at, _)| {
                    let before = line[..at].chars().next_back();
                    let after = line[at + marker.len()..].chars().next();
                    !before.is_some_and(|c| c.is_alphanumeric())
                        && !after.is_some_and(|c| c.is_alphanumeric())
                });
                assert!(!hit, "{}:{}: contains {marker}", file.display(), i + 1);
            }
        }
    }
}

#[test]
fn the_required_pages_exist_with_content() {
    let root = repo_root();
    for page in [
        "docs/index.md",
        "docs/architecture.md",
        "docs/configuration.md",
        "evals/README.md",
        "README.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "CHANGELOG.md",
    ] {
        let text = std::fs::read_to_string(root.join(page)).unwrap_or_else(|_| panic!("{page}"));
        assert!(text.lines().count() > 8, "{page} is a stub");
    }
}
