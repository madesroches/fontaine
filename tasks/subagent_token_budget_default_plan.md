# Subagents Uncapped by Default Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/285

## Overview

Remove the default 200,000-token cap on every subagent so a subagent runs until its task is
done, matching Claude Code, which places no token budget on Task agents. `subagent_token_budget`
stays as an opt-in cap for users who want per-agent spend control; the outer guardrails
(`--max-budget-usd`, session budgets, the opt-in per-subagent cost budget) are unchanged. Today
the cap makes failure correlate with task size: the largest delegations (a reviewer subagent over
a 51-file, +6,013-line diff) die mid-work at ~200k tokens with only a partial report.

## Current State

- `AgentsSection.subagent_token_budget: Option<u64>` is the raw config key
  (`rust/fontaine-core/src/config.rs:580`).
- The resolved `Agents.subagent_token_budget` is a **non-optional** `u64` defaulting to
  `200_000` (`config.rs:1355`, default at `config.rs:1621`); the merge overwrites it when a
  scope sets the key (`config.rs:1817`).
- `agent_limits` wraps it in `Some(...)` when building `Limits.subagent`
  (`rust/fontaine-core/src/agent/setup.rs:220`), so **every** subagent is capped.
- `child_runner.rs:405` stops the child at the next event once
  `budget_tree.exhausted_of(&budget_id)` fires and reports `ChildStatus::BudgetExhausted` with
  the partial text (`child_runner.rs:446`); `rust/fontaine-agents/src/spawn.rs:121` wraps that as
  `[Partial result: the subagent stopped because its token budget is spent ...]`.
- `child_runner.rs:282` tells the user "the token budget still applies" whenever a child's cost
  budget is set on an unpriced model — unconditionally, so with no default cap the notice would
  be wrong; `budget.rs`'s module doc makes the same claim ("the token cap still binds").
- `Budget.tokens` is already `Option<u64>` (`rust/fontaine-agents/src/budget.rs:27`) and
  `Budget::default()` is uncapped, so nothing downstream needs to learn what `None` means.
- The TUI already renders an uncapped row as `"<n> tok"` (`rust/fontaine-tui/src/agents_panel.rs:86`);
  `ui/mod.rs:60` passes `row.budget.tokens` straight through.
- The config catalog row prints the resolved value directly
  (`rust/fontaine-core/src/config_catalog.rs:404`); `opt(...)` (used by
  `agents.session_token_budget` at :412) is the existing helper for optional scalars.
- Docs state the `200000` default: `docs/configuration.md:687` and `docs/agents.md:137`;
  `docs/agents.md:83` warns that a fork's inherited context can burn the default budget.

## Design

Make the resolved value optional and default it to unset:

1. `config.rs:1355` — `pub subagent_token_budget: Option<u64>`.
2. `config.rs:1621` — default `subagent_token_budget: None`.
3. `config.rs:1817` — merge sets `t.subagent_token_budget = Some(v)` (same shape as
   `session_token_budget` two lines below).
4. `agent/setup.rs:220` — `tokens: config.agents.subagent_token_budget` (drop the `Some(...)`
   wrap).
5. `config_catalog.rs:404` — render with `opt(c.agents.subagent_token_budget)`.
6. `child_runner.rs:282` — the unpriced-cost notice's tail "the token budget still applies"
   only when `request.budget.tokens.is_some()`; `budget.rs`'s module doc becomes "a token cap,
   if set, still binds".

That is the whole behavior change: with no key set, `Limits.subagent.tokens` is `None`,
`Budget::exhausted` never fires on tokens, and the child runs until it answers, is killed, fails,
or hits a cost/session/max-turns limit. `subagent_cost_budget_usd` keeps its `2.0` default and
its non-optional resolved type — the issue is about the token cap only.

No validation change: the key already takes any `u64`, and a project scope setting the key only
ever adds a cap (tightens), which the scope rules allow.

## Implementation Steps

1. `rust/fontaine-core/src/config.rs`: make the resolved field `Option<u64>`, default `None`,
   merge to `Some(v)`; add the config unit tests for the resolved agents defaults.
2. `rust/fontaine-core/src/agent/setup.rs`: pass the `Option` through in `agent_limits`.
3. `rust/fontaine-core/src/config_catalog.rs`: use `opt(...)` for the
   `agents.subagent_token_budget` row.
4. Tests (below).
5. `rust/fontaine-core/src/child_runner.rs`: condition the unpriced-cost notice's token tail on
   a token budget being set; reword the `rust/fontaine-agents/src/budget.rs` module doc.
6. Docs: `docs/configuration.md:687` (description becomes "opt-in per-subagent token cap …",
   default column `200000` → none/unset) and `docs/agents.md` (:137 table row; reword :83 — a
   fork's inherited context is billed on its first call and counts against an explicit
   `subagent_token_budget` or the cost budget, not a default one). `docs/tui.md:667`: the agents
   panel example `1500 / 200000 tok · $0.0021 / $2.00` no longer matches a default session —
   show the uncapped form and note the `/ <cap>` form appears when a budget is set.
7. `tasks/fontaine_harness_plan.md` `## Decisions`: dated entry that subagents are uncapped by
   default for Claude Code parity (#285).
8. `CHANGELOG.md` `Changed` entry.

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/src/agent/setup.rs`
- `rust/fontaine-core/src/config_catalog.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-agents/src/budget.rs` (module doc)
- `rust/fontaine-tui/src/agents_panel.rs` (test only)
- `rust/fontaine-tui/tests/agents.rs` (explicit budget in one test's config)
- `docs/configuration.md`, `docs/agents.md`, `docs/tui.md`
- `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **Remove the default vs. raise it to the model's context window.** The issue offers both. A
  raised cap is still a silent parity gap, still fires mid-work with a partial report, and its
  value would track whatever the largest context window happens to be. Removing it matches
  Claude Code exactly and keeps one knob with one meaning: unset = uncapped, set = cap.
- **Drop the cost-budget default too.** Rejected: the issue and this change are scoped to the
  token cap; the `$2.0` per-subagent cost default is a separate spend-control question and stays.
- **Keep the resolved field non-optional and use `0`/`u64::MAX` as "uncapped".** Rejected: the
  `Budget` type and the merge for session budgets already use `Option`; a sentinel would invent
  a second convention.

## Decisions

- 2026-10-28 (user): No default per-subagent token cap at all — not a raised one; `subagent_token_budget` is opt-in only, for Claude Code parity (#285).

## Documentation

`docs/configuration.md` (:687, `[agents]` table), `docs/agents.md` (:83 fork note, :137
limits table) and `docs/tui.md` (:667 agents-panel example).

## Testing Strategy

- Config unit tests (`config.rs`): the resolved default has `subagent_token_budget == None`;
  setting `[agents] subagent_token_budget` resolves to `Some(v)`; an outer-scope value survives
  an inner scope that does not set the key.
- Catalog: extend `defaults_render_as_documented` in `config_catalog.rs` with
  `effective("agents.subagent_token_budget", &c) == "unset"` (as `agents.session_token_budget`
  renders), plus a set-key case asserting the number.
- `agent_limits` unit coverage that an unset key yields `Limits.subagent.tokens == None` and a
  set key yields `Some(v)` (the wiring the bug lived in).
- TUI `agents_panel.rs`: a row with `token_budget: None` renders `"<n> tok"` (the rendering
  branch exists today but has no test).
- `rust/fontaine-tui/tests/agents.rs`
  (`a_killed_subagents_row_shows_killed_and_its_spend_against_its_budget`) asserts the capped
  form `1500 / 200000 tok` while relying on today's default: give it an explicit
  `[agents] subagent_token_budget = 200000` in its config so it keeps pinning the capped row.
- Existing integration tests `rust/fontaine/tests/parallel_agents.rs:234` and :290 set explicit
  budgets and keep passing unchanged — they pin that the opt-in cap still stops a child and
  returns a partial report, which is the regression that matters once the default is gone.

No live or manual tier: exhaustion logic is already covered against explicit budgets, and the
new behavior is "no cap present", which the unit tests above reach directly.

## Open Questions

None.
