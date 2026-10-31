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
- The mock model already has `MockStreamEvent::ReasoningDelta` (used in `tests/billing.rs`). It
  emits `End{Reasoning}` after the text unless a closing `MockStreamEvent::Reasoning` with the same
  id is sent before the text.

## Design

**Core.** Add `SessionEvent::ReasoningDelta(String)` and `SessionEvent::Reasoning(String)` (the
settled text, from `End { content: AssistantContent::Reasoning }`), mirroring the text pair.
`drive()` emits the delta after `step.note(..)`. Reasoning is model output rendered as text; it
never reaches the guardian or permission pipeline, and history is untouched (rig keeps the
reasoning parts).

**Transcript.** Add `Entry::Reasoning { text, streaming, clipped }`. A `ReasoningDelta` appends
to a trailing streaming `Reasoning` entry or pushes a new one; the push calls the generalised
`finish_streaming` first (settling a trailing streaming `Assistant`), symmetric with
`AssistantDelta` settling `Reasoning`. Event order is not guaranteed
(OpenAI-compatible chat and the mock emit `End{Reasoning}` after the text; rig can interleave
Reasoning, ToolCall, Reasoning), so `Reasoning(full)` settles the latest `Reasoning`
entry if it has not yet been committed, replacing its text (End text replaces rather than appends:
rig's Responses API can restate it), and is otherwise ignored. `AssistantDelta`, a tool call, a user
entry and every other push settle a trailing streaming `Reasoning`: `finish_streaming` is
generalised to both variants. An empty settled `Reasoning` (redacted block) is dropped, not
rendered. Deltas are appended raw. Once the buffer passes a margin above `OUTPUT_KEEP_BYTES`
(2x), the whole buffer is run through `display_text` (`fontaine_core::preview`) and then the
head+tail clip factored out of `keep_output` (`transcript.rs:131-159`), in that order, redacting
before clipping as `keep_output` requires; the buffer is replaced by the result and `clipped` is
set, so memory stays bounded while streaming and a cut never leaves an unredactable credential
prefix. The settle runs the same sanitise-then-clip on the final text. The sanitised text is
cached and refreshed only when the buffer grows (the UI renders live entries several times per
frame); rendering uses the cache. `/expand thinking` shows the settled (sanitised, possibly
clipped) text.

**Rendering.** Dim, italic, behind a `thinking` header row, passed through the existing paint-safe
text path (no markdown). While streaming the card shows the header plus the last `REASONING_ROWS`
(5) wrapped rows live, in the live area only. A streaming `Entry::Reasoning` is unsettled in
`is_settled` and excluded from the `Assistant` special cases in `commit_entries`/`commit_within`
(which write rows to scrollback permanently), so `commit_within` (`transcript.rs:549-560`)
stops at a streaming `Reasoning` entry as `commit_entries` stops at an unsettled one, rather than
writing it when the live area is over budget; it is committed once, folded to its first 5 rows
plus a footer `  expand: /expand thinking`, when it settles, as tool cards are
(`transcript.rs:636-647, 835-836`). `/expand thinking` prints the latest reasoning card in full
(see **`/expand thinking`** below; an `Expanded` wrapper generalised to hold a `Reasoning` entry; render arms in `transcript.rs`
and `linear.rs`, and the `Expanded` doc comment at `transcript.rs:63` updated). Linear/screen-reader
mode prints the settled text prefixed `thinking:`, folded with the same `expand:` hint as linear
tool output (`linear.rs:270-286`).

**`/expand thinking`.** The argument parses to a new
`pub enum ExpandTarget { Latest, Call(usize), Thinking }`; `call_number` (`commands.rs:690-700`)
is renamed `expand_target(argument) -> Result<ExpandTarget, String>`: empty is `Latest`, the exact
lowercase word `thinking` is `Thinking`, a `usize` is `Call(n)` (unchanged: bare `/expand` is still
the latest tool call), anything else errors with
``/expand takes a call number or `thinking`, such as `/expand 3`, not `{argument}` ``.
`Transcript::expand` takes `ExpandTarget` instead of `Option<usize>`; the tool arms keep their
current error texts. `Thinking` picks the latest `Entry::Reasoning`: none (including when
`show_reasoning` is false) errors `there is no reasoning to expand`; a still-streaming one errors
`the reasoning is still streaming; /expand thinking works once it settles`; otherwise it pushes
`Expanded(Reasoning)` with the full settled text, followed by the same clip notice the expanded
tool output uses when `clipped` is set. The help hint becomes `[n | thinking]` and the
`Command::Expand` doc comment (`commands.rs:61`) becomes `/expand [n | thinking]`. The caller in
`ui/commands/reports.rs:102-114` and the `call_number` test (`commands.rs:~819`) follow the rename.

**Config.** `[tui] show_reasoning` (bool, default `true`). When false the transcript
ignores both events. Added to `TuiSection`/`Tui` (the schema is the `JsonSchema` derive on
`TuiSection`) and to the settings panel catalog as a `tui.show_reasoning` bool-choice row
(`Applies::NextLaunch`, like `tui.screen_reader`). `Transcript` gets a manual `Default` with
`show_reasoning: true` (the derived one would give false). Scope comes from the existing
`tui` row in `config_scope.rs`, no table change.

**Subagents.** Their events already flow through `AgentViewEvent::Event` into that agent's own
view transcript, so reasoning shows when the user opens the agent view and never inline in the
parent's transcript. `show_reasoning` is a field on `Transcript`, set next to `glyphs` in
`fontaine-tui/src/lib.rs` `run` and copied in `App::view_of` (`app.rs:348-355`) so view
transcripts honor it.

**Headless `-p`.** Text `-p` prints nothing for reasoning (Claude Code parity). `stream-json`
carries no reasoning: it is not in the closed v1 schema (`docs/schemas/headless-v1.schema.json`),
a deviation since Claude Code's stream-json carries thinking blocks. `ReasoningDelta` and
`Reasoning` are explicit no-op arms in both.

## Implementation Steps

1. `fontaine-core/src/turn.rs`: add the two variants; emit them in `drive()`.
2. `fontaine-core/src/config.rs`: `show_reasoning` in `TuiSection`/`Tui`, merge, `JsonSchema`
   derive; `fontaine-core/src/config_catalog.rs`: `tui.show_reasoning` row.
3. `fontaine-tui/src/transcript.rs`: `Entry::Reasoning`, event handling, clipping, row rendering,
   commit logic, `Expanded(Reasoning)` arm. `fontaine-tui/src/commands.rs` (`/expand` hint at
   `:210-215` and doc comment at `:61`; `call_number` at `:690-700` becomes `expand_target`
   returning `ExpandTarget`, plus a parse test)
   and `ui/commands/reports.rs:102-114`.
4. `fontaine-tui/src/linear.rs` (incl. `Expanded(Reasoning)` arm), `ui/turn.rs`: handle the new variants / entry (compiler-driven).
5. `fontaine/src/headless.rs`: no-op arms.
6. Wire `config.tui.show_reasoning` into `Transcript` in `fontaine-tui/src/lib.rs` `run` and `App::view_of` (`app.rs`).
7. Docs and changelog.

## Files to Modify

`rust/fontaine-core/src/turn.rs`, `rust/fontaine-core/src/config.rs`,
`rust/fontaine-core/src/config_catalog.rs`, `rust/fontaine-tui/src/transcript.rs`, `rust/fontaine-tui/src/linear.rs`,
`rust/fontaine-tui/src/ui/turn.rs`, `rust/fontaine-tui/src/commands.rs`,
`rust/fontaine-tui/src/ui/commands/reports.rs`, `rust/fontaine-tui/src/ui/tests.rs` (the old
`/expand takes a call number` assertion at ~:322), `rust/fontaine-tui/src/lib.rs`,
`rust/fontaine-tui/src/app.rs`, `rust/fontaine/src/headless.rs`, `docs/configuration.md`,
`docs/tui.md`, `docs/terminal.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions).

## Trade-offs

- Fold-after-settle versus always streamed open: settled reasoning can be long and pushes the
  answer off screen; folding keeps the answer visible while `/expand thinking` recovers the rest.
- Config key versus a `/` toggle: a key matches how `[tui]` preferences work; a toggle can follow.

## Decisions

- 2026-10-31: Model reasoning is shown in the transcript by default, folded once settled;
  `[tui] show_reasoning = false` hides it; unlike Claude Code, as observed to keep thinking out of the main
  transcript (Ctrl-O transcript view; not re-verified with `claude -p` in this plan). Headless
  `stream-json` omits reasoning, though Claude Code's carries thinking blocks (#344). Appended to `### TUI` in `tasks/fontaine_harness_plan.md`.
- 2026-10-31: AFK: Reasoning buffer is appended raw, then past 2x `OUTPUT_KEEP_BYTES` run through `display_text` and the head+tail clip in `keep_output`'s order (redact, then clip), so memory is bounded while streaming and redaction holds; the sanitised cache refreshes on growth.
- 2026-10-31: AFK: `/expand thinking` parses via `ExpandTarget { Latest, Call(n), Thinking }` (`call_number` becomes `expand_target`, `Transcript::expand` takes it); bare `/expand` stays the latest tool call; only a settled card expands, errors `there is no reasoning to expand` / still-streaming message; hint `[n | thinking]`; clip notice shown when clipped.

## Documentation

`docs/configuration.md` `[tui]` table (`show_reasoning`); `docs/tui.md` (the card, folding,
subagent behaviour; `/expand` row ~line 397 becomes `[<n> | thinking]`, and the `/expand`
paragraph ~lines 46-51 mentions `/expand thinking`; the Settings panel example ~lines 494-520
gains a `tui.show_reasoning` row); `docs/terminal.md` (a
`thinking:` row, ~lines 163-176); `CHANGELOG.md`.

## Testing Strategy

- Core: a mock-model turn with `ReasoningDelta`, a closing `MockStreamEvent::Reasoning`, then text
  yields `ReasoningDelta`… `Reasoning` then `Assistant` in order (extend `tests/billing.rs`); a
  second test with the OpenAI-like order (`ReasoningDelta`, `AssistantDelta`, `Assistant`,
  `Reasoning`).
- Transcript unit tests: deltas accumulate into one entry; settle on `Reasoning`/`finish_streaming`;
  ordering after the user entry; late `Reasoning(full)` settles the earlier entry and an
  interleaved Reasoning, ToolCall, Reasoning stays two entries; empty settled reasoning is dropped;
  an over-budget live area holding a streaming reasoning card is not committed by `commit_within`;
  a view transcript for a subagent honors `show_reasoning`; clipping bounds memory while streaming and redacts before clipping (a credential straddling the cut is masked); streaming card shows last 5 rows, settled
  card shows first 5 plus footer; `show_reasoning = false` drops the entries; control characters
  are stripped; `/expand thinking` prints the whole settled text with the clip notice when clipped, errors with
  `there is no reasoning to expand` when none, and reports a still-streaming card; `expand_target`
  parses empty, number, `thinking` and rejects the rest.
- Config: default true, schema; catalog row `tui.show_reasoning`.
- Headless: `-p` output has no reasoning text.
- Layout of live area and scrollback commits is reproduced in a real tmux per `tasks/design/testing.md`.
