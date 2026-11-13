//! Dev-only test helpers shared by the workspace's integration tests.
//!
//! Integration tests spawn Cargo example binaries (`hook_marker`, `repo_mcp_server`, ...) as
//! helper processes, from a private copy of the binary cargo built under
//! `target/<profile>/examples/`. A workspace-wide
//! `cargo test` builds every member's examples, but a per-package run builds only that
//! package's targets and would silently use whatever example binaries happen to be on disk;
//! stale ones fail with timeouts far from the cause.
//!
//! [`example_bin`] builds the requested example on first use (a no-op under the workspace
//! run, since cargo's own freshness check is exact) and returns the path of a copy private to
//! this process. Cargo re-links `examples/<name>` on every nested build, so a sibling test
//! process's build could remove it between this process's build and its spawn. Concurrent
//! first calls for the same example are serialized per example, so the build and the copy
//! happen once per process.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

type Key = (String, String);
type Slot = Arc<Mutex<Option<PathBuf>>>;

/// One entry per example, so a build holds only its own lock: other examples and cache hits
/// never wait on it.
static CACHE: OnceLock<Mutex<HashMap<Key, Slot>>> = OnceLock::new();

/// The triple the outer build compiled this crate (and the test binaries linking it) for,
/// captured by the build script from Cargo's `TARGET`: exactly the outer build's target,
/// host or foreign, unlike a `rustc -vV` host lookup.
const BUILD_TARGET: &str = env!("FONTAINE_TESTKIT_TARGET");

/// The rustc the outer build used, captured by the build script from Cargo's `RUSTC`.
const BUILD_RUSTC: &str = env!("FONTAINE_TESTKIT_RUSTC");

static BUILTIN_TARGETS: OnceLock<Vec<String>> = OnceLock::new();

/// Returns the path to a private copy of the built example `name` of workspace package
/// `package`, which cargo never touches. The first call builds and copies it; concurrent first
/// calls for the same example wait for that one and share its path. Panics with cargo's
/// combined output if the build fails. Panics early when the outer build targeted a
/// custom JSON specification that cannot be resolved back to its file: cargo exports the
/// target to the test process only as the spec file's stem, which resolves only against
/// a spec file in this crate's directory or on RUST_TARGET_PATH.
pub fn example_bin(package: &str, name: &str) -> PathBuf {
    let key = (package.to_string(), name.to_string());
    let entry = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("fontaine-testkit cache mutex poisoned")
        .entry(key)
        .or_default()
        .clone();
    fill_slot(&entry, || build_and_copy(package, name))
}

/// Fills the slot with `build()` once; later callers get the stored path.
fn fill_slot(entry: &Slot, build: impl FnOnce() -> PathBuf) -> PathBuf {
    // A build that panics poisons only this entry, which is still `None`, so the next caller
    // retries the build instead of failing on the poison.
    let mut slot = entry.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = slot.as_ref() {
        return path.clone();
    }
    let path = build();
    *slot = Some(path.clone());
    path
}

fn build_and_copy(package: &str, name: &str) -> PathBuf {
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
        // example where this helper does not look. A custom JSON spec is forwarded by
        // absolute path so the nested build need not resolve the stem itself.
        let target = assert_nested_target_resolvable(triple, Path::new(env!("CARGO_MANIFEST_DIR")));
        cmd.arg("--target")
            .arg(target.as_deref().unwrap_or_else(|| Path::new(triple)));
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
    copy_private(&path, &layout.profile_dir, package, name)
}

/// Copies the built executable to a path only this process uses. The copy sits under the
/// target dir so it shares the filesystem and `cargo clean` removes it.
fn copy_private(src: &Path, profile_dir: &Path, package: &str, name: &str) -> PathBuf {
    let dir = profile_dir.join("example-bin");
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    let dest = dir.join(format!(
        "{package}-{name}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    // The source can be missing for an instant while a sibling build re-links it, and
    // `NotFound` does not say whether the source or the destination directory was missing.
    // The destination is removed first so the copy lands on a fresh inode: reusing a pid would
    // otherwise truncate and rewrite a file a previous run executed, which macOS kills
    // (code-signing cache) and Linux can refuse with ETXTBSY.
    retry_not_found(50, Duration::from_millis(100), || {
        match std::fs::remove_file(&dest) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::copy(src, &dest).map(|_| ())
    })
    .unwrap_or_else(|e| {
        if !src.is_file() {
            panic!("{} was not produced by the build", src.display());
        }
        panic!("cannot copy {} to {}: {e}", src.display(), dest.display())
    });
    dest
}

/// Runs `f` up to `attempts` times, sleeping `delay` between tries, while it fails with
/// `NotFound`; any other error returns at once.
fn retry_not_found<F>(attempts: u32, delay: Duration, mut f: F) -> std::io::Result<()>
where
    F: FnMut() -> std::io::Result<()>,
{
    let mut tries = 1;
    loop {
        match f() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && tries < attempts => {
                tries += 1;
                std::thread::sleep(delay);
            }
            other => return other,
        }
    }
}

/// The target triples built into the toolchain, from `rustc --print target-list`,
/// cached for the process. An empty list means the lookup failed; the nested build uses
/// the same rustc and fails for the same reason, so the resolvability check below then
/// relies on spec files alone.
fn builtin_targets() -> &'static [String] {
    BUILTIN_TARGETS.get_or_init(|| {
        Command::new(BUILD_RUSTC)
            .args(["--print", "target-list"])
            .output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Determines the `--target` value forwarded to the nested build: `None` for a built-in
/// target triple (the triple name itself is forwarded), or the canonical absolute path
/// of a custom JSON spec located in the nested build's working directory (`package_dir`)
/// or on RUST_TARGET_PATH. Panics when a non-built-in stem resolves to no spec file:
/// cargo exports an outer `cargo test --target <spec>.json` to the test process only as
/// the spec file's stem, and rejecting an unresolvable stem here keeps the error
/// actionable; otherwise the nested build fails with cargo's opaque target-not-found
/// output.
///
/// RUST_TARGET_PATH entries are resolved against the test process's working directory —
/// where the outer cargo read them — and the spec is forwarded by absolute path, because
/// the nested build runs from `package_dir` and would resolve relative entries against
/// that instead. Forwarding the absolute path also drops the nested build's dependence
/// on RUST_TARGET_PATH entirely.
///
/// A custom spec must therefore not reuse a built-in target triple's filename stem (a
/// `x86_64-unknown-linux-gnu.json` under `--target` is indistinguishable from a real
/// built-in `x86_64-unknown-linux-gnu` run): the built-in check returns early and the
/// nested build resolves the stem to the built-in spec, silently building for the wrong
/// target. The stem-only export makes this case mechanically undetectable in-process, so
/// it is rejected by convention rather than here.
fn assert_nested_target_resolvable(triple: &str, package_dir: &Path) -> Option<PathBuf> {
    resolve_nested_target(
        triple,
        package_dir,
        std::env::var_os("RUST_TARGET_PATH").as_deref(),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    )
}

/// `rust_target_path` is the RUST_TARGET_PATH value the test process inherited from the
/// outer cargo invocation, and `cwd` is the test process's working directory, the base
/// relative entries resolve against. Injectable so the resolution logic stays testable
/// without mutating process-global environment.
fn resolve_nested_target(
    triple: &str,
    package_dir: &Path,
    rust_target_path: Option<&OsStr>,
    cwd: &Path,
) -> Option<PathBuf> {
    if builtin_targets().iter().any(|t| t == triple) {
        return None;
    }
    let spec = Path::new(triple);
    let spec_json = PathBuf::from(format!("{triple}.json"));
    let locate = |dir: &Path| -> Option<PathBuf> {
        [spec, spec_json.as_path()]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
    };
    let found = locate(package_dir).or_else(|| {
        rust_target_path.and_then(|paths| {
            std::env::split_paths(paths)
                .map(|dir| {
                    if dir.is_absolute() {
                        dir
                    } else {
                        cwd.join(dir)
                    }
                })
                .find_map(|dir| locate(&dir))
        })
    });
    let found = found.unwrap_or_else(|| {
        panic!(
            "custom JSON target spec `{triple}` cannot be resolved to a spec file: cargo \
             exports the outer `cargo test --target <spec>.json` to the test process only \
             as the file stem. Run the tests with a built-in target triple, place the spec \
             in {}, or add its directory to RUST_TARGET_PATH as an absolute path.",
            package_dir.display()
        )
    });
    Some(found.canonicalize().unwrap_or(found))
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
            // Without a configured dir, the build script compiled the outer build's
            // target triple into this crate, so a segment equal to it is exactly the
            // outer build's target dir layout: the triple is forwarded for genuine
            // `--target <host>` and `--target <foreign>` runs. For a custom JSON target
            // cargo uses the file stem — e.g. `foo` for `foo.json` — as both the output
            // segment and the target name, so it lands here too, where example_bin
            // resolves the stem back to the spec file and forwards its absolute path
            // (see assert_nested_target_resolvable). Any other segment — however
            // triple-like — is a
            // `--target-dir`/build config dir cargo does not export to the test
            // process, and stays the target dir.
            None => {
                let name = above.file_name()?.to_string_lossy().into_owned();
                if build_target == Some(name.as_str()) {
                    (Some(name), above.parent()?.to_path_buf())
                } else {
                    (None, above.to_path_buf())
                }
            }
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
    fn custom_json_target_stem_is_detected_but_rejected_before_forwarding() {
        // An outer `cargo test --target foo.json` places the build under the file stem
        // `foo` and compiles the build target in as `foo`; the layout detects the segment
        // as the outer target's dir, and example_bin must not forward the bare stem: no
        // spec file named `fontaine-custom-json-stem[.json]` exists in the crate dir or
        // on RUST_TARGET_PATH, so the nested build could not resolve it.
        let l = layout_with_target("/ws/target/foo/debug/deps/my_test", None, Some("foo")).unwrap();
        assert_eq!(l.target_dir, Path::new("/ws/target"));
        assert_eq!(l.triple.as_deref(), Some("foo"));
        assert_eq!(l.profile_dir, Path::new("/ws/target/foo/debug"));
        std::panic::catch_unwind(|| {
            resolve_nested_target(
                "fontaine-custom-json-stem",
                Path::new("/nonexistent"),
                None,
                Path::new("/nonexistent"),
            )
        })
        .expect_err("an unresolvable custom JSON target stem must be rejected, not forwarded");
    }

    #[test]
    fn custom_spec_in_package_dir_resolves_to_absolute_path() {
        let dir = unique_temp_dir("package-dir-spec");
        std::fs::write(dir.join("fontaine-test-spec.json"), "{}").unwrap();
        let resolved = resolve_nested_target(
            "fontaine-test-spec",
            &dir,
            None,
            Path::new("/nonexistent-cwd"),
        )
        .expect("a spec in the nested build's working directory must resolve");
        assert!(resolved.is_absolute());
        assert_eq!(
            resolved,
            dir.canonicalize().unwrap().join("fontaine-test-spec.json")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_rust_target_path_entry_resolves_against_test_cwd() {
        // Regression: the outer cargo reads relative RUST_TARGET_PATH entries against the
        // test process's cwd, but the nested build runs from the package dir. The spec
        // must be located against the test cwd and forwarded by absolute path.
        let dir = unique_temp_dir("relative-rtp");
        let specs = dir.join("specs");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(specs.join("fontaine-test-spec.json"), "{}").unwrap();
        let resolved = resolve_nested_target(
            "fontaine-test-spec",
            Path::new("/nonexistent-package-dir"),
            Some(OsStr::new("specs")),
            &dir,
        )
        .expect("a spec behind a relative RUST_TARGET_PATH entry must resolve");
        assert!(resolved.is_absolute());
        assert_eq!(
            resolved,
            specs
                .canonicalize()
                .unwrap()
                .join("fontaine-test-spec.json")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absolute_rust_target_path_entry_resolves() {
        let dir = unique_temp_dir("absolute-rtp");
        std::fs::write(dir.join("fontaine-test-spec.json"), "{}").unwrap();
        let resolved = resolve_nested_target(
            "fontaine-test-spec",
            Path::new("/nonexistent-package-dir"),
            Some(dir.as_os_str()),
            Path::new("/nonexistent-cwd"),
        )
        .expect("a spec behind an absolute RUST_TARGET_PATH entry must resolve");
        assert_eq!(
            resolved,
            dir.canonicalize().unwrap().join("fontaine-test-spec.json")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fontaine-testkit-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builtin_build_target_passes_the_resolvability_check() {
        assert_eq!(
            assert_nested_target_resolvable(BUILD_TARGET, Path::new(env!("CARGO_MANIFEST_DIR"))),
            None
        );
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
    fn profile_flag_maps_debug_to_test_and_keeps_other_names() {
        assert_eq!(profile_flag(Path::new("/ws/target/debug")), "test");
        assert_eq!(profile_flag(Path::new("/ws/target/release")), "release");
        assert_eq!(profile_flag(Path::new("/ws/target/ci")), "ci");
    }

    #[test]
    fn retry_not_found_succeeds_after_transient_not_found() {
        let mut calls = 0;
        let r = retry_not_found(5, Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Err(std::io::ErrorKind::NotFound.into())
            } else {
                Ok(())
            }
        });
        assert!(r.is_ok());
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_not_found_gives_up_at_the_cap() {
        let mut calls = 0;
        let r = retry_not_found(4, Duration::ZERO, || {
            calls += 1;
            Err(std::io::ErrorKind::NotFound.into())
        });
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert_eq!(calls, 4);
    }

    #[test]
    fn retry_not_found_does_not_retry_other_errors() {
        let mut calls = 0;
        let r = retry_not_found(5, Duration::ZERO, || {
            calls += 1;
            Err(std::io::ErrorKind::PermissionDenied.into())
        });
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
    }

    #[test]
    fn copy_private_copies_content_and_permissions() {
        let dir = unique_temp_dir("copy-private");
        let src = dir.join("src-bin");
        std::fs::write(&src, b"payload").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&src).unwrap().permissions();
            p.set_mode(0o751);
            std::fs::set_permissions(&src, p).unwrap();
        }
        let dest = copy_private(&src, &dir.join("profile"), "pkg", "ex");
        assert!(dest.starts_with(dir.join("profile").join("example-bin")));
        assert_eq!(std::fs::read(&dest).unwrap(), b"payload");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o751);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panicking_build_leaves_the_key_retryable() {
        let entry: Slot = Arc::default();
        let e2 = entry.clone();
        let r = std::thread::spawn(move || fill_slot(&e2, || panic!("build failed"))).join();
        assert!(r.is_err());
        assert_eq!(
            fill_slot(&entry, || PathBuf::from("/built")),
            Path::new("/built")
        );
        assert_eq!(
            fill_slot(&entry, || panic!("built twice")),
            Path::new("/built")
        );
    }
}
