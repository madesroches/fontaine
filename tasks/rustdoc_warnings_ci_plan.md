# Fix rustdoc Warnings and Gate CI on Them Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/197

## Overview
`cargo doc --no-deps --workspace` emits three rustdoc warnings and nothing in CI builds the docs, so
such regressions go unnoticed. Fix the three doc comments and add a CI step that fails on rustdoc warnings.

## Current State
Running `cargo doc --no-deps --workspace` (also with `--features fontaine/pg2`) warns:
- `fontaine-core/src/accounts.rs:139` — public `rows` links to private `credential`.
- `fontaine-core/src/quota.rs:43` — unresolved link ``[`fetch`]`` in a doc comment on a trait method
  (`fn fetch` at line 40 is a sibling method, so the bare path does not resolve).
- `fontaine-agents/src/spawn.rs:453` — public `watch_report` links to private `continue_agent`.

`.github/workflows/ci.yml` runs clippy, tests, and `cargo test --doc` (line ~100) but never `cargo doc`.

## Design
- Doc fixes: replace each link with plain code formatting (`` `credential` ``, `` `continue_agent` ``)
  where the target is private; for `fetch` use ``[`Self::fetch`]`` (verify it resolves, else plain code).
- CI: add a step "Rustdoc" right after "Doctests", gated like its neighbours
  (`if: needs.changes.outputs.code == 'true'`), running `cargo doc --no-deps --workspace` with
  `RUSTDOCFLAGS: -D warnings` set via `env:`. Runs on all three OSes (cheap, and platform-specific
  `cfg` code gets documented per-OS). pg2-feature docs are covered by an extra Linux-only invocation
  `cargo doc --no-deps --workspace --features fontaine/pg2` in the same step's script, since pg2 code is
  only compiled with the feature.

## Implementation Steps
1. Edit the three doc comments listed above.
2. Add the CI step to `.github/workflows/ci.yml`.
3. Update the master plan's `ci.yml` description (`tasks/fontaine_harness_plan.md` ~line 205) to mention
   the rustdoc step, and AGENTS.md "Essential Commands" CI list.
4. Add a CHANGELOG entry if the file's convention calls for one.

## Files to Modify
- `rust/fontaine-core/src/accounts.rs`, `rust/fontaine-core/src/quota.rs`, `rust/fontaine-agents/src/spawn.rs`
- `.github/workflows/ci.yml`
- `tasks/fontaine_harness_plan.md`, `AGENTS.md`, `CHANGELOG.md`

## Trade-offs
- Dropping links vs. making the private items public: the items are deliberately private; plain code
  formatting loses nothing for readers.
- `--document-private-items` was not chosen: the issue targets public docs, and it would add noise.

## Documentation
AGENTS.md command list and the master plan's CI description (steps 3).

## Testing Strategy
The CI step is itself the regression test. Locally verify
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` (and with `--features fontaine/pg2`) exits 0,
and that it fails when a bad link is reintroduced.

## Open Questions
None.
