# Lock-Poisoning Policy Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/186

## Overview

A poisoned lock (a thread panicked while holding it) is handled three ways today: about 70 sites
silently take the guard with `.unwrap_or_else(|e| e.into_inner())`, `providers/token_source.rs`
uses `.expect("… not poisoned")`, and nothing says which is right. The silent recovery is
concentrated in `SessionState`, whose ~18 locks hold permission state; a panic halfway through
updating a grant list or the loop breaker leaves state that later permission decisions read as
consistent. This plan picks one policy (Option A of the issue): state that feeds a permission or
guardian decision, or a durable record, panics on poison; best-effort state recovers through one
named helper with a stated reason. The `into_inner()` pattern then appears in exactly one file.

## Current State

`rg 'into_inner\(\)'` over `rust/` lists 74 sites. Split by file:

- Permission and guardian state: `fontaine-core/src/gate.rs` (40, the `SessionState` fields and
  the `Inner` rules lock behind its private `read()`/`write()`), `fontaine-agents/src/tree.rs`
  (5: tree `Inner`, `root_mode`, `root_tools`), `fontaine-guardian/src/guardian.rs` (4: verdict
  cache, usage tally), `fontaine-core/src/scanner_stage.rs` (1: scanner backend).
- Durable records the guardian and audit read back: `ledger.rs` (3), `audit.rs` (1).
- Model/agent wiring: `agent.rs` (7), `child_runner.rs` (6), `skills.rs` (2).
- Best-effort: `steering.rs` (1, the queued-text buffer), `fontaine-tui/src/ui/mod.rs` (1, the
  `@`-file index), `fontaine/src/telemetry/micromegas.rs` (2, metric/location tallies),
  `fontaine/tests/telemetry_micromegas.rs` (1, a test-serialising mutex).
- `fontaine-core/src/providers/token_source.rs` already panics with `expect` (10 sites).

`fontaine-guardian` has no `fontaine-*` dependencies and is meant to stay lift-out-able, so it
cannot use a helper from `fontaine-tools`. `fontaine-tools` is a dependency of `fontaine-agents`,
`fontaine-core`, `fontaine-tui`, `fontaine-mcp`, `fontaine-evals` and `fontaine`.

## Design

### Policy

1. **Strict (default).** Any lock whose contents feed a permission decision, a guardian input or
   verdict, a spend tally, the model actually used, or a durable record (ledger, audit): poison
   panics, naming the lock. A corrupted permission state ends the session instead of continuing
   to decide.
2. **Recovering (exception).** A lock whose contents are display or advisory state, where a
   half-updated value is harmless and a panic would turn one bad thread into a session crash:
   poison recovers via `lock_recovering` / `read_recovering` / `write_recovering`. Each call site
   carries a one-line reason comment (the *why*, per AGENTS.md).

Classification of every site:

| File | Policy |
|---|---|
| `gate.rs`, `tree.rs`, `guardian.rs`, `scanner_stage.rs`, `ledger.rs`, `audit.rs` | strict |
| `agent.rs` model slots (`main` slot `:1042`; `llm_scanner` slot `:982`; switcher read `:991`; switcher `config.accounts` writes `:1127`, `:1140`), `child_runner.rs` model reads and `status`, `skills.rs` shell handle | strict |
| `agent.rs` `files` index slots (`:1301`, `:1311`), `ui/mod.rs` files read, `steering.rs`, `micromegas.rs` (2), `child_runner.rs` `last_text`/`seen` partial text (`:360`, `:410`), `telemetry_micromegas.rs` test mutex | recovering |
| `token_source.rs` | strict (already; moved onto the helper) |

### Helpers

New `fontaine-tools/src/lock.rs`, re-exported as `fontaine_tools::lock`:

```text
pub fn lock<'a, T>(m: &'a Mutex<T>, what: &str) -> MutexGuard<'a, T>      // panics: "the {what} lock is not poisoned"
pub fn read<'a, T>(l: &'a RwLock<T>, what: &str) -> RwLockReadGuard<'a, T>
pub fn write<'a, T>(l: &'a RwLock<T>, what: &str) -> RwLockWriteGuard<'a, T>
pub fn lock_recovering / read_recovering / write_recovering              // PoisonError::into_inner, no `what`
```

`expect` needs a `'static` literal message, so the strict helpers format the name with
`unwrap_or_else(|_| panic!(...))`. The `what` argument keeps the panic message as informative as
the current ad-hoc `expect` texts.

`SessionState` keeps its private `read()`/`write()` for `Inner` (now strict) and gains no further
per-field wrappers: `lock(&self.breaker, "loop breaker")` at each site is already one call, and the
policy lives in the helper module. `AgentTree::lock` and `Steering::state` and `Ledger::view` keep
their wrapper bodies, switched to the helpers.

`fontaine-guardian` cannot depend on `fontaine-tools`, so its four sites use
`.expect("the verdict cache lock is not poisoned")` / `.expect("the usage tally lock is not
poisoned")` directly — the same policy, spelled locally.

### No behaviour change off the panic path

Guards are the same types as before; only the poisoned branch differs.

## Implementation Steps

1. Add `fontaine-tools/src/lock.rs` with the six helpers and unit tests; `pub mod lock;` in
   `fontaine-tools/src/lib.rs`.
2. `fontaine-core/src/gate.rs`: replace all 40 sites with `lock`/`read`/`write` helpers (names
   taken from the field), including the `set_root_mode` closure.
3. `fontaine-agents/src/tree.rs`, `fontaine-core/src/{scanner_stage,ledger,audit,skills}.rs`:
   strict helpers.
4. `fontaine-core/src/agent.rs`, `child_runner.rs`: strict for model slots and status, recovering
   for the file-index slots and partial-text buffers, each with a reason comment.
5. `fontaine-core/src/steering.rs`, `fontaine-tui/src/ui/mod.rs`,
   `fontaine/src/telemetry/micromegas.rs`, `fontaine/tests/telemetry_micromegas.rs`: recovering
   helpers with reason comments.
6. `fontaine-core/src/providers/token_source.rs`: move the ten `expect` sites to the strict
   helpers (same message text): `what` is "warning sink" at `:128` and `:290`, "registry" at `:167`, and "token" for the other seven.
7. `fontaine-guardian/src/guardian.rs`: four local `expect`s.
8. Record the policy in `tasks/fontaine_harness_plan.md` `## Decisions`; add the poisoning rule to
   AGENTS.md's Error handling bullets; CHANGELOG entry.
9. Verify `rg 'into_inner\(\)' rust --glob '*.rs'` lists only `lock.rs`.

## Files to Modify

`fontaine-tools/src/{lock.rs,lib.rs}`; `fontaine-core/src/{gate,agent,child_runner,ledger,audit,skills,steering,scanner_stage}.rs`;
`fontaine-core/src/providers/token_source.rs`; `fontaine-agents/src/tree.rs`;
`fontaine-guardian/src/guardian.rs`; `fontaine-tui/src/ui/mod.rs`;
`fontaine/src/telemetry/micromegas.rs`; `fontaine/tests/telemetry_micromegas.rs`;
`tasks/fontaine_harness_plan.md`; `AGENTS.md`; `CHANGELOG.md`.

## Trade-offs

- **`parking_lot` (issue Option B).** Never poisons, so no policy needed, but a poisoned-by-panic
  permission state is then silently *consistent-looking* again, which is the defect; it also adds
  a pinned dependency and a `cargo deny` surface, and `fontaine-guardian` must stay
  dependency-light.
- **Helper methods per struct** (issue suggestion) versus free functions with a `what` name: the
  structs hold ~18 locks of three guard kinds, so per-field methods would be ~18 near-identical
  wrappers; free helpers keep the policy in one file.
- **Strict everywhere.** Simpler, but a panic in a telemetry tally or the `@`-file index would then
  end a session for no safety gain.

## Decisions

- 2026-10-25: Locks guarding permission, guardian, spend, model-selection or durable-record state panic on poison; display and advisory state recovers through `lock_recovering` with a reason comment, because a corrupted permission state must end the session rather than keep deciding, and recovery is only harmless where a half-updated value cannot reach a decision (#186).
- 2026-10-25: `fontaine-guardian` spells the strict policy locally with `expect`, because it must not depend on `fontaine-tools` (#186).
- 2026-10-25: `steering.rs` stays recovering: its lock guards two `Vec<String>` buffers touched only by `mem::take`/`extend`, which cannot be left half-updated, and the durable `Record::UserMessage` is written under the separate strict ledger lock (#186).

## Documentation

AGENTS.md Error handling: one bullet stating the poisoning policy and naming the helpers. No
`docs/` page covers locks.

## Testing Strategy

- `lock.rs` unit tests: poison a `Mutex` and an `RwLock` by panicking a thread that holds the
  guard; assert the strict helpers panic with the lock's name (`#[should_panic(expected = …)]`)
  and the recovering helpers return the stored value.
- `gate.rs` test: add a new child `mod tests` in `gate.rs` (it can reach private fields); poison
  `SessionState`'s `breaker` lock and assert `guardian_denied()` or `auto_paused()` panics rather than deciding — pins the headline behaviour change.
- Existing suites stay green unchanged (no non-panicking behaviour change). The `rg` acceptance
  check is run by hand in step 9 and is not a test.

## Open Questions

- Should `skills.rs`'s shell handle (set once at startup, read per skill command) be recovering?
  Plan says strict because it decides what executes; revisit only if a reviewer disagrees.
