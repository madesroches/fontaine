# Android CI On Every PR Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/712

## Overview
`.github/workflows/android.yml` (NDK clippy plus a debug-signed APK) runs only when a PR touches a
short list of paths. The Android crate depends transitively on other workspace crates and shared
config, so a PR outside that list can break the Android build with the workflow never running.
Make the workflow run on every PR and on every push to `main`.

## Current State
- `.github/workflows/android.yml` has `paths:` filters on both `pull_request` and `push` (to
  `main`): `rust/fontaine-android/**`, `rust/fontaine-remote/**`, `rust/Cargo.toml`,
  `rust/Cargo.lock`, `rust/rust-toolchain.toml`, the workflow file.
- `ci.yml` covers only the portable part of `fontaine-android`.
- The filter is described in prose in `tasks/fontaine_harness_plan.md` (workspace layout, the
  `android.yml` line) and in `tasks/android_app_plan.md` step 7 (the latter is a historical plan).

## Design
Remove both `paths:` blocks. The `pull_request` trigger stays unfiltered and `push` stays limited
to `branches: [main]`. Nothing else in the workflow changes: the concurrency group already cancels
superseded PR runs, and `rust-cache` saves only on `main`, so repeated PR runs reuse the main cache.

Update the workspace-layout line in `tasks/fontaine_harness_plan.md` to say the workflow runs on
every PR.

The workflow's header comment stays accurate (it explains the split with `ci.yml`, not the filter).

## Implementation Steps
1. Edit `.github/workflows/android.yml`: delete the two `paths:` lists.
2. Edit `tasks/fontaine_harness_plan.md`: the `android.yml` layout line, and one line
   `- 2026-11-19 (#712): ...` at the end of `### Build and release` in `## Decisions`, recording the
   cost accepted below.
3. Add a `CHANGELOG.md` `[Unreleased]` entry: `### Changed` there is a flat list, so append a plain
   entry at its end, ending with `(#712)`.

## Files to Modify
- `.github/workflows/android.yml`
- `tasks/fontaine_harness_plan.md`
- `CHANGELOG.md`

## Trade-offs
- Drop the filter (chosen): the issue's stated expectation is that the build runs on every PR; no
  path list can track the Android crate's transitive dependencies without drifting again.
- Widen to `rust/**`: still misses non-Rust inputs (workflow, scripts, toolchain) and adds a list
  to maintain.
- Keep the filter with an always-running required job: more moving parts and still blind to
  transitive breakage.

## Decisions
- Cost accepted: one Linux job (cached from `main`) per PR, including PRs that touch only `tasks/`
  or `AGENTS.md` (which `ci.yml` skips Rust steps for), in exchange for no blind spot.

## Documentation
No `docs/` page mentions the trigger. Plan-doc and changelog updates are listed above.

## Testing Strategy
No unit-testable behavior. Validate the YAML parses (a YAML load, or `actionlint` if installed).
The end-to-end check is the workflow running on a later PR that touches only `tasks/` or `docs/`
(this PR edits the workflow itself, so its own run proves nothing about the filter).

## Open Questions
None.
