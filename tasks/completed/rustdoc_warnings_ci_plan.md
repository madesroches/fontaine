# Fix rustdoc Warnings and Gate CI on Them Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/197

## Overview
`cargo doc --no-deps --workspace` emits three rustdoc warnings and nothing in CI builds the docs, so
such regressions go unnoticed. Fix the three doc comments and add a CI step that fails on rustdoc warnings.

## Current State
Running `cargo doc --no-deps --workspace` (also with `--features fontaine/pg2`) warns:
- `rust/fontaine-core/src/accounts.rs:139` — public `rows` links to private `credential`.
- `rust/fontaine-core/src/quota.rs:43` — unresolved link ``[`fetch`]`` in a doc comment on a trait method
  (`fn fetch` at line 40 is a sibling method, so the bare path does not resolve).
- `rust/fontaine-agents/src/spawn.rs:453` — public `watch_report` links to private `continue_agent`.

`.github/workflows/ci.yml` runs clippy, tests, and `cargo test --doc` (line ~100) but never `cargo doc`.

## Design
- Doc fixes: replace each link with plain code formatting (`` `credential` ``, `` `continue_agent` ``)
  where the target is private; for `fetch` use ``[`Self::fetch`]`` (verify it resolves, else plain code).
- CI: two steps placed after "Doctests" and before "Test (pg2)" (which runs `cargo clean` because Linux
  disk is tight, so the pg2 docs must build before it). Each has its own `env: RUSTDOCFLAGS: -D warnings`:
  - "Rustdoc" on all OSes (cheap, and platform-specific `cfg` code gets documented per-OS), gated
    `if: needs.changes.outputs.code == 'true'`, running `cargo doc --no-deps --workspace`.
  - "Rustdoc (pg2)", gated `needs.changes.outputs.code == 'true' && runner.os == 'Linux'`, running
    `cargo doc --no-deps --workspace --features fontaine/pg2`, since pg2 code is only compiled with the
    feature. Separate steps because a step-level `if` cannot make one command of a shared script
    Linux-only, and pwsh can mask a first-command failure.

## Implementation Steps
1. Edit the three doc comments listed above.
2. Add the two CI steps to `.github/workflows/ci.yml`.
3. Update the master plan's `ci.yml` description (`tasks/fontaine_harness_plan.md` ~line 205) to mention
   the rustdoc steps, AGENTS.md "Essential Commands" CI list, and CONTRIBUTING.md (add the rustdoc step
   to its CI-steps paragraph and `cargo doc --no-deps --workspace` to its command list). Also update
   `tasks/design/platforms.md` (~lines 7-9, "only pg2 clippy/test and the fixture-eval steps are
   Linux-only") to include the Linux-only pg2 rustdoc step.
4. Add a `Build and release` entry under `## [Unreleased]` in `CHANGELOG.md` (CI now builds rustdoc with warnings denied), as CONTRIBUTING.md requires.

## Files to Modify
- `rust/fontaine-core/src/accounts.rs`, `rust/fontaine-core/src/quota.rs`, `rust/fontaine-agents/src/spawn.rs`
- `.github/workflows/ci.yml`
- `tasks/fontaine_harness_plan.md`, `tasks/design/platforms.md`, `AGENTS.md`, `CONTRIBUTING.md`, `CHANGELOG.md`

## Trade-offs
- Dropping links vs. making the private items public: the items are deliberately private; plain code
  formatting loses nothing for readers.
- `--document-private-items` was not chosen: the issue targets public docs, and it would add noise.

## Testing Strategy
The CI step is itself the regression test. Locally verify
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` (and with `--features fontaine/pg2`) exits 0,
and that it fails when a bad link is reintroduced. The same local run also confirms ``[`Self::fetch`]``
resolves; if it does not, fall back to plain code formatting.

## Decisions
- AFK: CHANGELOG entry added under `Build and release`, since CONTRIBUTING.md requires one for every change and has no exemption for internal ones.

## Open Questions
None.
