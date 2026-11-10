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

/// The triple the outer build compiled this crate (and the test binaries linking it) for,
/// captured by the build script from Cargo's `TARGET`: exactly the outer build's target,
/// host or foreign, unlike a `rustc -vV` host lookup.
const BUILD_TARGET: &str = env!("FONTAINE_TESTKIT_TARGET");

/// Returns the path to the built example `name` of workspace package `package`, building it on
/// first use. Panics with cargo's combined output if the build fails.
pub fn example_bin(package: &str, name: &str) -> PathBuf {
    let key = (package.to_string(), name.to_string());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // The lock is only held for the cache get/insert. The build below panics on failure, and
    // holding the guard across it would poison the mutex, making unrelated tests that share
    // the process panic on their next `example_bin` call instead of building.
    if let Some(path) = cache
        .lock()
        .expect("fontaine-testkit cache mutex poisoned")
        .get(&key)
    {
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
    // The nested build uses default features: cargo does not export the outer invocation's
    // feature selection (no CARGO_FEATURE_* vars) or its default-feature state to the test
    // process, so a feature-matched build is impossible to request reliably. The outer
    // `cargo test --features ...` run compiles feature-selected example variants beside the
    // default ones; these examples have no feature-gated behavior, so the mismatch only
    // costs a redundant default-feature build.
    cmd.args(["build", "-p", package, "--example", name])
        .arg("--target-dir")
        .arg(&layout.target_dir)
        .arg("--profile")
        .arg(profile_flag(&layout.profile_dir))
        .arg("--message-format=json-render-diagnostics");
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
    // Ask the build where it put the executable instead of deriving it from the layout:
    // this stays correct for triple, custom target-dir, and profile combinations the path
    // guess gets wrong. The guessed location is only a fallback for a cargo that reports no
    // artifact message.
    let path = artifact_executable(&out.stdout, name).unwrap_or_else(|| {
        layout
            .profile_dir
            .join("examples")
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    });
    assert!(
        path.is_file(),
        "{} was not produced by the build",
        path.display()
    );
    cache
        .lock()
        .expect("fontaine-testkit cache mutex poisoned")
        .insert(key, path.clone());
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
        Self::from_exe_with(exe, cargo_target_dir, Some(BUILD_TARGET))
    }

    /// `build_target` is the outer build's target triple; injectable so the layout logic
    /// stays testable without relying on [`BUILD_TARGET`].
    fn from_exe_with(
        exe: &Path,
        cargo_target_dir: Option<&Path>,
        build_target: Option<&str>,
    ) -> Option<Layout> {
        let deps = exe.parent()?;
        if deps.file_name()? != "deps" {
            return None;
        }
        let profile_dir = deps.parent()?;
        let above = profile_dir.parent()?;
        // Cargo's configured target dir is authoritative when the exe descends from it:
        // anything between it and the profile dir is a triple, and the configured dir's
        // own name is part of the target dir even when it contains a `-`.
        let matched =
            cargo_target_dir.and_then(|c| configured_target_match(above, c, build_target));
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
            // Without a configured dir, a `-`-bearing segment may be a triple (an explicit
            // outer command-line `--target`, which cargo does not export to the test
            // process) or a hyphenated `--target-dir`/build config dir (cargo exports
            // neither of those either). The build script compiled the outer build's
            // target triple into this crate, so a segment equal to it is exactly the outer
            // build's target dir layout: forward it (correct for genuine `--target <host>`
            // and `--target <foreign>` runs alike, and harmless for a directory
            // coincidentally named after the build target). Any other segment — however
            // triple-like — is a target dir.
            None if looks_like_triple(above) => {
                let name = above.file_name()?.to_string_lossy().into_owned();
                if build_target == Some(name.as_str()) {
                    (Some(name), above.parent()?.to_path_buf())
                } else {
                    (None, above.to_path_buf())
                }
            }
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
/// the configured dir against that unknown base. A match is accepted only when the
/// remainder is empty or the build target: a CLI `--target-dir` may override
/// `CARGO_TARGET_DIR` into a subdirectory of it, and the overridden name (e.g. `host`) is
/// not a triple; anything else falls through to the unconfigured heuristic.
fn configured_target_match(
    above: &Path,
    configured: &Path,
    build_target: Option<&str>,
) -> Option<(PathBuf, PathBuf)> {
    if configured.is_absolute() {
        let rest = above.strip_prefix(configured).ok()?;
        let rest = rest.to_path_buf();
        if !valid_configured_rest(&rest, build_target) {
            return None;
        }
        return Some((configured.to_path_buf(), rest));
    }
    let suffix: Vec<_> = configured.components().collect();
    let comps: Vec<_> = above.components().collect();
    if suffix.is_empty() || suffix.len() > comps.len() {
        return None;
    }
    // The configured dir sits directly below the (unknown) base Cargo resolved it
    // against, so it is a contiguous run of `above`'s components with at most one
    // segment — the triple — after it. A match carrying the build target as that
    // segment is a real triple layout and wins over an empty-remainder match of the
    // same configured name (a base directory may itself end with it).
    let mut dir_only = None;
    for start in (0..=comps.len() - suffix.len()).rev() {
        if comps[start..start + suffix.len()] != suffix[..] {
            continue;
        }
        let rest: PathBuf = comps[start + suffix.len()..].iter().collect();
        if !valid_configured_rest(&rest, build_target) {
            continue;
        }
        let base: PathBuf = comps[..start].iter().collect();
        let matched = (base.join(configured), rest);
        if matched.1.components().next().is_some() {
            return Some(matched);
        }
        if dir_only.is_none() {
            dir_only = Some(matched);
        }
    }
    dir_only
}

/// The remainder between a matched configured dir and `above` is a triple segment only
/// when it is empty or equals the outer build's target triple.
fn valid_configured_rest(rest: &Path, build_target: Option<&str>) -> bool {
    match rest.components().next() {
        None => true,
        Some(_) => {
            rest.components().count() == 1 && build_target.is_some_and(|t| rest == Path::new(t))
        }
    }
}

/// Best-effort triple detection for the optional segment between the target dir and the
/// profile dir. This only gates validation: a `-`-bearing name is a triple candidate, and
/// comparison against [`BUILD_TARGET`] decides — only a segment equal to the outer build's
/// compiled-in target triple is treated as one. Any directory name with no `-` (including a
/// custom `CARGO_TARGET_DIR`) is not a triple.
fn looks_like_triple(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.contains('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Extracts the executable cargo built for example `name` from the stdout of a build run
/// with `--message-format=json-render-diagnostics`, or `None` when no artifact message
/// carries an `executable` for it. A dependency sharing the example's name is filtered out
/// by the `executable` field: only bin/example artifacts have one.
fn artifact_executable(stdout: &[u8], name: &str) -> Option<PathBuf> {
    let mut found = None;
    for line in stdout.split(|&b| b == b'\n') {
        let Ok(msg) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        if msg
            .pointer("/target/name")
            .and_then(serde_json::Value::as_str)
            != Some(name)
        {
            continue;
        }
        if let Some(exe) = msg.get("executable").and_then(serde_json::Value::as_str) {
            found = Some(PathBuf::from(exe));
        }
    }
    found
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

    fn layout_with_target(
        exe: &str,
        cargo_target_dir: Option<&str>,
        build_target: Option<&str>,
    ) -> Option<Layout> {
        Layout::from_exe_with(
            Path::new(exe),
            cargo_target_dir.map(Path::new),
            build_target,
        )
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
        let l = layout_with_target(
            "/ws/rust/target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            None,
            Some("x86_64-pc-windows-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/rust/target"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-pc-windows-gnu"));
        assert_eq!(
            l.profile_dir,
            Path::new("/ws/rust/target/x86_64-pc-windows-gnu/debug")
        );
    }

    #[test]
    fn exe_with_foreign_build_target_sets_triple_and_strips_it_from_target_dir() {
        // Regression: an outer `cargo test --target aarch64-unknown-linux-musl` (runnable
        // via a cross runner or compatible musl host) places the test exe under the
        // foreign triple, which the compiled-in build target now recognizes instead of
        // rejecting every non-host triple as a directory name.
        let l = layout_with_target(
            "/ws/rust/target/aarch64-unknown-linux-musl/debug/deps/my_test",
            None,
            Some("aarch64-unknown-linux-musl"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/rust/target"));
        assert_eq!(l.triple.as_deref(), Some("aarch64-unknown-linux-musl"));
        assert_eq!(
            l.profile_dir,
            Path::new("/ws/rust/target/aarch64-unknown-linux-musl/debug")
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
    fn hyphenated_outer_target_dir_without_env_is_not_misread_as_triple() {
        // Regression: an outer `cargo test --target-dir /tmp/fontaine-target` with no
        // CARGO_TARGET_DIR exported must not produce `--target fontaine-target`.
        let l = layout("/tmp/fontaine-target/debug/deps/my_test").unwrap();
        assert_eq!(l.target_dir, Path::new("/tmp/fontaine-target"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/tmp/fontaine-target/debug"));
    }

    #[test]
    fn non_matching_hyphenated_segment_is_a_target_dir_not_a_triple() {
        let l = layout_with_target(
            "/ws/fontaine-target/debug/deps/my_test",
            None,
            Some("x86_64-unknown-linux-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/fontaine-target"));
        assert_eq!(l.triple, None);
    }

    #[test]
    fn host_triple_segment_is_forwarded_as_triple() {
        let l = layout_with_target(
            "/ws/target/x86_64-unknown-linux-gnu/debug/deps/my_test",
            None,
            Some("x86_64-unknown-linux-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/target"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn foreign_triple_segment_is_forwarded_as_triple() {
        let l = layout_with_target(
            "/ws/target/aarch64-unknown-linux-musl/debug/deps/my_test",
            None,
            Some("aarch64-unknown-linux-musl"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/target"));
        assert_eq!(l.triple.as_deref(), Some("aarch64-unknown-linux-musl"));
    }

    #[test]
    fn foreign_triple_named_target_dir_is_not_misread_as_triple() {
        // Regression: an outer host build such as `cargo test --target-dir
        // /tmp/aarch64-apple-darwin` places the test exe under
        // /tmp/aarch64-apple-darwin/debug/deps/. The segment is a directory name: it does
        // not equal the outer build's compiled-in target triple.
        let l = layout_with_target(
            "/tmp/aarch64-apple-darwin/debug/deps/my_test",
            None,
            Some("x86_64-unknown-linux-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/tmp/aarch64-apple-darwin"));
        assert_eq!(l.triple, None);
    }

    #[test]
    fn unknown_build_target_falls_back_to_treating_segments_as_target_dirs() {
        let l = layout_with_target(
            "/ws/target/x86_64-pc-windows-gnu/debug/deps/my_test",
            None,
            None,
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/target/x86_64-pc-windows-gnu"));
        assert_eq!(l.triple, None);
    }

    #[test]
    fn artifact_executable_picks_the_named_examples_executable() {
        let stdout = br#"{"reason":"compiler-artifact","target":{"name":"hook_marker","kind":["example"]},"executable":"/t/debug/examples/hook_marker"}
{"reason":"compiler-artifact","target":{"name":"hook_marker","kind":["lib"]},"executable":null}
{"reason":"compiler-artifact","target":{"name":"other","kind":["example"]},"executable":"/t/debug/examples/other"}
"#;
        assert_eq!(
            artifact_executable(stdout, "hook_marker"),
            Some(PathBuf::from("/t/debug/examples/hook_marker"))
        );
    }

    #[test]
    fn artifact_executable_returns_none_without_a_matching_executable() {
        assert_eq!(artifact_executable(b"not json\n", "x"), None);
        assert_eq!(
            artifact_executable(b"{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"x\",\"kind\":[\"lib\"]},\"executable\":null}\n", "x"),
            None
        );
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
        let l = layout_with_target(
            "/ws/my-target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            Some("/ws/my-target"),
            Some("x86_64-pc-windows-gnu"),
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
        let l = layout_with_target(
            "/ws/my-target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            Some("my-target"),
            Some("x86_64-pc-windows-gnu"),
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
    fn cli_overridden_target_dir_under_env_dir_absolute_is_not_misread_as_triple() {
        // Regression: CARGO_TARGET_DIR=/tmp/base with an outer `cargo test --target-dir
        // /tmp/base/host` places the exe under /tmp/base/host/debug/deps. `host` is the
        // overridden directory name, not a triple: the configured match is rejected and
        // the fallback treats the whole overridden dir as the target dir rather than
        // forwarding `host` as `--target`.
        let l = layout_with_target(
            "/tmp/base/host/debug/deps/my_test",
            Some("/tmp/base"),
            Some("x86_64-unknown-linux-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/tmp/base/host"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/tmp/base/host/debug"));
    }

    #[test]
    fn cli_overridden_target_dir_under_env_dir_relative_is_not_misread_as_triple() {
        let l = layout_with_target(
            "/ws/base/host/debug/deps/my_test",
            Some("base"),
            Some("x86_64-unknown-linux-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/base/host"));
        assert_eq!(l.triple, None);
        assert_eq!(l.profile_dir, Path::new("/ws/base/host/debug"));
    }

    #[test]
    fn relative_match_prefers_target_bearing_over_dir_only() {
        // A base directory that itself ends with the configured name: the match carrying
        // the build target as its remainder is the real triple layout.
        let l = layout_with_target(
            "/x86_64-pc-windows-gnu/x86_64-pc-windows-gnu/debug/deps/my_test",
            Some("x86_64-pc-windows-gnu"),
            Some("x86_64-pc-windows-gnu"),
        )
        .unwrap();
        assert_eq!(l.target_dir, Path::new("/x86_64-pc-windows-gnu"));
        assert_eq!(l.triple.as_deref(), Some("x86_64-pc-windows-gnu"));
    }

    #[test]
    fn exe_outside_configured_target_dir_falls_back_to_heuristic() {
        let l = layout_with_target(
            "/ws/rust/target/x86_64-pc-windows-gnu/debug/deps/my_test.exe",
            Some("/ws/other-target"),
            Some("x86_64-pc-windows-gnu"),
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
