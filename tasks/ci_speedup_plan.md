# CI Speed-up Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/178

## Overview

Windows dominates CI wall time (~32 min vs ~8 min on Ubuntu, run 40202180861). Cut the Windows job
to well under half of that without weakening what CI gates, by removing redundant work (pg2 legs,
eval runs, cache saves on PRs, doc-only PRs) first and tuning build mechanics (debug info, linker,
test runner, working drive) second, measuring each change and keeping only what pays off.

## Current State

`.github/workflows/ci.yml` has one `check` job over `[ubuntu, macos, windows]`, each running
sequentially: `fmt`, `clippy`, `clippy (pg2)`, `test`, `test (pg2)`, `cargo deny`, and three
`fontaine-evals` runs (`fixture`, `fixture --profile minimal`, `none`); a separate Ubuntu
`release plan` job runs `dist plan`. `Swatinem/rust-cache@v2` saves the cache on every run,
including PRs. There is no `[profile]` section in `rust/Cargo.toml`, so dev/test builds carry full
debuginfo. There is no concurrency control, and every PR runs the whole matrix.

The pg2 feature (`fontaine-guardian/pg2`: candle + tokenizers, forwarded through `fontaine-core`,
`fontaine`, `fontaine-evals`) is pure Rust with `default-features = false`; code behind it lives in
`fontaine-guardian/src/scanner{,/pg2}.rs`, `fontaine-core/src/{scanner_stage,lib}.rs` and four test
files. There is no OS-specific code in this repo behind it, but candle-core's dependencies (gemm,
memmap2, half) have arch/OS-specific code, and release binaries ship with pg2 on every dist target
(`[package.metadata.dist] features = ["pg2"]` in `rust/fontaine/Cargo.toml`).

Branch protection on `main` requires exactly three status contexts: `ubuntu-latest`, `macos-latest`,
`windows-latest` (the `check` matrix job names). These names must not change.

Baseline Windows steps (s): Test 584, Test (pg2) 644, cache post-save 313, Eval default 98,
Clippy 100, Clippy (pg2) 58.

## Design

Ordered cheapest/safest first; each stage is its own commit and is measured on a CI run before the
next is kept. "Measure" means: push the branch, read per-step durations with
`gh run view <id> --json jobs`, and record before/after in the PR description.

### Stage 1 — remove redundant work (no tooling risk)

1. **pg2 clippy and test on Linux only; compile check everywhere.** Add `if: runner.os == 'Linux'`
   to `Clippy (pg2)` and `Test (pg2)`, and replace them on Windows/macOS with a single
   `cargo check --features fontaine/pg2` (no test, no clippy; Linux runs pg2 clippy and test, macOS and
   Windows run the check). Linux catches lint and test regressions
   in the platform-independent pg2 code; the cheap check on macOS and Windows surfaces an arch/OS-specific
   candle dependency break at PR time instead of at release. Saves most of the ~700 s on Windows
   and ~290 s on macOS.
2. **Eval runs.** `fixture` (default and `--profile minimal`) are scoring over static data and
   platform-independent: Linux only. `--guardian none` drives the real `PermissionPipeline`
   including path handling, so it stays on all three OSes.
3. **Concurrency.** `concurrency: { group: ci-${{ github.event_name == 'pull_request' && github.ref || github.sha }}, cancel-in-progress: ${{ github.event_name == 'pull_request' }} }`
   (cancels pull request runs only; `main` pushes get per-commit groups so queue replacement never
   cancels them), so a force-push doesn't queue behind a stale run.
4. **Cache saves only on `main`.** `rust-cache` with `save-if: ${{ github.ref == 'refs/heads/main' }}`;
   PRs restore but do not save, removing the ~313 s Windows post-step. `main` runs keep the cache warm.

### Stage 2 — shrink and speed up builds

5. **Less debuginfo.** `[profile.dev] debug = "line-tables-only"` in `rust/Cargo.toml` (the test
   profile inherits it). Backtraces keep file/line; PDB size and link time drop.
6. **Windows working drive.** Measure Defender/disk effect by setting `CARGO_TARGET_DIR` to a path on
   `D:\` in the job env (Windows runners only) before the `rust-cache` step, and point the cache at
   the new location (`workspaces: rust -> <path>` or `cache-directories`). Judge the gain net of
   cache restore and save time; keep only if clear. The `working-directory: rust` default is unchanged.
7. **Linker.** Try `-C linker=rust-lld` via `RUSTFLAGS` on the MSVC target for Windows only; keep
   only if it links measurably faster and all tests pass.
8. **`cargo nextest`.** Install with `taiki-e/install-action`, run `cargo nextest run` plus
   `cargo test --doc`. Nextest has known process-spawn overhead on Windows, so trial it per-OS and
   adopt only where it wins; if it wins on Linux/macOS but not Windows, use it conditionally via a
   matrix variable rather than forking the step list.

### Stage 3 — structural (only if Windows is still above target after Stage 1–2)

9. **Not done: splitting lint and test into separate jobs.** It would rename or add checks, and
   branch protection requires `ubuntu-latest` / `macos-latest` / `windows-latest`; the split would
   need an aggregate required check and a branch-protection change. Recorded as a follow-up only if
   Stage 1-2 and item 10 miss the target and the maintainer approves a protection change.
10. **Skip the Rust work for non-code PRs, keeping the matrix job names.** Add a `changes` job (plain
    `git diff --name-only` against the base; `code` is false only when every changed path is under
    `tasks/` or is `AGENTS.md`, since Rust tests also read `docs/` (which links into `research/`), `analytics/` and
    the root markdown files) and make `check` `needs: changes`. The `check` job itself always
    runs (so its three expanded names always report), and every step after checkout carries
    `if: needs.changes.outputs.code == 'true'`. A job-level `if` is not used: a skipped matrix job
    reports one check under the unexpanded name and the three required contexts would stay
    "Expected - Waiting". Non-code PRs then finish in seconds with all three contexts green.
    `release plan` stays unconditional.

### Not done

Sharing artifacts between clippy and test: clippy produces check metadata, not codegen, so there is
nothing to reuse. `cargo test --no-run` reuse is already what `cargo test` does.

## Implementation Steps

1. Capture the baseline: record per-step durations for run 40202180861 in the PR description.
2. Stage 1 edits to `.github/workflows/ci.yml` (items 1–4); push; measure all three OSes.
3. Stage 2 items 5–8 as separate commits in `ci.yml` / `rust/Cargo.toml`, each measured; revert any
   that does not pay off (record the measurement in `## Decisions`).
4. If Windows wall time is still above ~16 min, do Stage 3 (item 10).
5. Update documentation (below) to match the final step set.
6. Record final before/after wall times per OS in the PR description.

## Files to Modify

- `.github/workflows/ci.yml`
- `rust/Cargo.toml` (`[profile.dev]`)
- `AGENTS.md` (Essential Commands CI paragraph)
- `tasks/fontaine_harness_plan.md` (workspace-layout `ci.yml` description; `## Decisions`)
- `tasks/design/platforms.md` ("every CI step runs on all three OSes" sentence)
- `CONTRIBUTING.md` (line ~18 says CI runs the first seven steps on all three OSes and lists the
  eval commands; stale once pg2 and fixture evals are Linux-only)

## Decisions

- 2026-10-25 (AFK): The eval `none` run stays on all OSes because it exercises the real permission
  pipeline over platform path handling; only the pure-scoring `fixture` runs go Linux-only.
- 2026-10-25 (AFK): Path-filtered skipping uses `git diff` in a `changes` job rather than a
  third-party action, to avoid a new supply-chain dependency in CI.
- The master plan's decision "every CI step runs on all three OSes" is narrowed: platform-specific
  compile, lint and test coverage stays on all three; Linux runs pg2 clippy and test, macOS and
  Windows run `cargo check --features fontaine/pg2`; the fixture-eval steps are Linux-only. A dated line goes in
  the master plan's `## Decisions` when this ships.
- AFK: Required check names `ubuntu-latest`/`macos-latest`/`windows-latest` (verified in
  branch protection) must not change, and branch protection is not modified by this work.
- AFK: Stage 3 item 10 gates steps (not the job) on the `changes` output so the matrix
  names still report; item 9 (lint/test job split) is dropped because it renames checks.
- AFK: Stage 1 item 1 keeps `cargo check --features fontaine/pg2` on macOS and Windows
  (release ships pg2 on all dist targets; candle deps have arch/OS-specific code); only pg2
  clippy and test are Linux-only.
- AFK: Stage 2 items 6 (D:\ target dir), 7 (rust-lld) and 8 (nextest) are deliberately not
  shipped in this change: they need CI measurement and are follow-ups to evaluate with the PR's
  measured run.
- AFK: Stage 2 item 5 shipped in the combined Stage 1 commit.
- AFK: Stage 3 item 10 shipped unconditionally because it cannot hurt required checks.

## Documentation

- `AGENTS.md` and `CONTRIBUTING.md`: note which CI steps are Linux-only.

## Testing Strategy

CI configuration has no unit-testable logic. Verification is the CI run itself:
- every job in the final `ci.yml` is green on the PR for all three OSes;
- `actionlint` (if available locally) passes on the workflow;
- the choke-point tests, `cargo deny`, Windows/macOS `cargo test` and `clippy` still execute on every
  code-touching PR (confirmed by reading the run's step list).

## Manual Verification

1. Open the PR; read `gh run view <id> --json jobs` for the final run and compare Windows total with
   the 32 min baseline (target < 16 min). Not automated: wall time depends on runner variance and
   must be judged over a couple of runs.
2. Push a commit touching only `tasks/` (or `AGENTS.md`) to a throwaway branch/PR and confirm the matrix is skipped but the PR
   shows as mergeable (item 10 shipped), with `ubuntu-latest`, `macos-latest` and `windows-latest` all
   reporting green.
