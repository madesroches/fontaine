# Subagents Uncapped by Default Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/285

## Overview

Remove the default per-subagent budgets — 200,000 tokens and $2.00 — so a subagent runs until
its task is done, matching Claude Code, which places no token budget on Task agents.
`subagent_token_budget` and `subagent_cost_budget_usd` stay as opt-in caps for users who want
per-agent spend control; the outer guardrails (`--max-budget-usd`, session budgets) are
unchanged. Today the token cap makes failure correlate with task size: the largest delegations
(a reviewer subagent over a 51-file, +6,013-line diff) die mid-work at ~200k tokens with only a
partial report.

## Current State

- `AgentsSection.subagent_token_budget: Option<u64>` and
  `AgentsSection.subagent_cost_budget_usd: Option<f64>` are the raw config keys
  (`rust/fontaine-core/src/config.rs:580-581`).
- The resolved `Agents.subagent_token_budget` is a **non-optional** `u64` defaulting to
  `200_000` and `Agents.subagent_cost_budget_usd` a non-optional `f64` defaulting to `2.0`
  (`config.rs:1355-1356`, defaults at `config.rs:1621-1622`); the merge overwrites each when a
  scope sets the key (`config.rs:1817`, `config.rs:1820`).
- `agent_limits` wraps both in `Some(...)` when building `Limits.subagent`
  (`rust/fontaine-core/src/agent/setup.rs:220-221`), so **every** subagent is capped.
- `child_runner.rs:405` stops the child at the next event once
  `budget_tree.exhausted_of(&budget_id)` fires and reports `ChildStatus::BudgetExhausted` with
  the partial text (`child_runner.rs:446`); `rust/fontaine-agents/src/spawn.rs:121` wraps that as
  `[Partial result: the subagent stopped because its token budget is spent ...]`.
- `child_runner.rs:282` tells the user "the token budget still applies" whenever a child's cost
  budget is set on an unpriced model — unconditionally, so with no default token cap the notice
  would be wrong; `budget.rs`'s module doc makes the same claim ("the token cap still binds").
- `Budget.tokens` and `Budget.cost_usd` are already `Option` (`rust/fontaine-agents/src/budget.rs`)
  and `Budget::default()` is uncapped, so nothing downstream needs to learn what `None` means.
- The TUI already renders an uncapped row as `"<n> tok"` / `"$<x>"`
  (`rust/fontaine-tui/src/agents_panel.rs:86`); `ui/mod.rs:60` passes `row.budget` straight
  through.
- The config catalog rows print the resolved values directly
  (`rust/fontaine-core/src/config_catalog.rs:404`, :407); `opt(...)` (used by
  `agents.session_token_budget` at :411) is the existing helper for optional scalars.
- Docs state the defaults: `docs/configuration.md:687-688` and `docs/agents.md:137-138`;
  `docs/agents.md:83` warns that a fork's inherited context can burn the default budget;
  `docs/tui.md:667` shows a default capped panel row.

## Design

Make both resolved values optional and default them to unset:

1. `config.rs:1355-1356` — `pub subagent_token_budget: Option<u64>`,
   `pub subagent_cost_budget_usd: Option<f64>`.
2. `config.rs:1621-1622` — defaults `None`.
3. `config.rs:1817`, :1820 — merges set `Some(v)` (same shape as the session budgets below).
4. `agent/setup.rs:220-221` — pass both `Option`s through in `agent_limits` (drop the
   `Some(...)` wraps).
5. `config_catalog.rs:404`, :407 — render both with `opt(...)`.
6. `child_runner.rs:282` — the unpriced-cost notice's tail "the token budget still applies"
   only when `request.budget.tokens.is_some()`; `budget.rs`'s module doc becomes "a token cap,
   if set, still binds". The notice itself already fires only when a cost budget is set
   (`request.budget.cost_usd.is_some()`), so with no default cost cap it stays silent unless the
   user opted in.

That is the whole behavior change: with no keys set, `Limits.subagent` is fully `None`,
`Budget::exhausted` never fires for a subagent, and the child runs until it answers, is killed,
fails, or hits a session/max-turns limit.

No validation change: the keys already take any `u64` / finite non-negative `f64`, and a project
scope setting a key only ever adds a cap (tightens), which the scope rules allow.

## Implementation Steps

1. `rust/fontaine-core/src/config.rs`: make both resolved fields `Option`, defaults `None`,
   merges to `Some(v)`; extend the config integration tests for the resolved agents defaults.
2. `rust/fontaine-core/src/agent/setup.rs`: pass the `Option`s through in `agent_limits`.
3. `rust/fontaine-core/src/config_catalog.rs`: use `opt(...)` for both per-subagent budget rows.
4. Tests (below).
5. `rust/fontaine-core/src/child_runner.rs`: condition the unpriced-cost notice's token tail on
   a token budget being set; reword the `rust/fontaine-agents/src/budget.rs` module doc.
6. Docs: `docs/configuration.md` (:687-688, `[agents]` table — both rows become opt-in, default
   `unset`) and `docs/agents.md` (:137-138 table rows; reword :83 — a fork's inherited context
   is billed on its first call and counts against explicit budgets, not default ones; :150
   becomes "a cost cap, when set, …"). `docs/tui.md:667`: the agents panel example
   `1500 / 200000 tok · $0.0021 / $2.00` no longer matches a default session — show the uncapped
   form and note each `/ <cap>` form appears when that budget is set.
   `tasks/design/config.md:110-111`: comment out both sample lines.
7. `tasks/fontaine_harness_plan.md` `## Decisions`: dated entries that both per-subagent budgets
   are opt-in, for Claude Code parity (#285).
8. `CHANGELOG.md` `Changed` entry.

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/src/agent/setup.rs`
- `rust/fontaine-core/src/config_catalog.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-core/tests/config.rs`
- `rust/fontaine-agents/src/budget.rs` (module doc)
- `rust/fontaine-tui/src/agents_panel.rs` (test only)
- `rust/fontaine-tui/tests/agents.rs` (explicit token budget in one test's config)
- `docs/configuration.md`, `docs/agents.md`, `docs/tui.md`, `tasks/design/config.md`
- `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **Remove the defaults vs. raise the token cap to the model's context window.** The issue
  offers both. A raised cap is still a silent parity gap, still fires mid-work with a partial
  report, and its value would track whatever the largest context window happens to be. Removing
  it matches Claude Code exactly and keeps one knob with one meaning: unset = uncapped, set =
  cap.
- **Keep the `$2.0` cost-budget default while uncapping tokens.** Initially chosen to keep the
  change scoped to the issue's token cap; the maintainer then decided no per-subagent budget
  should bind unless explicitly set, so the cost default goes too (see Decisions).
- **Keep the resolved fields non-optional and use `0`/`u64::MAX` as "uncapped".** Rejected: the
  `Budget` type and the merge for session budgets already use `Option`; a sentinel would invent
  a second convention.

## Decisions

- 2026-10-28 (user): No default per-subagent token cap at all — not a raised one; `subagent_token_budget` is opt-in only, for Claude Code parity (#285).
- 2026-10-28 (user): No default per-subagent cost cap either — `subagent_cost_budget_usd` is opt-in only; no per-subagent budget binds unless explicitly set (#285).

## Documentation

`docs/configuration.md` (:687-688, `[agents]` table), `docs/agents.md` (:83 fork note, :137-138
limits table, :150 unpriced-cost note), `docs/tui.md` (:667 agents-panel example) and
`tasks/design/config.md` (:110-111 sample).

## Testing Strategy

- Config integration tests (`tests/config.rs`): the resolved defaults have both per-subagent
  budgets `None`; setting the keys resolves to `Some(v)`; outer-scope values survive an inner
  scope that does not set them.
- Catalog: `defaults_render_as_documented` asserts both rows render `unset`; the set-key case
  asserts the numbers.
- `agent_limits` unit coverage that unset keys yield `Limits.subagent.tokens == None` and
  `cost_usd == None`, and set keys yield `Some(v)` (the wiring the bug lived in).
- TUI `agents_panel.rs`: a row with `token_budget: None` and `cost_budget: None` renders
  `"<n> tok"` and `"$<x>"` with no `/ <cap>` — now the default row shape (the rendering branches
  existed but had no test).
- `rust/fontaine-tui/tests/agents.rs`
  (`a_killed_subagents_row_shows_killed_and_its_spend_against_its_budget`) asserts the capped
  form `1500 / 200000 tok` while relying on today's default: give it an explicit
  `[agents] subagent_token_budget = 200000` in its config so it keeps pinning the capped row.
- Existing integration tests `rust/fontaine/tests/parallel_agents.rs:234` and :290 set explicit
  budgets and keep passing unchanged — they pin that the opt-in caps still stop a child and
  return a partial report, which is the regression that matters once the defaults are gone.

No live or manual tier: exhaustion logic is already covered against explicit budgets, and the
new behavior is "no cap present", which the unit tests above reach directly.

## Open Questions

None.
