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
resolve / budget-cap-skip / rebuild-agent / notify body into one helper taking the chain, the start
order, the `Tried`, the index sink, the from-ref and `settles`, rather than copying it):

- Walks the `fallback` chain over `0..entries.len()` in order, ignoring the chain's `active` and
  with no `failed` exclusion (so it does not reuse `fall_through_order`/`Tried::order`, which start
  at `active`), skipping the accounts of every own-list entry that actually failed on quota in this
  request (collected during `fall_through_child`), entries that fail to resolve (with an
  `Approver::warn`), and unpriced entries under an explicit `--max-budget-usd`. Its own walk
  never settles the fallback chain (`settles = false`). A child already on the fallback chain (its
  list chain is the fallback chain by `Arc::ptr_eq`, which includes `ModelSpec::Named("fallback")`)
  does not fall back again: `fall_back_child` returns `Err` at once and the code goes straight to
  the wait.
- Does not settle any shared chain: the role's other subagents are unaffected and the spent
  role keeps its entry, so a later child still tries it and pauses/falls back for itself.
- The child then serves from the fallback chain: its later quota fall-throughs walk the fallback
  chain, not back onto the spent own list. `fall_back_child` sets `*tried = Tried::default()`
  before its walk, then notes into that `tried` every fallback index it skips (spent account,
  resolve failure, unpriced) as well as the chosen one, since the fallback chain is never settled,
  `active` stays 0, and a skipped entry left un-noted would be walked back onto on a later
  fall-through, making the wait run on the spent account; the fallback walk keeps using
  that `tried` for the fallback chain, so an exhausted fallback chain ends the walk and goes to the
  pause. `RunShared.list` becomes the serving state that `fall_through_child` and `served_of` read:
  a `Mutex<Option<Arc<ChildList>>>` that `fall_back_child` swaps to
  `ChildList{chain: fallback, index, settles: false}`; `fall_through_child` clones the Arc out
  before awaiting. It is valid for list-less children (`ModelSpec::Model`, `GuardianDefault`) too,
  which fall back the same way.
- Notice: one `Approver::warn` line `<id>: <from> usage window spent; continuing on <to>`, worded by a
  function beside `fall_through_notice`, and `record_model_fall_through` with the failure's own class
  (`failure.class.unwrap_or("quota")`) and the `fallback` role named in the record (session log and
  audit log, like any fall-through).
- If every fallback entry is also spent on a window, the existing `wait_out_usage_limit` pause runs
  on the current account and, after the reset, the request goes through again with tried entries
  forgotten. After the wait, and after later answers, the child stays on the fallback chain and
  retries its current entry with tried entries forgotten.

## Implementation Steps

1. `model_roles.rs`: add `FALLBACK_ROLE`; document it with the other reserved roles.
2. `child_runner.rs`: fetch the chain at switch time with
   `shared.context.chains.chain_of_role(&shared.context.roles, FALLBACK_ROLE)`; turn `RunShared.list` into the
   serving state (`Mutex<Option<Arc<ChildList>>>`, locked with `fontaine_tools::lock::lock`); extract the shared entry-switch helper from `fall_through_child`;
   add `fall_back_child`; call it before the `wait_out_usage_limit` block.
3. Add the usage-window notice wording in `model_roles.rs`, beside `fall_through_notice`.
4. Docs and CHANGELOG (below).
5. Tests (below).

## Files to Modify

- `rust/fontaine-core/src/model_roles.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `docs/configuration.md`, `docs/agents.md`, `docs/headless.md` (usage windows), `docs/tui.md`,
  `docs/claude-subscription.md`, `docs/architecture.md`, `tasks/fontaine_harness_plan.md`,
  `CHANGELOG.md`

## Trade-offs

- **A `fallback` role vs. a per-type key.** A role reuses list syntax, validation, user-scope
  rules and shared-chain plumbing; a per-type key would duplicate all of it.

## Decisions

- 2026-11-13 AFK: the `fallback` role is opt-in (undefined = pause as today), the most conservative
  reading of "configurable".
- 2026-11-13 AFK: transport failures (503) are not in scope; they are not quota-class and the issue's
  stall was the usage-window pause. Revisit separately if wanted.

## Documentation

- `docs/configuration.md`: add the `fallback` row to the roles table and a paragraph under
  "Falling through to the next entry" (noting a pre-existing user role named `fallback` now also
  serves as the usage-window fallback); update the "Each request tries every entry once" bullet and
  "five role names" (~642) to six.
- `docs/agents.md`: update the "spent on a usage window, the whole session pauses" sentence.
- `docs/headless.md#usage-windows`: say a subagent tries the `fallback` role first (insertion at
  ~183-185).
- `docs/tui.md`: the "Usage windows spent" section (~217-228) says the run pauses; note a subagent
  tries the `fallback` role first.
- `docs/claude-subscription.md`: the usage-window paragraph (~166-168) and the "Plan limit with the
  usage window spent" table row (~178).
- `docs/architecture.md`: ~220 describes `model_fell_through` as moving to the next entry of the
  same list; add a clause for a subagent moving to the `fallback` role's list after its own list was
  spent on a usage window, recorded with role `fallback`.
- `CHANGELOG.md`: Added entry, noting a pre-existing user role named `fallback` now also serves as
  the usage-window fallback. On completion add a dated line at the end of the `### Providers`
  subsection of `tasks/fontaine_harness_plan.md` `## Decisions`, saying it amends the Providers
  line "a spent list pauses the run until the reset" (#115, line 1011) for subagents when a
  `fallback` role is defined.

## Testing Strategy

Unit/integration with `rig_core::test_utils` deterministic models (no live APIs). The subagent cases
use `rust/fontaine/tests/model_roles.rs` (e.g. `subagents_on_a_role_share_the_entry_that_worked`
drives a child through a quota fall-through with mock endpoints and asserts the `model_fell_through`
ledger record) and `rust/fontaine/tests/usage_limit_headless.rs` (`refusal(WINDOW, n)` helper,
`--max-usage-wait` bound) as seams; `rust/fontaine-core/tests/usage_pause_turn.rs` and
`model_roles_runtime.rs` stay for the wait itself:

- Spent-window subagent with a defined `fallback`: moves to its first usable entry, continues, no
  pause begins, notice and ledger/audit record emitted.
- No `fallback` defined: pauses exactly as before (regression pin).
- Fallback entries all spent: pauses; entry on the spent account is skipped; unresolved entry
  skipped with a warning; unpriced entry skipped under `--max-budget-usd`.
- Switch does not change the child's mode, rules or guardian; other children of the role keep their
  entry.
- Later quota failure on the fallback entry walks the fallback chain, never back to the spent entry.

## Open Questions

None.
