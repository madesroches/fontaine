# Agents Panel Partial Cost Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/333

## Overview

The agents panel and agent list print an agent's token count (every completion) next to a dollar
figure that only covers the *priced* calls. For a worker on an unpriced account (openai-compatible
gateway with no catalog entry) whose guardian calls are priced, the figure is the guardian's spend
alone, shown as if it were the run's cost. Track how many tokens were not covered by a price, and
mark the dollar figure as partial when any were.

## Current State

- `Spend { input_tokens, output_tokens, cost_usd: Option<f64> }` in
  `fontaine-agents/src/budget.rs`; `add` sums tokens and accrues `cost_usd` from the first priced
  call.
- `spend_of(&Usage)` (`fontaine-core/src/gate.rs`, near line 892) builds a `Spend` from one call:
  `cost_usd = usage.cost.map(|c| c.total)`. Used for model calls (`turn.rs`, `goal.rs`), guardian
  calls (`SessionState::add_guardian_usage`) and child runs (`child_runner::spend`).
- `AgentTree::debit` adds each `Spend` to the agent node and the session total.
- The TUI reads `row.spend.tokens()` / `row.spend.cost_usd` in `fontaine-tui/src/ui/mod.rs`
  (`PanelRow`) and `fontaine-tui/src/agent_list.rs`, both rendering through
  `agents_panel::spend_text`.
- `spend_text` prints `N tok · $X` when a cost exists, `cost n/a` when only a cost budget is set,
  nothing extra otherwise.
- `goal::spend_since` subtracts a base `Spend` from the current one.

## Design

Add `unpriced_tokens: u64` to `Spend`: the tokens of calls that carried no cost.

- `spend_of`: `unpriced_tokens = if usage.cost.is_none() { input + output } else { 0 }`. Because
  `turn.rs` clears `usage.cost` for accounts that are not priced (`params.priced == false`), those
  calls count as unpriced too, with no extra plumbing.
- `Spend::add`: sum `unpriced_tokens`.
- `Spend::is_partial_cost()`: `cost_usd.is_some() && unpriced_tokens > 0`. The cost figure is a
  lower bound exactly then. With no cost at all, `spend_text` still shows no dollar figure (today's
  behavior), so nothing is marked.
- `goal::spend_since`: subtract `unpriced_tokens` with `saturating_sub`, like the other counters.
- `PanelRow` gets `cost_partial: bool` (set from `row.spend.is_partial_cost()`); `spend_text` gains
  that parameter and appends ` + unpriced` to the dollar figure: `4863024 tok · $0.0519 + unpriced`,
  and with a cost budget `$0.0519 / $2.00 + unpriced`. The cost cap itself is unchanged: it still
  measures priced spend only (`cost_cap_inactive` semantics untouched).
- To keep `spend_text` from growing a fifth positional argument, pass a small struct or the
  `PanelRow` fields it needs; prefer whichever keeps both callers (`agents_panel.rs`,
  `agent_list.rs`) one-liners.

Out of scope: a `/cost` breakdown (main vs guardian), which belongs to #56, and user-supplied
prices for gateway accounts.

## Implementation Steps

1. `fontaine-agents/src/budget.rs`: add the field, `add`, `is_partial_cost`, unit tests.
2. `fontaine-core/src/gate.rs`: `spend_of` fills `unpriced_tokens`.
3. `fontaine-core/src/goal.rs`: `spend_since` subtracts it.
4. Fix existing `Spend { .. }` literals (tests in `fontaine-agents/src/tree.rs`,
   `fontaine-core/tests/{goal,agent_resume}.rs`, `fontaine-agents/tests/seam.rs`,
   `fontaine-tui/src/agent_list.rs`, `budget.rs`) with `..Default::default()`.
5. `fontaine-tui`: `PanelRow.cost_partial`, `spend_text` marker, wire both callers.
6. `docs/tui.md`: describe the `+ unpriced` marker next to the `cost n/a` explanation.
7. `CHANGELOG.md` entry (done by the PR step).

## Files to Modify

- `rust/fontaine-agents/src/budget.rs`
- `rust/fontaine-core/src/gate.rs`, `rust/fontaine-core/src/goal.rs`
- `rust/fontaine-tui/src/agents_panel.rs`, `agent_list.rs`, `ui/mod.rs`
- Test literals listed in step 4
- `docs/tui.md`

## Trade-offs

- `unpriced_tokens: u64` over `cost_complete: bool`: a count survives `spend_since` subtraction and
  `add` without a special merge rule, and leaves room for the #56 breakdown.
- Marking only when a dollar figure is shown: an agent with no priced call keeps today's output
  (no `$`), which is not misleading.

## Decisions

## Documentation

`docs/tui.md` (agents overview row description, around the `cost n/a` paragraph); the agent-list
description there if it shows spend.

## Testing Strategy

- `budget.rs` unit tests: `spend_of`-style unpriced accounting through `add`; `is_partial_cost`
  true for priced+unpriced mix, false for all priced, false for all unpriced.
- `gate.rs` test: `spend_of` with and without `usage.cost`.
- `goal.rs` test: `spend_since` subtracts `unpriced_tokens`.
- `agents_panel.rs` tests for `spend_text`: partial with and without a cost budget, and the
  non-partial output unchanged. `agent_list.rs` test covering the issue's scenario (a row with
  4863024 tokens, `$0.0519`, mostly unpriced tokens renders `+ unpriced`).

## Open Questions

- Marker wording: `+ unpriced` (chosen) versus `excl. main model`; the former is accurate for any
  unpriced call, not only the main model's.
