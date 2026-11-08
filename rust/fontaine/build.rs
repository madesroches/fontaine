//! Emits the git revision, build target and display version; the revision is `unknown` when
//! `.git` is absent.

use std::process::Command;

include!("build/version.rs");

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn tags_at_head() -> Vec<String> {
    git(&["tag", "--points-at", "HEAD"])
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

// Untracked files are ignored: the release workflow writes one before this runs.
// `--no-optional-locks` stops `status` rewriting the index this script watches.
fn tracked_changes() -> bool {
    git(&[
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=no",
    ])
    .is_some()
}

fn watch_git_path(name: &str) {
    if let Some(path) = git(&["rev-parse", "--git-path", name])
        && std::path::Path::new(&path).exists()
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    let found_sha = git(&["rev-parse", "--short=12", "HEAD"]);
    let sha = found_sha.clone().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FONTAINE_GIT_SHA={sha}");
    let version = display_version(
        &std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION"),
        found_sha.as_deref(),
        &tags_at_head(),
        tracked_changes(),
    );
    println!("cargo:rustc-env=FONTAINE_VERSION={version}");
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=FONTAINE_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    // `--git-path` resolves inside a worktree, where `.git` is a file.
    for name in ["HEAD", "packed-refs", "index"] {
        watch_git_path(name);
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&reference);
    }
}
