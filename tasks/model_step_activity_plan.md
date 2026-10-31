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
  `writing: u64` and, throttled (at most one `changed` notification per ~250 ms per tree), wakes
  watchers. `reset_writing(agent)` zeroes it. `AgentRow` gains `writing: u64`.
  It is display state: not part of `Spend`, never budgeted, never persisted.
- **`drive` feeds it.** On `StreamEvent::Reasoning { text }` add `text.chars().count()`; on
  `StreamEvent::Arguments { json }` add `json.chars().count()`; `Text` also counts (the fragment
  still goes out as `AssistantDelta` unchanged). Reset on `CompletionCall` and when the drive loop
  ends (including failure/stop), so a finished step never shows stale progress.
  No new `SessionEvent` variant: the tree is the channel, because it already serves the root and
  subagents uniformly and the list/panel already read it.
- **TUI.** `ListRow` and `PanelRow` get `writing: u64`. While the agent is running and
  `writing > 0`, append `· writing 4.2k chars` (format `<n>` below 1000, else `<n.n>k`) after the
  spend text. `main`'s row uses the same field. One shared formatter next to `spend_text` in
  `agents_panel.rs`.
- **Debug logs.** In `drive`, `tracing::debug!(target = "fontaine::turn", agent.id, ...)` at model
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
   `ListRow.writing`, render; update the driver code that maps `AgentRow` to both.
4. Docs: `docs/tui.md` (agent list / agents panel sections) and `docs/agents.md` mention.
5. CHANGELOG entry.

## Files to Modify
- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-tui/src/agents_panel.rs`, `rust/fontaine-tui/src/agent_list.rs` (and the row mapping site)
- `docs/tui.md`, `docs/agents.md`, `CHANGELOG.md`

## Trade-offs
- New `SessionEvent` variant rejected: it would need handling in headless, linear and screen
  consumers and would not reach the agents panel, which reads the tree.
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
- Integration (core): drive a mock stream of `Reasoning` + `Arguments` + `CompletionCall` events
  via `MockStreamEvent` (if it supports reasoning/argument deltas; otherwise unit-test the
  fragment-counting helper extracted from the loop) and assert the tree counter rises mid-step and
  is zero after the call. Pins the #343 bug: counter stayed invisible during reasoning-only steps.

## Open Questions
- Does `MockStreamEvent` in rig-core 0.45 emit `Reasoning`/`Arguments` deltas? Decides integration
  vs helper-level test above.
- Does the TUI redraw on the tree's `changed` notification, or only on its own tick? Confirm before
  step 3; if tick-only, no extra wiring is needed.
