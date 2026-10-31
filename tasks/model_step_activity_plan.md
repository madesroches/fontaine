# Model-Step Activity Indicator Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/343

## Overview
The ~60 s gaps in #343 are model output time (reasoning and tool-call arguments being written),
not a harness stall. The defect is that nothing shows it: `drive` drops `StreamEvent::Reasoning`
and `StreamEvent::Arguments`, and tokens/cost only update at `CompletionCall`. This plan surfaces a
live count of characters written in the current model step, per agent (main included), in the agent
list and agents panel, and adds debug logs for model-request start and end so a diagnosis like the
one in the issue is possible from logs alone.

## Current State
- `fontaine-core/src/turn.rs` (`drive` loop, ~l.340): only `StreamEvent::Text` (→ `AssistantDelta`)
  and `End` are handled; `_ => Flow::Continue` drops `Reasoning` and `Arguments`.
- `CompletionCall` debits the agent via `AgentTree::debit` (`fontaine-agents/src/tree.rs`), which
  is the only moment `AgentRow.spend` changes.
- `AgentRow` (`tree.rs:168`) feeds `agent_list.rs` (`ListRow`, elapsed + `spend_text`) and
  `agents_panel.rs` (`PanelRow`).
- The tree wakes watchers through `changed: Notify`.

## Design
- **Counter in the tree.** Add `AgentTree::note_writing(agent, chars)`: adds to a per-node
  `writing: u64` and, throttled (at most one `changed` notification per ~250 ms per tree, measured with
  `tokio::time::Instant`), wakes watchers. `reset_writing(agent)` zeroes it and always notifies,
  bypassing the throttle. `AgentRow` gains `writing: u64`.
  It is display state: not part of `Spend`, never budgeted, never persisted.
- **`drive` feeds it.** On `StreamEvent::Reasoning { text, .. }` add `text.chars().count()`; on
  `StreamEvent::Arguments { json, .. }` add `json.chars().count()`; `Text` also counts (the fragment
  still goes out as `AssistantDelta` unchanged). Reset on `CompletionCall` and when the drive loop
  ends (including failure/stop), so a finished step never shows stale progress.
  No new `SessionEvent` variant: the tree is the channel, because it already serves the root and
  subagents uniformly and the list/panel already read it.
- **TUI.** `ListRow` and `PanelRow` get `writing: u64`. While the agent is running and
  `writing > 0`, append `· writing 4.2k chars` (format `<n>` below 1000, else `<n.n>k`) after the
  spend text. `main`'s row uses the same field. One shared formatter next to `spend_text` in
  `agents_panel.rs`. The TUI redraws on tree `changed` (it selects on `tree.changed().notified()`
  and re-reads `tree.rows()`; the 1 s list tick is only a backstop), so the throttled notify is
  what drives updates.
- **Debug logs.** In `drive`, `tracing::debug!(agent.id = ..., ...)` at model
  request start and, at `CompletionCall`/failure, request end with elapsed ms and characters
  written. Debug level, not telemetry spans, so no telemetry doc contract changes.
- **Agents panel last tool call + age** (issue's other suggestion): out of scope here; file a
  follow-up issue if wanted.
- **Killed agents' transcripts** (issue side finding): out of scope; file a separate issue.

## Implementation Steps
1. `fontaine-agents/src/tree.rs`: add `writing` to the node and `AgentRow`; add `note_writing` (throttled
   notify) and `reset_writing`; populate in the snapshot.
2. `fontaine-core/src/turn.rs`: handle `Reasoning`/`Arguments`/`Text` counts; reset points; debug logs.
3. `fontaine-tui/src/agents_panel.rs`: `PanelRow.writing`, formatter, render; `agent_list.rs`:
   `ListRow.writing`, render; update the driver code that maps `AgentRow` to both. Update every
   `ListRow`, `PanelRow` and `AgentRow` struct literal, including tests: `ListRow` at
   `agent_list.rs:33,71`, `app.rs:1453,1465`, `view.rs:582,693`; `PanelRow` at `ui/mod.rs:93`,
   `agents_panel.rs:299`, `screen.rs:1766`, `app.rs:1683`; `AgentRow` at `tree.rs:916`,
   `agent_list.rs:340`.
4. Docs: `docs/tui.md` (agent list / agents panel sections) and `docs/agents.md` mention.
5. CHANGELOG entry.

## Files to Modify
- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-tui/src/agents_panel.rs`, `rust/fontaine-tui/src/agent_list.rs` (and the row mapping site)
- `rust/fontaine-tui/src/app.rs`, `rust/fontaine-tui/src/view.rs`, `rust/fontaine-tui/src/screen.rs`,
  `rust/fontaine-tui/src/ui/mod.rs` (struct literals)
- `docs/tui.md`, `docs/agents.md`, `CHANGELOG.md`

## Trade-offs
- Token estimates instead of chars rejected: no tokenizer is available mid-stream; chars are exact
  and enough to show motion.
- Unthrottled notify rejected: reasoning streams emit many tiny fragments.

## Decisions
(none yet)

## Documentation
`docs/tui.md` agent list / agents panel: describe the `writing N chars` indicator and that it
clears when the model step ends. `docs/agents.md`: one line.

## Testing Strategy
- Unit (tree): `note_writing` accumulates, `reset_writing` zeroes, snapshot exposes it, notify is
  throttled (many calls → bounded wakeups), unknown agent id is a no-op.
- Unit (tui): formatter (`999`, `1.0k`, `12.3k`), list/panel row render with and without `writing`,
  not shown for non-running agents.
- Integration (core, `fontaine-core/tests/billing.rs` style): call `drive` with `MockStreamEvent`
  (`ReasoningDelta`, `ToolCallNameDelta`, `ToolCallArgumentsDelta`, then `Text`) and read
  `agents().rows()` inside `on_event` on `AssistantDelta` (`Usage`/`on_event` don't fire on
  Reasoning/Arguments alone); assert the counter is nonzero mid-step and zero after the
  `CompletionCall`. Pins the #343 bug: counter stayed invisible during reasoning-only steps.
