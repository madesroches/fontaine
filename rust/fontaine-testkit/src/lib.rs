//! Dev-only test helpers shared by the workspace's integration tests.
//!
//! Integration tests spawn Cargo example binaries (`hook_marker`, `repo_mcp_server`, ...) as
//! helper processes and locate them under `target/<profile>/examples/`. A workspace-wide
//! `cargo test` builds every member's examples, but a per-package run builds only that
//! package's targets and would silently use whatever example binaries happen to be on disk;
//! stale ones fail with timeouts far from the cause.
//!
//! [`example_bin`] builds the requested example on first use per test process (a no-op under
//! the workspace run, since cargo's own freshness check is exact) and returns its path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

static CACHE: OnceLock<Mutex<HashMap<(String, String), PathBuf>>> = OnceLock::new();

/// Returns the path to the built example `name` of workspace package `package`, building it on
/// first use. Panics with cargo's combined output if the build fails.
pub fn example_bin(package: &str, name: &str) -> PathBuf {
    let key = (package.to_string(), name.to_string());
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("fontaine-testkit cache mutex poisoned");
    if let Some(path) = cache.get(&key) {
        return path.clone();
    }
    let exe = std::env::current_exe().expect("current_exe() unavailable in a test process");
    let cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    let layout = Layout::from_exe(&exe, cargo_target_dir.as_deref()).unwrap_or_else(|| {
        panic!(
            "{} is not under a target/<profile>/deps directory",
            exe.display()
        )
    });
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    cmd.args(["build", "-p", package, "--example", name])
        .arg("--target-dir")
        .arg(&layout.target_dir)
        .arg("--profile")
        .arg(profile_flag(&layout.profile_dir));
    if let Some(triple) = &layout.triple {
        // A triple from CARGO_BUILD_TARGET or config is inherited by the child, but an explicit
        // command-line --target is not; without the flag the nested build would place the
        // example where this helper does not look.
        cmd.arg("--target").arg(triple);
    }
    let out = cmd.output().unwrap_or_else(|e| {
        panic!(
            "failed to spawn {}: {e}",
            cmd.get_program().to_string_lossy()
        )
    });
    if !out.status.success() {
        panic!(
            "cargo build -p {package} --example {name} failed with {}:\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    let path = layout
        .profile_dir
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "{} was not produced by the build",
        path.display()
    );
    cache.insert(key, path.clone());
    path
}

/// The layout of a test binary's path: `target[<triple>]/<profile>/deps/<test>`.
struct Layout {
    target_dir: PathBuf,
    triple: Option<String>,
    profile_dir: PathBuf,
}

impl Layout {
    /// `cargo_target_dir` is the `CARGO_TARGET_DIR` of the outer build, which Cargo exports to
    /// the test process when the outer build used one.
    fn from_exe(exe: &Path, cargo_target_dir: Option<&Path>) -> Option<Layout> {
        let deps = exe.parent()?;
        if deps.file_name()? != "deps" {
            return None;
        }
        let profile_dir = deps.parent()?;
        let above = profile_dir.parent()?;
        // Cargo's configured target dir is authoritative when the exe descends from it:
        // anything between it and the profile dir is a triple, and the configured dir's
        // own name is part of the target dir even when it contains a `-`.
        let matched = cargo_target_dir.and_then(|c| configured_target_match(above, c));
        let (triple, target_dir) = match matched {
            Some((dir, rest)) => {
                let triple = rest
                    .components()
                    .next()
                    .map(|segment| segment.as_os_str().to_string_lossy().into_owned());
                let target_dir = if triple.is_none() {
                    above.to_path_buf()
                } else {
                    dir
                };
                (triple, target_dir)
            }
            // Without a configured dir (or an exe outside it) the workspace's `target/`
            // name has no `-`, so a `-`-bearing segment must be a triple. A
            // `--target-dir` passed only on the outer command line with a `-` in its
            // name and no env var is indistinguishable from a triple; that combination
            // is not supported.
            None if looks_like_triple(above) => (
                Some(above.file_name()?.to_string_lossy().into_owned()),
                above.parent()?.to_path_buf(),
            ),
            None => (None, above.to_path_buf()),
        };
        Some(Layout {
            target_dir,
            triple,
            profile_dir: profile_dir.to_path_buf(),
        })
    }
}

/// Locates a configured `CARGO_TARGET_DIR` within `above` (the exe's ancestor above the
/// profile dir), returning the resolved target dir and the path remaining between it and
/// `above` (the triple segment, if any). Absolute configured dirs match by prefix; relative
/// ones match as a contiguous run of `above`'s trailing components (with the base
/// `current_exe()` cannot reveal), because `current_exe()` is absolute and Cargo resolved
/// the configured dir against that unknown base.
fn configured_target_match(above: &Path, configured: &Path) -> Option<(PathBuf, PathBuf)> {
    if configured.is_absolute() {
        let rest = above.strip_prefix(configured).ok()?;
        return Some((configured.to_path_buf(), rest.to_path_buf()));
    }
    let suffix: Vec<_> = configured.components().collect();
    let comps: Vec<_> = above.components().collect();
    if suffix.is_empty() || suffix.len() > comps.len() {
        return None;
    }
    // The configured dir sits directly below the (unknown) base Cargo resolved it
    // against, so it is a contiguous run of `above`'s components with at most one
    // segment — the triple — after it.
    for start in (0..=comps.len() - suffix.len()).rev() {
        if comps[start..start + suffix.len()] != suffix[..] {
            continue;
        }
        let rest: PathBuf = comps[start + suffix.len()..].iter().collect();
        if rest.components().count() > 1 {
            continue;
        }
        let base: PathBuf = comps[..start].iter().collect();
        return Some((base.join(configured), rest));
    }
    None
}

/// Best-effort triple detection for the optional segment between the target dir and the
/// profile dir. Any directory name with no `-` (including a custom `CARGO_TARGET_DIR`) is not
/// a triple; a custom target dir whose name itself contains a `-` would be misread as one.
fn looks_like_triple(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.contains('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The `--profile` flag that reproduces the profile under which `cargo test` built the
/// example in this profile dir.
fn profile_flag(profile_dir: &Path) -> String {
    let name = profile_dir
        .file_name()
        .expect("profile dir is a root or relative path")
        .to_string_lossy();
    if name == "debug" {
        // `cargo test` compiles examples under the `test` profile, which shares the `debug`
        // output dir with `dev`; `--profile test` verified accepted and keeps an example a
        // workspace `cargo test` built as a no-op, where a plain `dev` build would rebuild it.
        "test".to_string()
    } else {
        name.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(exe: &str) -> Option<Layout> {
        layout_with_env(exe, None)
    }

    fn layout_with_env(exe: &str, cargo_target_dir: Option<&str>) -> Option<Layout> {
        Layout::from_exe(Path::new(exe), cargo_target_dir.map(Path::new))
    }

    #[test]
    fn exe_without_triple_gives_target_dir_and_no_triple() {
        let l = layout("/ws/rust/target/debug/deps/my_test").unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/rust/target"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/ws/rust/target/debug"));
    }

    #[test]
    fn exe_with_triple_sets_triple_and_strips_it_from_target_dir() {
        let l = layout("/ws/rust/target/x86_64-pc-windows-gnu/debug/deps/my_test.exe").unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/rust/target"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-pc-windows-gnu"));
        assert_eq!(
            l.profile_dir,
            Path::new("/ws/rust/target/x86_64-pc-windows-gnu/debug")
        );
    }

    #[test]
    fn custom_profile_dir_is_kept_whole() {
        let l = layout("/ws/rust/target/ci/deps/my_test").unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/rust/target"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/ws/rust/target/ci"));
    }

    #[test]
    fn custom_target_dir_without_triple_is_not_misread() {
        let l = layout("/ws/mytarget/release/deps/my_test").unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/mytarget"));
        assert_eq!(l.triple, None);
    }

    #[test]
    fn hyphenated_custom_target_dir_is_not_misread() {
        let l = layout_with_env("/ws/my-target/debug/deps/my_test", Some("/ws/my-target")).unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/my-target"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/ws/my-target/debug"));
    }

    #[test]
    fn triple_under_configured_target_dir_is_still_detected() {
        let l = layout_with_env(
            "/ws/my-target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            Some("/ws/my-target"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/my-target"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-pc-windows-gnu"));
        assert_eq!(
            l.profile_dir,
            Path::new("/ws/my-target/x86_64-pc-windows-gnu/debug")
        );
    }

    #[test]
    fn relative_hyphenated_target_dir_is_not_misread_as_triple() {
        let l = layout_with_env("/ws/my-target/debug/deps/my_test", Some("my-target")).unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/my-target"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/ws/my-target/debug"));
    }

    #[test]
    fn triple_under_relative_target_dir_is_still_detected() {
        let l = layout_with_env(
            "/ws/my-target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            Some("my-target"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/my-target"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-pc-windows-gnu"));
        assert_eq!(
            l.profile_dir,
            Path::new("/ws/my-target/x86_64-pc-windows-gnu/debug")
        );
    }

    #[test]
    fn exe_outside_configured_target_dir_falls_back_to_heuristic() {
        let l = layout_with_env(
            "/ws/rust/target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            Some("/ws/other-target"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/rust/target"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-pc-windows-gnu"));
    }

    #[test]
    fn exe_not_directly_under_deps_is_rejected() {
        assert!(layout("/ws/rust/target/debug/my_test").is_none());
        assert!(layout("/ws/rust/target/debug/deps/nested/my_test").is_none());
    }

    #[test]
    fn triple_detection() {
        assert!(looks_like_triple(Path::new("x86_64-pc-windows-gnu")));
        assert!(looks_like_triple(Path::new("aarch64-apple-darwin")));
        assert!(!looks_like_triple(Path::new("target")));
        assert!(!looks_like_triple(Path::new("mytarget")));
        assert!(!looks_like_triple(Path::new("debug")));
    }

    #[test]
    fn profile_flag_maps_debug_to_test_and_keeps_other_names() {
        assert_eq!(profile_flag(Path::new("/ws/target/debug")), "test");
        assert_eq!(profile_flag(Path::new("/ws/target/release")), "release");
        assert_eq!(profile_flag(Path::new("/ws/target/ci")), "ci");
    }
}
