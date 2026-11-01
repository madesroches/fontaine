# Compact Counts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/365

## Overview
Counts shown to a human in the TUI render inconsistently: agent spend prints raw tokens
(`90887 tok`) next to chars with a `k` suffix (`writing 10.3k chars`). Three private formatters
exist today. Replace them with one shared `compact_count` helper (`999`, `1.0k`, `90.9k`, `1.2M`)
and route every display site through it. Machine-readable output keeps exact values.

## Current State
Three divergent formatters:
- `fontaine-tui/src/agents_panel.rs:95-98` `spend_text` prints `{tokens} tok` and `{cap} tok` raw
  (used by the agent tree and list rows, `agent_list.rs`).
- `fontaine-tui/src/agents_panel.rs:117` `writing_text`: `< 1000` exact, else `{:.1}k`; no `M`.
- `fontaine-tui/src/reports.rs:399` `tokens()`: `{:.1}k`, no `M`; used by `/context`
  (`reports.rs:429-523`) and `ui/commands/context.rs:88-90` (compaction message). `window_label`
  (`reports.rs:407`) prints exact-thousand windows as `200k`.
- `fontaine-core/src/goal.rs:204` `tokens_text`: `182k` (truncating) / `1.2M`; used by the goal
  summary (`goal.rs:186`, `goal.rs:293`).
- The headless `-p "/goal"` result text (`fontaine/src/headless/goal.rs:91`, via `GoalStatus::describe()`)
  carries the goal figures, so it changes from `182k` to `182.0k`.

- `fontaine-agents/src/budget.rs:55,61` `Exhausted::describe()` prints raw `({spent} of {cap})`;
  it reaches the agent tree via `fontaine-tui/src/ui/mod.rs:114` and `agents_panel.rs:264-265`,
  under the compacted spend line.

Not display-only, left alone: `fontaine-core/src/project_env.rs:592` `usage_line` (shown to humans
in the TUI transcript and setup/critique reports, but the same string is the machine-readable
JSON field `scan.usage`, `project_env.rs:526`, `auto_mode.rs:402`), the ledger, audit log,
headless JSON, telemetry.

## Design
New `pub fn compact_count(n: u64) -> String` in `fontaine-tools/src/display.rs` (already the home
of terminal-display helpers, depended on by both `fontaine-core` and `fontaine-tui`; update the
module doc to cover "making values readable on a terminal").

Rules (integer math, no float rounding surprises):
- `n < 1_000`: exact (`999`).
- `n < 1_000_000`: tenths of a thousand, rounded half up: `t = n.saturating_add(50) / 100`. If `t < 10_000`,
  render `{t/10}.{t%10}k` (`1000` → `1.0k`, `9950` → `10.0k`, `90887` → `90.9k`). If rounding
  reaches `1000.0k` (`t == 10_000`, i.e. `n ≥ 999_950`) fall through to the M form, so `999_999`
  → `1.0M`.
- `n ≥ 1_000_000`: tenths of a million, rounded half up: `n.saturating_add(50_000) / 100_000`,
  rendered `{t/10}.{t%10}M` (`1_000_000` → `1.0M`, `1_234_567` → `1.2M`).
- Always one decimal in the k/M forms, so widths are stable; no `B` tier (u64 counts of that
  size render as e.g. `12345.7M`, acceptable for a token count).

Call sites switched to it:
- `spend_text`: `{compact(tokens)} / {compact(cap)} tok` (the cost half is unchanged).
- `writing_text`: `writing {compact(chars)} chars`.
- `Exhausted::describe()`: `({compact(spent)} of {compact(cap)})`.
- `reports::tokens` becomes a thin removal: delete it and call `compact_count` directly in
  `reports.rs` and `context.rs`; `window_label` uses `compact_count` too (`200000` → `200.0k`,
  `1_000_000` → `1.0M`).
- `goal::tokens_text` removed; goal summary uses `compact_count`.

## Implementation Steps
1. Add `compact_count` + unit tests to `fontaine-tools/src/display.rs`.
2. Replace `spend_text`, `writing_text` internals in `fontaine-tui/src/agents_panel.rs`, and
   route `Exhausted::describe()` in `fontaine-agents/src/budget.rs` through `compact_count`.
3. Delete `reports::tokens`, update `reports.rs`, `ui/commands/context.rs`, `window_label`.
4. Delete `goal::tokens_text`, update `goal.rs` call sites and any tests asserting old output.
5. Update existing test expectations to the compact forms: `agent_list.rs:416,474`;
   `agents_panel.rs:346-396`; `reports.rs:941,953`; `rust/fontaine-tui/tests/agents.rs:66,71`;
   `rust/fontaine-core/tests/instructions.rs:400` (`"41000 characters"` -> `"41.0k characters"`);
   plus any `budget.rs` test asserting `describe()` output.
6. Route the remaining display sites through `compact_count`:
   `fontaine-tui/src/ui/commands/reports.rs:91-93` (`copied reply {n} ({} characters)`) and
   `fontaine-core/src/instructions/mod.rs:177` (`{path} ({size} characters)` startup
   warnings) and `:181` (the "instruction files add up to {total} characters" sentence); both
   sizes are `usize` and need a `u64` conversion to call `compact_count`. Messages echoing a configured limit (`goal.rs:87`, `session.rs:756`, `spawn.rs:415`,
   `memory.rs:163`, the KiB constants in `transcript.rs:1075,1281`) and the markdown export
   (`export.rs:300`) stay exact. Finish with an `rg` sanity check for stragglers
   (`tok`, `tokens`, `chars`, `characters`).
7. Docs and changelog.

## Files to Modify
- `rust/fontaine-tools/src/display.rs`
- `rust/fontaine-tui/src/agents_panel.rs`, `agent_list.rs` (tests), `reports.rs`,
  `ui/commands/context.rs`, `rust/fontaine-tui/tests/agents.rs`, `rust/fontaine-core/tests/instructions.rs` (test expectations)
- `rust/fontaine-agents/src/budget.rs`
- `rust/fontaine-tui/src/ui/commands/reports.rs`
- `rust/fontaine-core/src/goal.rs`, `rust/fontaine-core/src/instructions/mod.rs`
- `docs/tui.md`, `CHANGELOG.md`

## Trade-offs
- A new crate or a core→tui dependency is unwarranted.
- One decimal always (`1.0k`) over trimming `.0`: stable width in the k/M forms. `window_label`
  adopts `200.0k` for consistency with every other count, not for column width.
- Round half up vs truncate (old `tokens_text` truncated): matches the issue's `90.9k` for 90887
  and the `9950` boundary case.

## Decisions
- 2026-11-01: `usage_line` (`project_env.rs`) keeps exact counts — the same string is the
  machine-readable JSON field `scan.usage`.

## Documentation
`docs/tui.md` lines ~795, 856-861 (and the `182k tokens` example at 752 → `182.0k`; the
"plain below 1000, then in thousands" sentence at 803 → mention `M`): update the example spend strings (`1500 tok`,
`1500 / 200000 tok`, `4863024 tok`) to compact forms (`1.5k tok`, `1.5k / 200.0k tok`,
`4.9M tok`) and state that displayed counts are compact while ledger/audit/JSON are exact.
Update the doc comment at `fontaine-core/src/goal.rs:175`. `CHANGELOG.md` entry.

## Testing Strategy
Unit tests (all behavior is pure formatting):
- `compact_count` boundaries: 0, 999, 1000, 1049, 1050, 9950, 99_949, 999_949, 999_950, 999_999,
  1_000_000, 1_234_567, `u64::MAX` (no panic/overflow — use saturating add).
- Existing row/report/goal tests updated.

## Open Questions
None.
