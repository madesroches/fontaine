# Fix "Text file busy" flake in fake-runtime test scripts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/506

## Overview

`managed_search::tests::a_running_container_that_never_answers_is_removed_and_replaced` fails
intermittently on Linux with `ExecutableFileBusy` (ETXTBSY) when the `fontaine-core` lib suite runs
in parallel. The test helper writes a fake container CLI script and the code under test then
executes it. If another test thread forks while the helper's write descriptor is open, the child
inherits that descriptor until its own `exec`; an `exec` of the script in that window fails with
ETXTBSY. The fix is test-side only: after writing the script, the helper waits until the script is
exec-able before returning.

## Current State

- `rust/fontaine-core/src/managed_search.rs:441` `fake_runtime` does `fs::write` + `set_permissions`
  and returns; `scripted_runtime` (line 596) builds on it, and every `prepare` test then spawns the
  script.
- `rust/fontaine-core/tests/websearch_runtime.rs:139` has an identical `fake_runtime` copy with the
  same exposure (integration tests there also fork `sleep`/scripts).
- `rust/fontaine-core/tests/websearch_runtime.rs:60-69` writes `start.sh` with `common::write` +
  `set_permissions`, and the websearch config spawns it in the same parallel binary: same exposure.
- `rust/fontaine-core/src/bug_submit.rs:273-280` `fake_gh` writes a `gh` script the tests exec, in
  the same lib test binary as `managed_search::tests`: same exposure.
- Precedent: `rust/fontaine-guardian/tests/scanner.rs:121` retries the scan on "Text file busy".
  That works because the scan is the call under test; here the spawn is deep inside production
  code (`prepare` → `RuntimeCli`), so a retry at the call site would re-run `prepare` with
  possibly partial side effects.

## Design

The helper makes the file safe to exec before returning, so no production code and no call site
changes:

1. Write the script with a guard line after the shebang:
   `[ -z "$FONTAINE_TEST_PROBE" ] || exit 0`.
2. `chmod 0755`.
3. Probe: run the script with `FONTAINE_TEST_PROBE=1`, retrying every few ms on
   `io::ErrorKind::ExecutableFileBusy` (bounded, e.g. 500 × 10 ms, then panic with the error). Any other spawn error, or a nonzero probe
   exit, panics immediately with the error.
   The guard makes the probe side-effect free, so scripts that log to a state file are unaffected.

Why the probe is sufficient: ETXTBSY needs a live writable descriptor to the inode. Once the
helper's descriptor is closed, the only holders are forked children that have not yet exec'd; they
are transient and no new ones can appear (no writer remains to inherit). The first successful probe
exec proves they are gone.

Helper shape: `write_executable(path: &Path, body: &str)` doing steps 1-3 (the shebang and guard
are prepended; callers pass the body). `fake_runtime` in both files, `fake_gh` and the `start.sh`
write call it. `fake` in
`managed_search.rs` (empty `#!/bin/sh`, only found on PATH, never executed) is left alone.

Placement: lib unit tests cannot see `tests/common`, and `managed_search::tests` and
`bug_submit::tests` both need the helper, so the lib side gets one crate-level
`#[cfg(test)] #[cfg(unix)] mod test_support` (new file `rust/fontaine-core/src/test_support.rs`,
declared in `lib.rs`) with `pub(crate) fn write_executable`. `tests/common/mod.rs` keeps its own
copy of the ~15-line helper (integration tests cannot see the lib's `cfg(test)` items).

## Implementation Steps

1. `rust/fontaine-core/src/test_support.rs` (new, `#[cfg(all(test, unix))]` module declared in
   `lib.rs`): add `pub(crate) fn write_executable` as above, with its unit test.
   `managed_search.rs` tests: make `fake_runtime` call it. `bug_submit.rs` tests: make `fake_gh`
   call it (its `script` argument becomes the body).
2. `rust/fontaine-core/tests/common/mod.rs`: add `#[cfg(unix)] pub fn write_executable(path, body)`
   with the same logic.
3. `rust/fontaine-core/tests/websearch_runtime.rs`: replace the body of `fake_runtime` with a call
   to `common::write_executable`, and replace the `start.sh` `common::write` + `set_permissions`
   (lines 60-69) with `common::write_executable`. Other `set_permissions(0o755)` sites in that file and
   `tests/config.rs`/`src/bug_submit.rs` that write executables are out of scope unless they execute
   the script under parallel load; note any found, do not widen the change.
4. `CHANGELOG.md`: no entry (test-only change, no user-visible behavior).

## Files to Modify

- `rust/fontaine-core/src/test_support.rs` (new, test-only)
- `rust/fontaine-core/src/lib.rs` (declare the `#[cfg(all(test, unix))]` module)
- `rust/fontaine-core/src/managed_search.rs` (tests only)
- `rust/fontaine-core/src/bug_submit.rs` (tests only)
- `rust/fontaine-core/tests/common/mod.rs`
- `rust/fontaine-core/tests/websearch_runtime.rs`

## Trade-offs

- Retry spawn in production code on `ExecutableFileBusy`: rejected; ETXTBSY cannot occur for a
  real runtime binary, so production code would carry a test-only workaround.
- Retry the whole `prepare` in the test: rejected; re-running after a partial run can double
  side effects (`rms`/`runs` logs) and masks real failures.
- Serialize script-writing tests with a mutex: rejected; it does not stop an unrelated test's
  `fork` from inheriting the descriptor.
- Write to a temp name then rename: rejected; the descriptor refers to the inode, rename does not
  release another process's copy of it.

## Decisions

- 2026-11-08: keep `a_running_container_that_never_answers_is_removed_and_replaced`; it is the only
  test reaching the starting-container deadline expiry (`managed_search.rs:357-361`); `exited`
  covers Unhealthy→`rm` and `answers later` covers reuse.
- 2026-11-08: the helper exists twice, a crate-level `#[cfg(test)]` module for the lib unit tests and
  `tests/common` for integration tests; no test-support crate for one helper.
- AFK: `websearch_runtime.rs` `start.sh` switches to `common::write_executable` (verified: same
  parallel binary spawns it, same ETXTBSY exposure as `fake_runtime`).
- AFK: `bug_submit::tests::fake_gh` is fixed too, via a shared crate-level `#[cfg(test)]` helper
  module also used by `managed_search::tests` (verified: both run in the lib test binary and exec the
  script); chosen over leaving bug_submit exposed as the conservative way to fully close the flake.

## Documentation

None; no docs describe test helpers.

## Testing Strategy

- The race is probabilistic and cannot be pinned by a deterministic unit test. Verification:
  run `cargo test -p fontaine-core --lib managed_search` and `--test websearch_runtime` repeatedly
  (e.g. 50 iterations, and once with `--test-threads` high) after a forced rebuild of the test
  binary, since the failures appeared on the first run after a rebuild. Expect zero failures.
- A small `#[cfg(unix)]` unit test for `write_executable`: the written script, executed normally (no probe var),
  runs its body; executed with the probe var set, exits 0 without running it. This pins that the
  probe is side-effect free.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p fontaine-core`.

## Open Questions

None.
