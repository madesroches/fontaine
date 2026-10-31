# Model Reasoning Card Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/344

## Overview

Show the model's reasoning in the TUI transcript as a card that streams live, then folds to a few
rows once the reasoning settles, so a long silent stretch gives progress and the trace can be
re-read. Display is **on by default** (the user's call on the issue), switchable with
`[tui] show_reasoning = false`.

## Current State

- `turn.rs` `drive()` receives `StreamEvent::Reasoning { text, .. }` and only counts its chars
  for the step-activity indicator (`rust/fontaine-core/src/turn.rs:425`); `StreamEvent::End` with
  `AssistantContent::Reasoning` falls into `_ => Flow::Continue` (`turn.rs:433`).
- `SessionEvent` (`turn.rs:35`) has `AssistantDelta` / `Assistant` for text and nothing for reasoning.
- Consumers of `SessionEvent`: `fontaine-tui/src/transcript.rs` (`on_event`, ~line 270),
  `fontaine/src/headless.rs:450`, `fontaine-core/src/child_runner.rs:596` (forwards subagent events
  as `AgentViewEvent::Event`, applied by `fontaine-tui/src/ui/turn.rs:780`), and `ui/turn.rs`.
- `Entry` (`transcript.rs:42`) has `Assistant { text, streaming }`; streaming replies commit to
  scrollback by block/row with `committed_chars`. Tool cards fold output to `OUTPUT_ROWS` with an
  `expand:` footer; output is clipped to `OUTPUT_KEEP_BYTES` (64 KiB).
- `[tui]` is `TuiSection`/`Tui` in `fontaine-core/src/config.rs:809,1675`, user-scope only.
- The mock model already has `MockStreamEvent::ReasoningDelta` (used in `tests/billing.rs`).

## Design

**Core.** Add `SessionEvent::ReasoningDelta(String)` and `SessionEvent::Reasoning(String)` (the
settled text, from `End { content: AssistantContent::Reasoning }`), mirroring the text pair.
`drive()` emits the delta after `step.note(..)`. Reasoning is model output rendered as text; it
never reaches the guardian or permission pipeline, and history is untouched (rig keeps the
reasoning parts).

**Transcript.** Add `Entry::Reasoning { text, streaming }`. A `ReasoningDelta` appends to a
trailing streaming `Reasoning` entry or pushes a new one; `Reasoning(full)` replaces/settles it
(same logic as `Assistant`); `finish_streaming` also settles it. Text is clipped on append to
`OUTPUT_KEEP_BYTES` (head + tail, as tool output is) so a huge trace cannot grow memory. Ordering
is natural: reasoning precedes the answer of the same model call, landing after the user entry.

**Rendering.** Dim, italic, behind a `thinking` header row, passed through the existing paint-safe
text path (no markdown, control characters stripped like tool output). While streaming the card
shows the last `REASONING_ROWS` (5) wrapped rows live; once settled it keeps the first 5 rows and
a footer `  expand: /expand thinking`. Streaming rows commit to scrollback using the same
row-cutting mechanism as `Assistant`. `/expand thinking` prints the latest reasoning card in full
(an `Expanded` wrapper generalised to hold a `Reasoning` entry). Linear/screen-reader mode prints
the settled text once, prefixed `thinking:`.

**Config.** `[tui] show_reasoning` (bool, default `true`, user scope). When false the transcript
ignores both events. Added to `TuiSection`/`Tui`, the schema, and the per-key scope table.

**Subagents.** Their events already flow through `AgentViewEvent::Event` into that agent's own
view transcript, so reasoning shows when the user opens the agent view and never inline in the
parent's transcript; no change beyond handling the new variants.

**Headless `-p`.** Prints nothing for reasoning (Claude Code parity); `stream-json` output, if it
emits per-event records, gets a `reasoning` record only if the existing format already mirrors
`AssistantDelta` — otherwise the arm is an explicit no-op.

## Implementation Steps

1. `fontaine-core/src/turn.rs`: add the two variants; emit them in `drive()`.
2. `fontaine-core/src/config.rs`: `show_reasoning` in `TuiSection`/`Tui`, merge, schema.
3. `fontaine-tui/src/transcript.rs`: `Entry::Reasoning`, event handling, clipping, row rendering,
   commit logic, `/expand thinking`.
4. `fontaine-tui/src/linear.rs`, `ui/turn.rs`: handle the new variants / entry (compiler-driven).
5. `fontaine/src/headless.rs`: no-op arms.
6. Wire `config.tui.show_reasoning` into the transcript at construction (`fontaine/src/lib.rs`).
7. Docs and changelog.

## Files to Modify

`rust/fontaine-core/src/turn.rs`, `rust/fontaine-core/src/config.rs`,
`rust/fontaine-tui/src/transcript.rs`, `rust/fontaine-tui/src/linear.rs`,
`rust/fontaine-tui/src/ui/turn.rs`, `rust/fontaine-tui/src/screen.rs` (slash command),
`rust/fontaine/src/headless.rs`, `rust/fontaine/src/lib.rs`, `docs/configuration.md`,
`docs/tui.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions).

## Trade-offs

- Fold-after-settle versus always streamed open: settled reasoning can be long and pushes the
  answer off screen; folding keeps the answer visible while `/expand thinking` recovers the rest.
- Config key versus a `/` toggle: a key matches how `[tui]` preferences work; a toggle can follow.

## Decisions

- 2026-10-31: reasoning display is on by default (user instruction on #344); the plan's Decisions
  gets a dated line noting this is a deviation from Claude Code, which folds thinking by default.

## Documentation

`docs/configuration.md` `[tui]` table (`show_reasoning`); `docs/tui.md` (the card, folding,
`/expand thinking`, subagent behaviour); `CHANGELOG.md`.

## Testing Strategy

- Core: a mock-model turn with `ReasoningDelta` then text yields `ReasoningDelta`… `Reasoning`
  then `Assistant` events in order (extend the pattern in `tests/billing.rs`).
- Transcript unit tests: deltas accumulate into one entry; settle on `Reasoning`/`finish_streaming`;
  ordering after the user entry; clipping bounds memory; streaming card shows last 5 rows, settled
  card shows first 5 plus footer; `show_reasoning = false` drops the entries; control characters
  are stripped; `/expand thinking` prints the whole text.
- Config: default true, user-scope-only (project scope rejected), schema.
- Headless: `-p` output has no reasoning text.
- Layout of live area and scrollback commits is reproduced in a real tmux per `tasks/design/testing.md`.

## Open Questions

- Whether `stream-json` headless output should carry reasoning records (default: no).
