# Goal Token and Wall-Clock Budgets Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/444

## Overview

`/goal` bounds an unattended stretch by `max_turns` and an optional `max_cost_usd`. A cost cap does
nothing for an unpriced model and a turn cap bounds neither token-heavy turns nor elapsed time. Add
two optional user-scope-only keys, `[goal] max_tokens` and `[goal] max_duration_secs`, checked at
the same evaluation boundary as the other caps, pausing the goal with a specific reason.

## Current State

- `rust/fontaine-core/src/goal.rs`: `GoalCaps { max_turns, max_cost_usd }` (built `From<&config::Goal>`);
  `PauseReason` (`TurnCap`, `CostCap`, ...) with `Display`; `Stretch { turns, idle_verdicts,
  spend_base }` is reset by `fresh()` and `user_prompt()`; `not_met()` (called from `apply()`) holds
  the cap checks in the order no-progress, turns, cost, using `Budget::exhausted` for the cost test.
- `rust/fontaine-core/src/agent/goal.rs::evaluate_goal` passes `tree_spend()` (main, guardian,
  evaluator, compaction, subagents) into `apply()`; `spend_since` already carries unpriced tokens
  inside `input_tokens`/`output_tokens`, so `Spend::tokens()` counts them.
- `rust/fontaine-agents/src/budget.rs::Budget` supports a token cap whether or not a price exists.
- Config: `GoalSection` (config.rs ~1235), validation (~2133), merged `Goal` (~2803), merge (~3405),
  catalog row `goal.max_cost_usd` (config_catalog.rs:584), scope rule `goal` user-or-CLI
  (config_scope.rs:128/175/671). A new key under `[goal]` inherits the scope rule.
- Headless and TUI render a pause through `PauseReason::notice()`/`Display`; a pause is exit 1 with
  an unchanged `subtype`. No headless schema change is needed if the reason is only a new variant.
- Docs: `docs/configuration.md` (`[goal]`), `docs/tui.md` (Goals / Pausing), `docs/headless.md`
  (Goals).

## Design

- `config::GoalSection` / `config::Goal`: add `max_tokens: Option<u64>` (valid: at least 1) and
  `max_duration_secs: Option<u64>` (valid: at least 1, at most `MAX_GOAL_DURATION_SECS`, 30 days, so
  `Duration` arithmetic cannot overflow). Unset means no bound. Merge like `max_cost_usd`.
  Config validation errors name `goal.max_tokens` / `goal.max_duration_secs`.
- `GoalCaps`: add `max_tokens: Option<u64>` and `max_duration: Option<Duration>`; `GoalCaps` stays
  `Copy`.
- `PauseReason`: add `TokenCap(u64)` ("the goal's tokens passed 500.0k", via `compact_count`) and
  `DurationCap(Duration)` ("the stretch ran longer than 2h 00m", via `duration_text`; the clock is per stretch while status shows goal-wide elapsed). Not `Impossible`,
  no new headless outcome, no new exit code.
- `Stretch`: add `started: Instant` and `paused_base: Duration` (the cumulative usage-pause time at
  stretch start), set wherever the stretch is created (`fresh`, `user_prompt`) so tokens, duration,
  turns and cost all reset at the same boundaries.
- `not_met()`: after the cost check, test tokens with `Budget { tokens: Some(cap), cost_usd: None }
  .exhausted(&spent, false)` (no `priced` guard: it is the point) and duration with
  `active_time > cap` (strict, matching `Budget::exhausted`'s `spent > cap`), where `active_time =
  stretch.started.elapsed()` less the usage-pause time accrued inside the stretch (`paused_now -
  paused_base`, saturating; pause time from before the stretch counts for nothing). Order of precedence: no-progress, turns, cost, tokens, duration.
  Like the other caps it fires on the "not met" boundary only; a met/impossible verdict ends the goal
  regardless. This is not hard cancellation: an in-flight model call or tool wait is not interrupted,
  so a stretch may overrun the duration by one turn plus one evaluation.
- No model tool is added; the model cannot raise budgets.
- Status/pause display: cap reasons reach `/goal` status and the headless/TUI notices unchanged.
  They do not reach ledger pause records: `evaluate_goal` records `Paused` with reason `None` for
  `Step::Pause` (only the AFK `pause_goal` records text).

## Implementation Steps

1. `config.rs`: fields, validation, `Goal` defaults (`None`), merge; `config_catalog.rs`: two rows
   beside `goal.max_cost_usd`; schema regeneration if the repo snapshots it.
2. `goal.rs`: `GoalCaps`, `PauseReason` variants and `Display`, `Stretch.started`, `not_met()` checks.
3. Fix the three `GoalCaps` struct literals in `rust/fontaine-core/tests/goal.rs` (approx. lines 37,
   367, 395); the `config::Goal` literal at `config.rs:3119` is covered by step 1.
4. Tests (below).
5. Docs and changelog; master plan Decisions line.

## Files to Modify

- `rust/fontaine-core/src/{config.rs,config_catalog.rs,goal.rs}`
- `rust/fontaine-core/tests/{goal.rs,config.rs}` (and `config_scope.rs` test table if it lists keys)
- `docs/{configuration.md,tui.md,headless.md}`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- Reuse `Budget::exhausted` for tokens rather than a hand-written comparison: same semantics as the
  agent-tree caps.
- Elapsed time per stretch (not per goal) matches the other caps; the status already shows goal-wide
  elapsed.
- Duration is checked at the evaluation boundary only, not by a timer task: no new concurrency, at
  the cost of an overrun of one turn. Documented.

## Decisions

- Kimi's model-controlled `SetGoalBudget` is not adopted: the independent evaluator stays and
  budgets are user-scope-only config.

## Documentation

- `docs/configuration.md`: two table rows plus example; `docs/tui.md`: two pause bullets and the
  overrun caveat; `docs/headless.md`: add the keys to the pause list; `CHANGELOG.md`.
- `tasks/fontaine_harness_plan.md` `## Decisions` (goal subsection): dated line that stretches are
  also capped by optional `[goal] max_tokens` and `max_duration_secs`, beyond Claude Code's
  no-progress cap, so unpriced models and slow turns are bounded.

## Testing Strategy

Unit tests in `fontaine-core/tests/goal.rs`, following the existing cost-cap tests:
- Token cap fires with an unpriced model (`priced = false`) and counts spend built as a tree total
  including guardian/evaluator/subagent tokens via `spend_since`.
- Duration cap fires once the stretch's `started` is strictly older than the cap (use a tiny cap,
  e.g. 1 s, with a sleep, or a test-only constructor), and does not fire at or below it.
- Usage-pause time accrued inside the stretch is subtracted from the duration; with `paused_base ==
  paused_now` nothing is subtracted, however large the pause total.
- Both reset on `user_prompt`: a paused goal continues under a fresh stretch.
- Pause leaves goal set; ordering vs. turn/cost caps; unset caps change nothing.
- `PauseReason` display strings.

`fontaine-core/tests/config.rs`: defaults `None`; user file sets both; project scope and `--settings`
behaviour matches `goal.max_cost_usd` (project rejected); invalid values: 0 and over-max
reach `validate_values` and the error names `goal.max_tokens`/`goal.max_duration_secs`; negative
and non-integer values fail in toml parsing (`ConfigError::Parse`), so assert only that an error is
returned. A headless test in `fontaine/tests/goal_headless.rs` for
a token-capped goal pausing with exit 1 and unchanged `subtype`.

## Open Questions

None.
