# example_bin Uplift Race Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/607

## Overview

Under `cargo nextest` on macOS, a test occasionally failed with ENOENT spawning
`target/debug/examples/fontaine_claude_cli_double`. Every test process runs a nested
`cargo build` in `fontaine_testkit::example_bin`, and cargo may re-uplift (remove and re-link)
`examples/<name>` while a sibling process holds that path and is about to spawn it. Return a
path that cargo never touches, so the race cannot occur.

## Current State

`rust/fontaine-testkit/src/lib.rs::example_bin` builds with
`--message-format=json-render-diagnostics`, takes `executable` from the matching
`compiler-artifact` message (`artifact_executable`), asserts `is_file()`, caches the path per
process and returns it. That path is the uplifted `examples/<name>`; the message's `filenames`
holds only that path (verified on Linux), so cargo reports no stable hashed alternative. Tests
spawn the returned path long after the build, so a sibling build re-uplifting it opens an ENOENT
window. Non-atomic uplift is unreproduced; the fix does not depend on it.

## Design

After a successful nested build, copy the executable to a path private to this process and
return that, so later spawns never touch a file cargo manages:

- Destination: `<profile_dir>/example-bin/<package>-<name>-<pid><EXE_SUFFIX>`. It sits under the
  target dir, so it shares the filesystem and `cargo clean` removes it. Files from earlier runs
  accumulate until then; that is accepted. Under nextest every test is its own process, so each
  run leaves one copy of each example per test process; that is accepted too.
- Concurrent first calls in one process for the same example would share that destination, so
  the cache becomes `HashMap<key, Arc<Mutex<Option<PathBuf>>>>`. The global lock is held only to
  get-or-insert the per-key entry. `example_bin` then locks that entry across build, copy and
  store; a later caller for the same key finds `Some` and returns it, so no thread writes to a
  file another thread may be spawning (also what keeps Windows from failing on a running exe).
  Other keys and cache hits never wait on a build. A build panic poisons only that entry, whose
  contents are still `None`, so the entry lock recovers poison with `PoisonError::into_inner`
  (one-line reason at the call site) and the next caller retries the build; the global lock
  keeps its `.expect` since nothing panics while holding it.
- `copy_private` creates `<profile_dir>/example-bin/` with `fs::create_dir_all` before copying.
- The copy itself can hit the window, so retry `fs::copy` on `NotFound` a bounded number of
  times (50 x 100 ms) and panic afterwards with the last error, naming both src and dest since
  `NotFound` does not say which was missing. Other error kinds fail at once.
- `fs::copy` preserves permission bits, so the executable bit survives.
- The cache stores the private path. The `is_file()` assertion moves to after the copy.
- The copy sits inside the retry loop; the retry loop is a small helper taking a closure, so it is unit-testable.

## Implementation Steps

1. In `rust/fontaine-testkit/src/lib.rs`, add `retry_not_found(attempts, delay, f)` and
   `copy_private(src, profile_dir, package, name)` (profile_dir comes from `layout.profile_dir`), which creates `<profile_dir>/example-bin/` with
   `fs::create_dir_all` before copying.
2. Call `copy_private` in `example_bin` in place of the bare `is_file()` assertion and cache
   the copy, holding the per-key entry lock (not the global lock) across build, copy and store.
3. Unit tests (add one: a build that panics leaves the key retryable, not poisoned): `retry_not_found` succeeds after N `NotFound` errors, fails after the cap, and
   does not retry other error kinds; `copy_private` copies content and permission bits into a
   temp dir.
4. Update the module doc (lib.rs lines 4-5, which say binaries are located under
   `target/<profile>/examples/`) and the `example_bin` doc: the returned path is a private copy, and concurrent first calls for the
   same example are serialized per key (the module doc says they are not deduplicated).
5. Spot-check with `cargo test -p fontaine --test claude_subscription`.
6. Add a prose entry under `## [Unreleased]` / `### Fixed` in `CHANGELOG.md`, citing #607.

## Files to Modify

- `rust/fontaine-testkit/src/lib.rs`
- `CHANGELOG.md`

## Trade-offs

- Retry-on-ENOENT at every spawn site: touches many call sites and only masks the window.
- Returning the hashed `deps/<name>-<hash>` file: cargo does not report it, and globbing finds
  stale hashes.
- CI pre-build plus skipping the nested build: changes CI and local workflows, and leaves the
  per-package `cargo test` stale-binary hazard the helper exists to prevent.
- Cross-process file lock around build and use: the lock would have to span the whole test.

## Documentation

`docs/architecture.md` describes `example_bin` as building on first use and returning its path;
that stays accurate. No other docs.

## Testing Strategy

Unit tests on the retry helper and the copy, as listed above. The race itself is
timing-dependent on macOS and cannot be reproduced deterministically; the fix removes its
precondition, since the returned path is never touched by cargo.

## Decisions

- AFK: 2026-11-13 per-key `Arc<Mutex<Option<PathBuf>>>` entries (global lock short, poison recovered on the entry) instead of holding the global cache lock across build+copy, avoiding poisoning and cross-key blocking while keeping one copy per key per process for Windows.
