# Cut CI Runtime Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/652

## Overview

Per-PR CI takes ~36 min on Linux and ~43 min on Windows, long enough that healthy runs look hung.
The single slowest step is `Test (pg2)` on Linux (15–21 min: `cargo clean`, then a full candle
rebuild and test run). The issue's own decision keeps the pg2 code but drops its tests from PR CI,
runs them nightly and on `main`, and adds a nextest hang guard and a dependency cache. Target (user-set): every
OS's PR check completes within 20 min of wall-clock latency.

## Current State

`.github/workflows/ci.yml`, job `check` (matrix ubuntu/macos/windows):

- Linux-only pg2 steps: `Clippy (pg2)`, `Rustdoc (pg2)` (commented "Before Test (pg2), which cleans
  the target dir"), and `Test (pg2)` = `cargo clean && cargo test --features fontaine/pg2`.
- macOS/Windows run `Check (pg2)` (`cargo check --features fontaine/pg2`).
- No `Swatinem/rust-cache` or sccache step exists in the workflow, so every run compiles the whole
  dependency tree. (The master plan's mention of a restore-only cache is not reflected in `ci.yml`.)
- `cargo nextest run` has no config file: no slow-timeout, so a hang is only stopped by the 6 h job
  limit.
- `Build tests` (`cargo test --no-run`) and `Test` (`cargo nextest run`) are separate steps over the
  same build; `Doctests` runs `cargo test --doc` separately.
- `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0` are set because the Linux runner's disk
  cannot hold the ~140 integration-test binaries with debug info; the pg2 `cargo clean` exists for
  the same reason.
- `ci.yml` triggers on `push: [main]` and `pull_request`. `dev.yml` runs on a successful `CI`
  `workflow_run` for `main`, so a failing `main` CI blocks dev-channel publication.

## Design

1. **Move the pg2 test run out of the PR job** into a new workflow `.github/workflows/pg2.yml`,
   triggered by `schedule` (nightly), `workflow_dispatch`, and `push` to `main` paths that touch the
   pg2 surface (`rust/fontaine-guardian/**`, `rust/Cargo.lock`, `rust/Cargo.toml`, the workflow
   itself). It does not run as part of `CI`, so a pg2 failure cannot block the dev channel
   (`dev.yml` follows `CI` only). Steps: free disk, install toolchain, `cargo test --features
   fontaine/pg2` (no `cargo clean` needed: the fresh runner has no default-feature build to
   displace). Release builds already enable `pg2` (`dist` config), so a release still compiles it;
   the release gate for pg2 tests is a green latest nightly, noted in CONTRIBUTING.
2. **Keep the cheap pg2 checks on PRs** (`Clippy (pg2)`, `Rustdoc (pg2)`, and `Check (pg2)` on
   macOS/Windows) so the feature keeps compiling. Remove the "Before Test (pg2)" comment. Decision
   on moving them out is data-driven: see Implementation step 5.
3. **Nextest hang guard**: add `rust/.config/nextest.toml` with `slow-timeout = { period = "60s",
   terminate-after = 5 }` in the default profile (a test running 5 min is killed and named), with an
   `ci` profile only if the default must differ. Slow tests already known to exceed this get a
   per-test override found from step 5's measurement.
4. **Dependency cache**: add `Swatinem/rust-cache@v2` (workspace `rust`) to `check` and `sdk-compat`,
   saving only on `main` pushes (`save-if: ${{ github.ref == 'refs/heads/main' }}`) and restoring on
   PRs, for all three OSes. It caches dependencies, not workspace crates, which keeps the cache
   within runner disk on Linux.
5. **Fit the 20 min budget**: record per-step durations before and after from the run summary
   (`gh run view <id> --json jobs`) for all three OSes. Two structural changes are in scope
   regardless of the first measurement: (a) drop `Build tests` (`cargo test --no-run`); nextest
   compiles what it needs, and the step only gave a clearer failure label; (b) split work that does
   not share a build into parallel jobs: `deny` (needs only the manifest, no compile), `docs`
   (rustdoc + pg2 rustdoc), and the Linux-only `Release signing` plus eval fixture runs, so latency is
   the slowest job rather than the sum. Required check names are kept for the existing `check` job
   matrix (`ubuntu-latest`, `macos-latest`, `windows-latest`); new jobs need adding to branch
   protection by the maintainer. If Windows is still over 20 min after this, trim its step set to
   what Linux does not already cover (the Linux job covers eval fixtures and doc builds); that
   deviation is recorded in Decisions.

## Implementation Steps

1. Add `.github/workflows/pg2.yml` (schedule, dispatch, path-filtered main push) running the pg2
   test with the same disk-freeing and toolchain steps as `ci.yml`.
2. In `ci.yml`: delete the `Test (pg2)` step and the "Before Test (pg2)" comment.
3. In `ci.yml`: add the rust-cache step to `check` and `sdk-compat`.
4. Add `rust/.config/nextest.toml` with the slow-timeout.
5. Drop `Build tests` from `check`; move `deny`, rustdoc (both) and, on Linux, `Release signing`
   and the eval fixture runs into parallel jobs. Push the branch, compare job durations to the
   20 min target and the issue baseline, and record the numbers in the PR description.
6. Update docs and the master plan (below).

## Files to Modify

- `.github/workflows/ci.yml`
- `.github/workflows/pg2.yml` (new)
- `rust/.config/nextest.toml` (new)
- `CONTRIBUTING.md` (CI paragraph, ~lines 29–33)
- `tasks/fontaine_harness_plan.md` (workspace layout CI paragraph at ~line 205, and a Decisions line)
- `CHANGELOG.md` if it tracks CI changes

## Trade-offs

- **Drop pg2 tests from PRs vs. a separate cache for pg2**: a separate cache still costs a candle
  build on every cache miss and doubles cache storage; the scanner is opt-in and unused by the
  maintainer, so nightly coverage is enough (the issue's decision).
- **Nightly workflow vs. a job in `ci.yml` gated on `main`**: a separate workflow cannot fail `CI`
  and therefore cannot block the dev channel.
- **rust-cache vs. sccache**: rust-cache needs no extra service or secrets and is a one-step change.

## Decisions

- 2026-11-14 (user): the latency target is 20 min per PR run on every OS.
- 2026-11-14: pg2 tests leave per-PR CI; pg2 clippy, rustdoc and check stay. (Issue #652 decision.)

## Documentation

- `CONTRIBUTING.md`: CI now runs pg2 tests nightly and on relevant `main` pushes, not per PR.
- Master plan: update the `ci.yml` description and add a Build and release Decisions line that
  narrows the 2026-10-25 and 2026-11-04 lines (pg2 tests off PRs; no `cargo clean` any more).

## Testing Strategy

Workflow YAML cannot be unit tested. Verification is the PR's own CI run: all required checks stay
green and the step timings are compared against the issue baseline. `cargo nextest run` locally
confirms the nextest config parses (`cargo nextest show-config test-groups` or a plain run). The
pg2 workflow is exercised by `workflow_dispatch` after merge.

## Manual Verification

1. After merge, trigger `gh workflow run pg2.yml` and confirm it completes green: the schedule and
   dispatch triggers only exist on the default branch, so this cannot run on the PR.
2. Confirm a cache is saved by the first `main` push and restored by the next PR run (cache step
   log shows a hit); cache behavior across branches is not reproducible locally.

## Open Questions

None.
