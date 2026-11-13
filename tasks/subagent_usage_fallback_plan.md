# Subagent Usage-Window Fallback Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/601

## Overview

When a subagent's whole model list is spent on a usage window, the session pauses until the
window resets even if another configured model has budget (observed: a `coding` role of
`kimi/kimi-for-coding` spent for 394h while `claudemax/sonnet` was at 7%). Add a user-scope
`fallback` role: before pausing, a subagent whose list is spent on a usage window moves to the next
usable entry of that role, says so, and continues. The pause happens only when the fallback list is
also spent (or undefined).

## Current State

- `[models.roles]` lists fall through on quota-class errors (`docs/configuration.md`, "Falling
  through to the next entry"); a child does it in `fall_through_child`
  (`rust/fontaine-core/src/child_runner.rs` ~1170) over its `ChildList` (`child_runner.rs:394`),
  with `Tried` (`model_roles.rs:353`) preventing cycles.
- When `fall_through_child` has nothing left and `failure.usage_limit` is set, the child loop
  (`child_runner.rs` ~1110) calls `wait_out_usage_limit` (`turn.rs:562`), which pauses the whole
  session (`usage_pause.rs`).
- Reserved roles live in `model_roles.rs:15-22` (`main`, `critique`, `setup`, `compaction`,
  `advisor`); `RoleChains::chain_of_role` gives a shared chain for a role. Roles are read from user
  scope and `--settings` only.
- A child's agent is rebuilt per entry with `build_child_agent` from the same `agent_type`, tools
  and kill channel, so a model switch never touches the mode, permissions or guardian.

## Design

**Config.** A sixth reserved role, `fallback` (`FALLBACK_ROLE` in `model_roles.rs`): an ordered
list of `<account>/<model>` or role names, validated by the existing role rules. Undefined means no
fallback and today's behavior (a switch of model is a cost and capability change, so it is
opt-in). Order and eligibility are the list itself: an entry not listed is never used. User scope
only comes for free from `[models.roles]`.

**Trigger.** Only a *subagent* whose own list is exhausted on a usage window (`failure.usage_limit`
is set after `fall_through_child` returns `Err`). The main conversation is unchanged: its `main`
list already expresses its fallbacks. Other quota-class failures keep their existing handling.

**Behavior.** New `fall_back_child` beside `fall_through_child`, sharing its entry loop (extract the
resolve / budget-cap-skip / rebuild-agent / notify body into one helper taking the chain, a
`Tried` and `settles = false`, rather than copying it):

- Walks the `fallback` chain from its first entry, skipping entries on the spent account, entries
  that fail to resolve (with an `Approver::warn`), and unpriced entries under an explicit
  `--max-budget-usd`.
- Does not settle any shared chain: the role's other subagents are unaffected and the spent
  role keeps its entry, so a later child still tries it and pauses/falls back for itself.
- Rebuilds the agent on the new handle with the same type, tools and restrictions; mode, rules,
  hooks and the guardian are untouched.
- The child then serves from the fallback chain: its later quota fall-throughs walk the fallback
  chain, not back onto the spent own list. A `ChildList`-level `serving` state (own / fallback with
  its index) carries this.
- Notice: one `Approver::warn` line `<id>: <from> usage window spent; continuing on <to>`, and
  `record_model_fall_through` with class `usage_window` (session log and audit log, like any
  fall-through).
- If every fallback entry is also spent on a window, the existing `wait_out_usage_limit` pause runs
  on the current account and, after the reset, the request goes through again with tried entries
  forgotten.

## Implementation Steps

1. `model_roles.rs`: add `FALLBACK_ROLE`; document it with the other reserved roles; expose the
   chain through the runtime's `role_chain` like `advisor`'s.
2. `child_runner.rs`: add the fallback chain to `RunContext`/`RunShared` (from `role_chain`);
   add the `serving` state; extract the shared entry-switch helper from `fall_through_child`;
   add `fall_back_child`; call it before the `wait_out_usage_limit` block.
3. Wire `fall_through_notice`-style wording for the usage-window case in `model_roles.rs`.
4. Docs and CHANGELOG (below).
5. Tests (below).

## Files to Modify

- `rust/fontaine-core/src/model_roles.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-core/src/agent/build.rs` (hand the fallback chain to the child context)
- `docs/configuration.md`, `docs/agents.md`, `docs/headless.md` (usage windows), `CHANGELOG.md`

## Trade-offs

- **A `fallback` role vs. a per-type key.** A role reuses list syntax, validation, user-scope
  rules and shared-chain plumbing; a per-type key would duplicate all of it.
- **Default off vs. defaulting to `$main`.** Silent model changes alter cost and quality; opt-in
  keeps today's behavior for everyone who has not asked.
- **Subagents only.** The main conversation already has its own list.

## Decisions

- 2026-11-13 AFK: the `fallback` role is opt-in (undefined = pause as today), the most conservative
  reading of "configurable".
- 2026-11-13 AFK: transport failures (503) are not in scope; they are not quota-class and the issue's
  stall was the usage-window pause. Revisit separately if wanted.
- A fallback never settles a shared chain, so one subagent's switch does not move its role's others.

## Documentation

- `docs/configuration.md`: add the `fallback` row to the roles table and a paragraph under
  "Falling through to the next entry"; update the "Each request tries every entry once" bullet.
- `docs/agents.md`: update the "spent on a usage window, the whole session pauses" sentence.
- `docs/headless.md#usage-windows`: say a subagent tries the `fallback` role first.
- `CHANGELOG.md`: Added entry. Record the deviation-free addition in the plan's `## Decisions`
  (Agents subsection) on completion.

## Testing Strategy

Unit/integration with `rig_core::test_utils` deterministic models (no live APIs), in the style of
`rust/fontaine-core/tests/usage_pause_turn.rs` and `model_roles_runtime.rs`:

- Spent-window subagent with a defined `fallback`: moves to its first usable entry, continues, no
  pause begins, notice and ledger/audit record emitted.
- No `fallback` defined: pauses exactly as before (regression pin).
- Fallback entries all spent: pauses; entry on the spent account is skipped; unresolved entry
  skipped with a warning; unpriced entry skipped under `--max-budget-usd`.
- Switch does not change the child's mode, rules or guardian; other children of the role keep their
  entry.
- Later quota failure on the fallback entry walks the fallback chain, never back to the spent entry.
- Config validation: `fallback` with an unknown account or cycle is exit code 2; project scope cannot
  set it.

## Open Questions

None.
