# Centralize the fontaine-tui `idle()` Test Helper Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/215

## Overview
`async fn idle(ui: &Ui)` is copy-pasted into the fontaine-tui integration tests. Move it onto the
shared harness as `Ui::idle`, delete the local copies and rewrite call sites.

## Current State
The issue counts 7 copies; the tree now holds 15 identical ones, in every file under
`rust/fontaine-tui/tests/` except `common/mod.rs`: afk, agents, bug, extensions, goal,
plugins_live, providers, quit, remote, rewind, schedule, sessions, shell_cwd, todos, tui. Each
is:

```rust
async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| s.contains("shift+tab mode · /add-dir")).await;
}
```

Call sites are `idle(&ui).await` (also `idle(ui)` where `ui` is already a reference, in tui.rs and rewind.rs).
`Ui` lives in `tests/common/mod.rs` with `wait_until` / `wait_for` at ~lines 640-657.
`fontaine-tools/src/mcp/elicitation.rs` has an unrelated `idle`; leave it.

## Design
Add to `impl Ui` in `tests/common/mod.rs`, after `wait_for`:

```rust
/// Waits for the idle prompt that every routed test drives from.
pub async fn idle(&self) {
    self.wait_until("an idle prompt", |s| s.contains("shift+tab mode · /add-dir")).await;
}
```

## Implementation Steps
1. Add `Ui::idle` in `rust/fontaine-tui/tests/common/mod.rs`.
2. In each of the 15 files, delete the local `idle` fn.
3. Rewrite call sites: `idle(&ui).await` → `ui.idle().await`; `idle(ui).await` → `ui.idle().await`.
   Find them by grepping `\bidle\(` per file (also covers other binding names, e.g. `idle(&u)`).
4. Drop `Ui` from imports in files where it becomes unused (the issue expects schedule,
   extensions, todos, sessions; others may also be affected). Let clippy decide.
5. Run the gate from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test -p fontaine-tui`.

## Files to Modify
- `rust/fontaine-tui/tests/common/mod.rs`
- the 15 test files listed above

## Trade-offs
- Method on `Ui` vs. free function in `common`: a method matches `wait_for`/`wait_for_prompt` and
  removes the need for a `Ui` import at call sites.
- Not touching other duplication named in the issue (fontaine/tests/common, guardian cascade
  tests): the issue deliberately leaves them.

## Documentation
None; test-only change. A CHANGELOG entry is not needed unless the project logs test refactors.

## Testing Strategy
The refactor is verified by the existing fontaine-tui integration tests compiling and passing, and
clippy `-D warnings` (catches unused imports). No new tests.
