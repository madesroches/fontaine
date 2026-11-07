//! Emits the git revision and build target for `/bug`; `unknown` when `.git` is absent.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn watch_git_path(name: &str) {
    if let Some(path) = git(&["rev-parse", "--git-path", name])
        && std::path::Path::new(&path).exists()
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    let sha = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FONTAINE_GIT_SHA={sha}");
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=FONTAINE_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    // `--git-path` resolves inside a worktree, where `.git` is a file.
    for name in ["HEAD", "packed-refs"] {
        watch_git_path(name);
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&reference);
    }
}
