# Cut CI Runtime Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/652

## Overview

Per-PR CI takes ~36 min on Linux and ~43 min on Windows, long enough that healthy runs look hung.
The single slowest step is `Test (pg2)` on Linux (15–21 min: `cargo clean`, then a full candle
rebuild and test run). The issue's own decision keeps the pg2 code but drops its tests from PR CI,
runs them nightly, and adds a nextest hang guard. A dependency cache is only an experiment (see Design 4). Target (user-set): every
OS's PR check completes within 20 min of wall-clock latency.

## Current State

`.github/workflows/ci.yml`, job `check` (matrix ubuntu/macos/windows):

- Linux-only pg2 steps: `Clippy (pg2)`, `Rustdoc (pg2)` (commented "Before Test (pg2), which cleans
  the target dir"), and `Test (pg2)` = `cargo clean && cargo test --features fontaine/pg2`.
- macOS/Windows run `Check (pg2)` (`cargo check --features fontaine/pg2`).
- No `Swatinem/rust-cache` or sccache step exists in the workflow, so every run compiles the whole
  dependency tree. This is deliberate: PR #180 measured cold runs beating `rust-cache` on every OS
  (Windows 11m29 cold vs 14m38 cached, macOS 7m54 vs 11m32, Ubuntu 10m27 vs 11m43) and removed it.
- After #180 Windows ran 17m22; it is now ~43 min, so something other than the missing cache grew.
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
   triggered by `schedule` (nightly) and `workflow_dispatch` only (no path-filtered push trigger: a
   filter could miss part of the pg2 surface). It does not run as part of `CI`, so a pg2 failure
   cannot block the dev channel (`dev.yml` follows `CI` only). Steps: the same job env block as
   `ci.yml` (`CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`), free disk, install toolchain,
   `cargo test --features fontaine/pg2` (no `cargo clean` needed: the fresh runner has no
   default-feature build to displace). Release builds already enable `pg2` (`dist` config), so a
   release still compiles it; the release gate for pg2 tests is a manual check that the latest
   nightly is green, documented as a readiness step in CONTRIBUTING `## Releasing` and not
   enforced in `release.yml`. The job sets `timeout-minutes` (e.g. 60) as its hang guard, since it
   runs `cargo test`, not nextest, and `defaults.run.working-directory: rust`.
2. **Keep `Clippy (pg2)`, `Rustdoc (pg2)` and `Check (pg2)` on PRs**; remove the "Before Test (pg2)" comment.
3. **Nextest hang guard**: add `rust/.config/nextest.toml` with `slow-timeout = { period = "60s",
   terminate-after = 10 }` in the default profile (a test running 10 min is killed and named), with a
   `ci` profile only if the default must differ. Per-test overrides are added only if the PR run
   kills a legitimate test.
4. **Dependency cache is an experiment, off by default.** PR #180 found cold runs faster than
   `rust-cache` on every OS, so none is added unconditionally. On the PR, measure each OS with and
   without `Swatinem/rust-cache@v2` (workspace `rust`). During the experiment use `save-if: true`
   (a PR run with `save-if` main-only never saves, so the with-cache leg would always be a cold
   miss) and measure two runs per OS: the first seeds the cache, the second restores it. Keep it
   only on an OS where the restore run (a run where the cache hit) is at least 3 minutes faster than the
   no-cache run on that OS, otherwise leave it off; for any
   cache that is kept, set `save-if` to main-only. Any kept cache must be sized against the 10 GB
   per-repo cap (the three OSes share it).
5. **Fit the 20 min budget**: first root-cause, then choose levers. Take a per-step breakdown of a
   current Windows run (`gh run view <id> --json jobs`) to find what grew from 17m22 after #180 to
   ~43 min, and do the same for macOS and Linux. Removing the pg2 test step only helps Linux, so it
   does not by itself bring Windows or macOS to 20 min. The job stays a single `check` matrix job
   (no split of `deny`, rustdoc, signing or evals into new jobs), which keeps cross-OS coverage and
   the required-check set unchanged. If an OS is still over 20 min, apply only levers the
   measurement points at, each recorded in Decisions; dropping any cross-OS step also needs a
   Decisions line narrowing the 2026-10-21 "every CI step runs on all three OSes" line. If any OS
   is still over 20 min after pg2 removal and the measured levers, the PR description reports the
   miss and the remaining cause rather than shipping silently.

6. **Fix the slow tests, not the timeout.** Measured locally with nextest 0.9.149 (13 min for the full
   run, 80 tests flagged slow, none close to the 10 min guard): the slow tests cluster in
   `fontaine::claude_subscription` (12, over 60s), `fontaine-core::claude_subscription` (11, over
   60s), `fontaine-tui::agents` (9), `fontaine-tui::hooks` (3) and `fontaine-core::afk` (1). The
   `fontaine-tui` ones take about 60s, 120s or 180s, i.e. multiples of 60s, and a lone
   `a_server_in_the_users_own_settings_starts_without_asking` takes 60.3s although its body is a
   400 ms sleep plus a wait for the idle prompt, so each `World::run` seems to wait out a fixed
   60s. Ruled out: the nested `cargo build` in `fontaine-testkit::example_bin` (60s with the
   example already built), the session-start file index (the workspace is a tempdir), and the
   `World` wait loops (capped at 10s). Not yet found: what the 60s waits on (candidates: a 60s
   constant on the teardown path of `drive`, MCP/hook shutdown waiting on `touch_marker`, which
   never speaks MCP and sleeps 300 ms). Find it by sampling a running test (`strace -f` or
   `/proc/<pid>/task/*/stack`), fix the cause in the code or the harness, then repeat for the
   `claude_subscription` and `afk` tests. Raising `terminate-after` or adding per-test overrides is
   not an acceptable substitute.

   Root cause (2026-11-14, found and fixed): the slowness was not an MCP/hook timeout. Every test
   that calls `fontaine_testkit::example_bin` ran a nested `cargo build -p <pkg> --example <name>`
   that was never a no-op. The test process inherits variables cargo sets only for it
   (`CARGO_MANIFEST_DIR`, `CARGO_PKG_*`, `OUT_DIR` for a package with a build script). `ring`'s
   build script tracks them, so the nested cargo saw different env than the outer `cargo test`
   build, marked `ring` and everything above it dirty, and recompiled (~45 s, ~90 s of CPU). The
   next outer build then saw the nested one's env and rebuilt it again, so the two ping-ponged on
   every run. Parallel test processes queued on cargo's build-dir lock, which gives the
   60 s / 120 s / 180 s multiples. Verified with `CARGO_LOG=cargo::core::compiler::fingerprint=info`
   ("Dirty ring: the env variable CARGO_MANIFEST_DIR changed", then "OUT_DIR changed").
   Ruled out on the way: the 30 s MCP `CONNECT_TIMEOUT` (the test takes 1.2 s run as a binary;
   `start_service` already returns on child EOF), `ANSWER_GRACE`, `QuotaPoller`, the `World`
   harness and the terminal reader.

   Fix: `example_bin` removes `CARGO_PKG_*`, `CARGO_MANIFEST_*`, `CARGO_BIN_EXE_*`,
   `CARGO_CRATE_NAME`, `CARGO_PRIMARY_PACKAGE`, `CARGO_TARGET_TMPDIR` and `OUT_DIR` from the
   nested command. Measured locally: `a_server_in_the_users_own_settings_starts_without_asking`
   36 s to 1.1 s; `a_credit_billed_model_is_refused` 54 s to 0.5 s. Full `cargo nextest run`:
   13 min with 80 slow tests before; 5 min 33 s with 23 slow in the first run after the fix (it
   includes the one-off rebuild); steady state 1 min 58 s with no slow test.

## Implementation Steps

1. Add `.github/workflows/pg2.yml` (schedule, dispatch) running the pg2 test with the same env
   block (`CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`), disk-freeing and toolchain steps as
   `ci.yml`.
2. In `ci.yml`: delete the `Test (pg2)` step and the "Before Test (pg2)" comment.
3. Add `rust/.config/nextest.toml` with the slow-timeout.
3a. Temporarily add a `pull_request` trigger to `pg2.yml` limited to `paths: ['.github/workflows/pg2.yml']`
   so the workflow runs on the PR and a YAML error cannot reach `main` unexercised; confirm it is green.
   Remove that trigger in the final commit before merge (the merged file keeps only `schedule` and
   `workflow_dispatch`).
4. Push the branch; take the per-step breakdown (Design 5), run the with/without cache experiment
   (Design 4: `save-if: true`, seed run then restore run), apply only measured levers, compare job durations to the 20 min target and the
   issue baseline, and record the numbers in the PR description.
5. Find and fix the cause of the slow tests (Design 6); re-run `cargo nextest run` and record the
   slow-test count and total time before and after in the PR description.
6. Update docs and the master plan (below).

## Files to Modify

- `.github/workflows/ci.yml`
- `.github/workflows/pg2.yml` (new)
- `rust/.config/nextest.toml` (new)
- `CONTRIBUTING.md` (CI paragraph, lines ~28–34; the pg2 sentence at ~30–32; the release gate as a
  readiness step in `## Releasing`)
- `AGENTS.md` (the "pg2 clippy/test/rustdoc run on Linux only" sentence in Essential Commands)
- `tasks/fontaine_harness_plan.md` (workspace layout CI paragraph at line 206; Decisions lines at 449
  (2026-10-25) and 450 (2026-11-04); line 41 and line 260 ("CI pg2 steps") checked for wording)
- `tasks/design/platforms.md` (line 8, "only pg2 clippy/test/rustdoc")
- `CHANGELOG.md` (updated: it has CI entries)

## Trade-offs

- **Drop pg2 tests from PRs vs. a separate cache for pg2**: a separate cache still costs a candle
  build on every cache miss and doubles cache storage; the scanner is opt-in and unused by the
  maintainer, so nightly coverage is enough (the issue's decision).

## Decisions

- 2026-11-14 (user): the latency target is 20 min per PR run on every OS.
- 2026-11-14: the slow tests' cause was the nested example build rebuilding on inherited cargo env (see Design 6); the nested build stays as the fallback and CI's `Build tests` step already builds the examples first.
- 2026-11-14 (user): slow tests are fixed at their cause; raising `terminate-after` or adding per-test overrides is rejected as lazy.
- 2026-11-14: pg2 tests leave per-PR CI; pg2 clippy, rustdoc and check stay. (Issue #652 decision.)
- 2026-11-14 AFK: no `rust-cache` unconditionally; it is a with/without experiment on the PR and is kept only on an OS where the cache-hit restore run is at least 3 min faster than the no-cache run, because PR #180 measured cold runs beating cached ones on every OS.
- 2026-11-14 AFK: `deny`, rustdoc, signing and evals are not split into new jobs; reason: keeps cross-OS coverage and the required-check set unchanged, as #180 also declined splitting.
- 2026-11-14 AFK: `pg2.yml` runs on `schedule` and `workflow_dispatch` only (no path-filtered push); the release gate (latest nightly green) is a manual check documented in CONTRIBUTING, accepted and not enforced in `release.yml`.
- 2026-11-14 AFK: `pg2.yml` gets a temporary `pull_request` trigger limited to `paths: ['.github/workflows/pg2.yml']` during the PR so it is exercised before merge, and the trigger is removed in the final commit; reason: schedule/dispatch only exist on the default branch, so a YAML error would otherwise reach `main` unexercised, and no permanent trigger is left.
- 2026-11-14 AFK: "wins by a clear margin" means the cache-hit restore run is at least 3 min faster than the no-cache run on that OS; otherwise the cache stays off (the default); reason: the margin was undefined and the conservative reading keeps #180's no-cache result unless the gain is substantial.

## Documentation

- `CONTRIBUTING.md`: CI now runs pg2 tests nightly (and on dispatch), not per PR; add the manual
  release check of the latest nightly to the numbered readiness steps in `## Releasing`.
- `AGENTS.md`: update the pg2 sentence in Essential Commands.
- Master plan: update the `ci.yml` description (line 206; the edit also corrects the sentence "PR
  runs restore but do not save the cache" to match the outcome) and add a Build and release Decisions line
  that narrows the 2026-10-25 (line 449) and 2026-11-04 (line 450) lines (pg2 tests off PRs; no
  `cargo clean` any more). `tasks/design/platforms.md` line 8 gets the same wording fix.
- `CHANGELOG.md`: add a CI entry.

## Testing Strategy

Workflow YAML cannot be unit tested. Verification is the PR's own CI run: all required checks stay
green and the step timings are compared against the issue baseline. A plain `cargo nextest run` locally
(or `cargo nextest list`) confirms the nextest config parses. The
pg2 workflow is exercised on the PR through the temporary path-limited `pull_request` trigger
(Implementation step 3a), and again by `workflow_dispatch` after merge.

## Manual Verification

1. After merge, trigger `gh workflow run pg2.yml` and confirm it completes green: the schedule and
   dispatch triggers only exist on the default branch, so only the temporary PR trigger could run it
   before merge. Also confirm the merged file no longer has the `pull_request` trigger.
2. If a cache was kept on any OS (with `save-if` main-only), confirm the first `main` push saves it
   and the next run restores it (cache step log shows a hit).

## Open Questions

None (all resolved; see Decisions).
