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

**Transcript.** Add `Entry::Reasoning { text, streaming, clipped }`. A `ReasoningDelta` appends
to a trailing streaming `Reasoning` entry or pushes a new one. Event order is not guaranteed
(OpenAI-compatible chat and the mock emit `End{Reasoning}` after the text; rig can interleave
Reasoning, ToolCall, Reasoning), so `Reasoning(full)` settles the latest streaming `Reasoning`
entry wherever it is, and is ignored if already settled. `AssistantDelta`, a tool call, a user
entry and every other push settle a trailing streaming `Reasoning`: `finish_streaming` is
generalised to both variants. An empty settled `Reasoning` (redacted block) is dropped, not
rendered. Raw text is bounded: once the buffer passes `OUTPUT_KEEP_BYTES` it is head+tail clipped
(the clip factored out of `keep_output`, `transcript.rs:131-159`) and `clipped` is set. Per-line
sanitise and redaction (`fontaine_core::preview::display_text`) plus the clip run on the
accumulated text at render/settle time, not per delta; `/expand thinking` shows the clipped text.

**Rendering.** Dim, italic, behind a `thinking` header row, passed through the existing paint-safe
text path (no markdown). While streaming the card shows the header plus the last `REASONING_ROWS`
(5) wrapped rows live, in the live area only. A streaming `Entry::Reasoning` is unsettled in
`is_settled` and excluded from the `Assistant` special cases in `commit_entries`/`commit_within`
(which write rows to scrollback permanently); it is committed once, folded to its first 5 rows
plus a footer `  expand: /expand thinking`, when it settles, as tool cards are
(`transcript.rs:636-647, 835-836`). `/expand thinking` prints the latest reasoning card in full
(an `Expanded` wrapper generalised to hold a `Reasoning` entry; render arms in `transcript.rs`
and `linear.rs`, and the `Expanded` doc comment at `transcript.rs:63` updated). Linear/screen-reader
mode prints the settled text prefixed `thinking:`, folded with the same `expand:` hint as linear
tool output (`linear.rs:270-286`).

**Config.** `[tui] show_reasoning` (bool, default `true`). When false the transcript
ignores both events. Added to `TuiSection`/`Tui` and the schema; scope comes from the existing
`tui` row in `config_scope.rs`, no table change.

**Subagents.** Their events already flow through `AgentViewEvent::Event` into that agent's own
view transcript, so reasoning shows when the user opens the agent view and never inline in the
parent's transcript. `show_reasoning` is a field on `Transcript`, set next to `glyphs` in
`fontaine-tui/src/lib.rs` `run` and copied in `App::view_of` (`app.rs:348-355`) so view
transcripts honor it.

**Headless `-p`.** Prints nothing for reasoning (Claude Code parity): `ReasoningDelta` and
`Reasoning` are explicit no-op arms in both text and `stream-json` (a closed versioned schema,
`docs/schemas/headless-v1.schema.json`).

## Implementation Steps

1. `fontaine-core/src/turn.rs`: add the two variants; emit them in `drive()`.
2. `fontaine-core/src/config.rs`: `show_reasoning` in `TuiSection`/`Tui`, merge, schema.
3. `fontaine-tui/src/transcript.rs`: `Entry::Reasoning`, event handling, clipping, row rendering,
   commit logic, `Expanded(Reasoning)` arm. `fontaine-tui/src/commands.rs` (`/expand` hint at
   `:210-215`; `call_number` at `:690-700` accepts only numbers, add `thinking`, plus a parse test)
   and `ui/commands/reports.rs:102-114`.
4. `fontaine-tui/src/linear.rs` (incl. `Expanded(Reasoning)` arm), `ui/turn.rs`: handle the new variants / entry (compiler-driven).
5. `fontaine/src/headless.rs`: no-op arms.
6. Wire `config.tui.show_reasoning` into `Transcript` in `fontaine-tui/src/lib.rs` `run` and `App::view_of` (`app.rs`).
7. Docs and changelog.

## Files to Modify

`rust/fontaine-core/src/turn.rs`, `rust/fontaine-core/src/config.rs`,
`rust/fontaine-tui/src/transcript.rs`, `rust/fontaine-tui/src/linear.rs`,
`rust/fontaine-tui/src/ui/turn.rs`, `rust/fontaine-tui/src/commands.rs`,
`rust/fontaine-tui/src/ui/commands/reports.rs`, `rust/fontaine-tui/src/lib.rs`,
`rust/fontaine-tui/src/app.rs`, `rust/fontaine/src/headless.rs`, `docs/configuration.md`,
`docs/tui.md`, `docs/terminal.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions).

## Trade-offs

- Fold-after-settle versus always streamed open: settled reasoning can be long and pushes the
  answer off screen; folding keeps the answer visible while `/expand thinking` recovers the rest.
- Config key versus a `/` toggle: a key matches how `[tui]` preferences work; a toggle can follow.

## Decisions

- 2026-10-31: Model reasoning is shown in the transcript by default, folded once settled;
  `[tui] show_reasoning = false` hides it; unlike Claude Code, which keeps thinking out of the main
  transcript (#344). Appended to `### TUI` in `tasks/fontaine_harness_plan.md`.

## Documentation

`docs/configuration.md` `[tui]` table (`show_reasoning`); `docs/tui.md` (the card, folding,
subagent behaviour; `/expand` row ~line 397 becomes `[<n> | thinking]`); `docs/terminal.md` (a
`thinking:` row, ~lines 163-176); `CHANGELOG.md`.

## Testing Strategy

- Core: a mock-model turn with `ReasoningDelta`, a closing `MockStreamEvent::Reasoning`, then text
  yields `ReasoningDelta`… `Reasoning` then `Assistant` in order (extend `tests/billing.rs`); a
  second test with the OpenAI-like order (`ReasoningDelta`, `AssistantDelta`, `Assistant`,
  `Reasoning`).
- Transcript unit tests: deltas accumulate into one entry; settle on `Reasoning`/`finish_streaming`;
  ordering after the user entry; late `Reasoning(full)` settles the earlier entry and an
  interleaved Reasoning, ToolCall, Reasoning stays two entries; empty settled reasoning is dropped;
  a view transcript for a subagent honors `show_reasoning`; clipping bounds memory; streaming card shows last 5 rows, settled
  card shows first 5 plus footer; `show_reasoning = false` drops the entries; control characters
  are stripped; `/expand thinking` prints the whole text.
- Config: default true, schema.
- Headless: `-p` output has no reasoning text.
- Layout of live area and scrollback commits is reproduced in a real tmux per `tasks/design/testing.md`.
