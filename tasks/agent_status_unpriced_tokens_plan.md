# Agent Status: Priced Tokens With Their Price, Then Unpriced Count Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/350

## Overview

An agent's spend text (`4.9M tok · $0.0519 + unpriced`) shows a raw total, hides how many tokens
are unpriced, and runs into the age with no separator. Replace it with the priced tokens next to
their price, followed by a separate `N unpriced tok` segment, joined to the age by ` · `:

```
◯ agent-4 explore running 52s · 110k tok · $0.0066 · 128k unpriced tok · writing 736 chars
```

## Current State

- `rust/fontaine-tui/src/agents_panel.rs:80-117`: `SpendView` carries `cost_partial: bool`;
  `spend_text` renders `{tokens} tok · {cost}` and appends ` + unpriced` to the cost when
  `cost_partial && cost_usd.is_some()`. `PanelRow` (line ~32) also carries `cost_partial`.
- Call sites: `agent_list.rs:90` (list rows) and `agents_panel.rs:247` (panel rows), built in
  `ui/mod.rs:131` from a `Spend`; `screen.rs:1965` and `app.rs:1884` build `PanelRow`s in tests.
- `agent_list.rs:~355` joins age and spend with `format!(" {}", row.spend)`, and the narrow-row
  logic (`~393`) checks `spend.ends_with(" + unpriced")` to cut the head, not the tail, so a
  partial cost never reads as complete.
- `fontaine_agents::Spend` (`budget.rs`) has `unpriced_tokens`; `Budget::exhausted` caps
  `spend.tokens()` (the total).
- `compact_count` (`fontaine-tools/src/display.rs:24`) yields `1.0k`, `200.0k`, `4.9M`.
- Docs: `docs/tui.md` lines ~846 and ~905-912.

## Design

`SpendView` and `PanelRow` replace `cost_partial: bool` with `unpriced_tokens: u64`. Both call
sites copy `row.spend.unpriced_tokens`.

`spend_text` builds segments joined by ` · `:

1. Tokens: `priced = tokens - unpriced_tokens` (saturating).
   - No token budget: `{priced} tok`; omitted when `priced == 0` and `unpriced_tokens > 0`.
   - Token budget set: the total stays the budgeted figure: `{total} / {cap} tok`, since the cap
     binds on the total.
2. Cost (unchanged): `$x`, `$x / $cap`, or `cost n/a`; no ` + unpriced` suffix.
3. Unpriced: `{n} unpriced tok` when `unpriced_tokens > 0`.

Examples: `110k tok · $0.0066 · 128k unpriced tok`; `238k / 500k tok · $0.0066 · 128k unpriced tok`;
fully unpriced `4.9M unpriced tok`; fully unpriced with a cost budget `cost n/a · 4.9M unpriced tok`;
fully unpriced with a token budget and a cost budget `4.9M / 5M tok · cost n/a · 4.9M unpriced tok`;
fully priced unchanged.

Token counts in these segments trim a trailing `.0` (`110k`, not `110.0k`), to match the issue's
examples. `compact_count` is shared with other output whose documented form is `1.0k`, so the trim
is a small local helper in `agents_panel.rs` over `compact_count`, applied to the spend segments
(including the token-budget cap), to `writing_text`, and to the `stopped_because` text built in
`ui/mod.rs:134` (which would otherwise show untrimmed counts under a trimmed spend line).

Narrow-row truncation in `agent_list.rs`: `AgentList::lines` joins age and spend with ` · `
(`format!(" · {}", row.spend)`, with the width arithmetic following). The special case re-keys
from `ends_with(" + unpriced")` to the spend containing an unpriced segment — the row's
`ListRow` gains `unpriced: bool` rather than string-matching rendered text — and still cuts the
head (`…` + spend) so the unpriced figure is never cut off. The writing suffix is still dropped
first.

## Implementation Steps

1. `agents_panel.rs`: swap the field on `SpendView` and `PanelRow`; rewrite `spend_text`; add the
   trim helper and use it in `writing_text`; update panel call site and the in-file tests.
2. `agent_list.rs`: pass `unpriced_tokens`, add `ListRow.unpriced`, change the join to ` · `,
   re-key the truncation, update tests.
3. `ui/mod.rs`, `screen.rs`, `app.rs`, `spinner.rs`, `view.rs`: build `PanelRow` with
   `unpriced_tokens`; pass the trim helper to `describe_with` for `stopped_because` in `ui/mod.rs`;
   set `unpriced` in the test-only `ListRow` literals (`spinner.rs:418`, `view.rs:779`,
   `app.rs:1637`, `app.rs:1651`; `ListRow` has no `Default`).
4. `docs/tui.md`: update the agent-list and agents-panel sections and examples.
5. `CHANGELOG.md` entry is handled by the `pr` step.

## Files to Modify

- `rust/fontaine-tui/src/agents_panel.rs`, `agent_list.rs`, `ui/mod.rs`, `screen.rs`, `app.rs`, `spinner.rs`, `view.rs`
- `docs/tui.md`

## Decisions

- 2026-11-02: trim `.0` only in agent spend and writing text, not in `compact_count` since `compact_count` feeds other output documented as `1.0k`.

## Documentation

`docs/tui.md`: agent-list paragraph (age is followed by ` · `, drop "`+ unpriced` marker", describe
the `N unpriced tok` segment) and agents-panel paragraph (replace the `4863024 tok · $0.0519 +
unpriced` example, state that a token budget keeps the total, that a fully unpriced agent reads
`4.9M unpriced tok`, and that counts drop a trailing `.0`; fix the `1.5k / 200.0k tok` examples), and line ~909 "Displayed counts are compact (`999`,
`1.0k`, `1.2M`)" (agent spend and writing text drop a trailing `.0`).

## Testing Strategy

Unit tests (all reachable by constructed inputs):
- `spend_text`: partial with/without cost budget, with a token budget (total kept), fully unpriced
  (no cost, and with `cost n/a` under a cost budget), fully priced, `.0` trimming.
- `agent_list` rows: spend string and ` · ` join after the age; narrow row keeps the unpriced
  segment (head cut with `…`), drops `writing` first; an ordinary narrow row is still cut at the
  end.
- Panel rendering test updated for the new format.
