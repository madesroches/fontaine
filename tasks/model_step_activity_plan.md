# Model-Step Activity Indicator Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/343

## Overview
The ~60 s gaps in #343 are model output time (reasoning and tool-call arguments being written),
not a harness stall. The defect is that nothing shows it: the drive loop drops `StreamEvent::Reasoning`
and `StreamEvent::Arguments`, and tokens/cost only update at `CompletionCall`. This plan surfaces a
live count of characters written in the current model step, per agent, in the agent
list (the `main` row included, whenever the list is shown) and the agents panel (subagents only), and adds debug logs for model-request start and end so a diagnosis like the
one in the issue is possible from logs alone.

## Current State
- `fontaine-core/src/turn.rs` (`drive_once` loop, ~l.340; `drive` is only its retry wrapper): only `StreamEvent::Text` (→ `AssistantDelta`)
  and `End` are handled; `_ => Flow::Continue` drops `Reasoning` and `Arguments`.
- `CompletionCall` debits the agent via `AgentTree::debit` (`fontaine-agents/src/tree.rs`), which
  is the only moment `AgentRow.spend` changes.
- `AgentRow` (`tree.rs:168`) feeds `agent_list.rs` (`ListRow`, elapsed + `spend_text`) and
  `agents_panel.rs` (`PanelRow`).
- The tree wakes watchers through `changed: Notify`; join and budget watchers depend on it.

## Design
- **Counter in the tree.** Add `AgentTree::note_writing(agent, chars)`: adds to a per-node
  `writing: u64` and, throttled (at most one `display_changed` notification per ~250 ms per tree, measured with
  `tokio::time::Instant`), wakes watchers. `reset_writing(agent)` zeroes it and always notifies
  `display_changed`, bypassing the throttle. Neither touches `changed`. `AgentRow` gains `writing: u64`. The root is a tree node but never an `AgentRow`
  (`rows()` keeps `parent.is_some()`), so add an accessor `AgentTree::writing_of(agent) -> u64` that
  works for any node, root included.
  It is display state: not part of `Spend`, never budgeted, never persisted.
- **`drive_once` feeds it.** On `StreamEvent::Reasoning { text, .. }` add `text.chars().count()`; on
  `StreamEvent::Arguments { json, .. }` add `json.chars().count()`; `Text` also counts (the fragment
  still goes out as `AssistantDelta` unchanged). Reset on each `CompletionCall`, and via a drop guard
  in `drive_once` that calls `reset_writing` (following the `EndTurnOnDrop` pattern), because `run_turn`
  drops the future at an await on interrupt and code after the loop never runs; so a finished,
  failed or cancelled step never shows stale progress.
  No new `SessionEvent` variant: the tree is the channel, because it already serves the root and
  subagents uniformly and the list/panel already read it.
- **TUI.** `ListRow` and `PanelRow` get `writing: u64`, a separate field, not folded into the `spend` string. While the agent is running and
  `writing > 0`, append `· writing 4.2k chars` (format `<n>` below 1000, else `<n.n>k`) after the
  spend text (in the list it is dropped before the `+ unpriced` marker is truncated; in the panel it
  goes on the spend line). `ListRow::main()` stays a constant row,
  but the list builder sets its `writing` from `writing_of(ROOT_AGENT)`; it renders the suffix on
  `writing > 0` alone (main has no Running status). List visibility is unchanged: `main` appears only
  while at least one subagent row is listed, so a root step with no subagents shows no indicator, and
  the agents panel has no main row. One shared formatter next to `spend_text` in
  `agents_panel.rs`. The TUI also redraws on the tree's `display_changed` (it selects on it beside
  `tree.changed().notified()` and re-reads `tree.rows()`; the 1 s list tick runs only while subagent rows exist), so the throttled notify is
  what drives updates. There is no trailing notify: a stale count after the model pauses is accepted
  until the next event.
- **Debug logs.** In `drive_once`, `tracing::debug!(agent.id = ..., ...)` at model
  request start and request end with elapsed ms and characters written. A request is timed and
  logged from its first stream item, so tool and approval time are excluded and `elapsed_ms` omits
  time-to-first-token. A request that errors, hangs or is cancelled before its first item logs
  nothing; its failure is reported on the normal path. Once started, the end log is emitted from the
  drop guard (so on all exit paths): completion, failure and cancellation each log exactly once.
  `drive_once` keeps a local `Instant` and a local char counter and reads both for the end log before
  `reset_writing`. Each `drive` retry runs a new `drive_once`. Debug level, not telemetry spans, so no
  telemetry doc contract changes.
- **Agents panel last tool call + age** (issue's other suggestion): out of scope here; file a
  follow-up issue if wanted.
- **Killed agents' transcripts** (issue side finding): out of scope; file a separate issue.

## Implementation Steps
1. `fontaine-agents/src/tree.rs`: add `writing` to the node and `AgentRow`; add `note_writing` (throttled
   notify) and `reset_writing`; populate in the snapshot; add `writing_of`.
2. `fontaine-core/src/turn.rs` (`drive_once`): handle `Reasoning`/`Arguments`/`Text` counts; reset on `CompletionCall` plus drop guard; debug logs.
3. `fontaine-tui/src/agents_panel.rs`: `PanelRow.writing`, formatter, render; `agent_list.rs`:
   `ListRow.writing` (main's set from `writing_of`), render; update the mapping sites: `agent_list.rs::rows()` and `ui/mod.rs::refresh_panel`. Update every
   `ListRow`, `PanelRow` and `AgentRow` struct literal, including tests: `ListRow` at
   `agent_list.rs:33,71`, `app.rs:1453,1465`, `view.rs:582,693`; `PanelRow` at `ui/mod.rs:93`,
   `agents_panel.rs:299`, `screen.rs:1766`, `app.rs:1683`; `AgentRow` at `tree.rs:916`,
   `agent_list.rs:340`.
4. Docs: `docs/tui.md` (agent list / agents panel sections) and `docs/agents.md` mention.
5. CHANGELOG entry.

## Files to Modify
- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-tui/src/agents_panel.rs`, `rust/fontaine-tui/src/agent_list.rs` (`agent_list.rs::rows()`)
- `rust/fontaine-tui/src/app.rs`, `rust/fontaine-tui/src/view.rs`, `rust/fontaine-tui/src/screen.rs`,
  `rust/fontaine-tui/src/ui/mod.rs` (`refresh_panel` mapping, struct literals)
- `docs/tui.md`, `docs/agents.md`, `CHANGELOG.md`


## Decisions
### Model-step activity
- 2026-10-31: the indicator reads `· writing N chars` (`<n>` below 1000, else `<n.n>k`); chars, not token estimates.
- 2026-10-31: tree wakeups from `note_writing` are throttled to ~250 ms per tree; `reset_writing` bypasses the throttle.
- 2026-10-31: display state has its own `Notify` (`display_changed`); waking the shared `changed` flaked the headless budget kill.
- 2026-10-31: AFK: the root's indicator is read through `AgentTree::writing_of` and shown on the `main` list row only while the list is shown (a subagent row exists); the agents panel stays subagents-only, since the root is never an `AgentRow` and widening list/panel visibility is out of scope for #343.

## Documentation
`docs/tui.md` agent list / agents panel: describe the `· writing 4.2k chars` indicator and that it
clears when the model step ends. `docs/agents.md`: one line.

## Testing Strategy
- Unit (tree): `note_writing` accumulates, `reset_writing` zeroes, snapshot exposes it, notify is
  throttled (many calls → bounded wakeups), unknown agent id is a no-op, `writing_of` works for the root.
- Unit (tui): main row shows the suffix when its `writing > 0` with a subagent row listed; no list (hence no main) without subagent rows.
- Unit (tui): formatter (`999`, `1.0k`, `12.3k`), list/panel row render with and without `writing`,
  not shown for non-running agents.
- Integration (core, `fontaine-core/tests/billing.rs` style): call `drive` (or `run_turn`) with `MockStreamEvent`
  (`ReasoningDelta`, `ToolCallNameDelta`, `ToolCallArgumentsDelta`, then `Text`) and read
  `agents().rows()` inside `on_event` on `AssistantDelta` (`Usage`/`on_event` don't fire on
  Reasoning/Arguments alone); assert the counter (via `writing_of(ROOT_AGENT)` for the root, since `rows()` omits it) is nonzero mid-step and zero after the
  `CompletionCall`. Pins the #343 bug: counter stayed invisible during reasoning-only steps.
- Narrow-width list row with `+ unpriced` and `writing > 0` keeps the marker (the writing suffix is added only if the row still fits).
- Dropping the `drive` future mid-stream zeroes the counter.
- A failed or retried step zeroes the counter.
