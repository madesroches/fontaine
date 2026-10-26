# Transcript Tool Cards Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/222

## Overview

The transcript shows one truncated line per tool call (`• shell python3 -m unittest -q 2>&1 | tail -40 ✓`).
The command's output, an edit's diff and a long or multi-line command are never visible after the
approval prompt closes, although all of it is already in the model's context. This plan renders each
tool call as a compact **card** at settle time (full call capped, last lines of output, diff for
`edit`/`write`) and adds an on-demand `/expand` command that re-prints a call in full as new lines.

## Current State

- `Entry::Tool { id, tool, summary, status }` (`rust/fontaine-tui/src/transcript.rs:30`) keeps only
  `summarize(tool, args)`, the first line of one argument (`transcript.rs:66`). `entry_lines` renders it
  as `truncate("• {tool} {summary}", width - 2)` plus a status mark (`transcript.rs:583`). Full args and
  the result text are dropped: `ToolResult { text }` only feeds `first_line` for `Failed`.
- Settled lines are written to the terminal's scrollback and cannot be redrawn (`commit`, `commit_within`,
  `commit_entries`). A pending call released early gets a follow-up line when it settles (`settle`).
  `Entry::Shell` (human `!` commands) already prints its whole output.
- Three renderers consume the transcript: the inline `Screen` (`screen.rs`), the `linear` renderer
  (screen-reader mode, `linear.rs:315`, which calls `commit_entries(Cut::Lines)` and its own `label`)
  and `live_lines` in `view.rs`.
- The approval prompt already has a diff: `fontaine_core::preview::preview` (`preview.rs`) builds a
  sanitised, redacted `DiffPreview` for `edit`/`write` (UTF-8, ≤ 1 MiB, in-scope, ≤ 200 rows) in
  `ask_human` (`permissions.rs:1534`) and `approval::diff_rows` (`approval.rs:289`) paints it.
  It is computed **before** the tool runs, only when a human is asked; an edit allowed by rule or
  mode never gets one, and after the call the pre-change text is gone.
- `Ctrl-O` toggles the task panel (`app.rs:622`, `docs/tui.md`); the master plan's Decisions of 2026-10-25
  record that. Mouse capture is off, so hover is not available.

## Design

### Settle-time card, expand by command

Of the issue's options: **(a)** render a capped card when the call settles, plus **(b)** a command that
re-prints the full call as new lines. **(c)** (a transcript view drawn from memory) is not built: it
needs an alternate screen or a key that collides with Ctrl-O, and (b) gives the same reach without
either. No key is rebound.

### Data (`transcript.rs`)

`Entry::Tool` gains `ordinal: usize` (assigned from a session counter when the `ToolCall` event
creates the entry; entries are retired and duplicated by follow-ups, so an entries index is not a
stable handle), `call: String` (the full call text), `output: Option<ToolOutput>` and
`diff: Option<DiffPreview>`:

```
pub struct ToolOutput { pub text: String, pub clipped: bool }   // text capped at OUTPUT_KEEP_BYTES
```

- `call` is built from the same `SUMMARY_KEYS` lookup as `summarize`, but whole (`command`, `path`,
  `url`...; falls back to the compact JSON of args, as `summarize` does). `summary` stays the
  one-line form used by the pending row, the denial rows and `linear`.
- `output` is the `ToolResult` text, sanitised (below) and capped at `OUTPUT_KEEP_BYTES` (64 KiB), keeping head and tail
  halves with a `clipped` flag, so a 100 MB `cat` cannot grow memory. Entries are already kept for
  the session (`Transcript.entries`), so this adds a bounded amount per call.
- `todo_write`, `read`, and tools with empty output get no output block (a card is only drawn when
  there is something beyond the one-line form).

### Card layout

Settled `Done`/`Failed` call, width `w`:

```
• shell  (call, first 3 rows, then "… (+N rows)")      ✓
    python3 -m unittest -q 2>&1 | tail -40
    ...
  │ last 10 output rows (dim; leading "… N earlier rows" when cut)
  │ Ran 38 tests in 0.12s
  expand: /expand 3
```

- Call: up to `CALL_ROWS` (3) wrapped rows under the `•` row, then `… (+N rows)` using the count of
  hidden wrapped rows. A single short line renders exactly as today (no behaviour change for the
  common case).
- Output: the last `OUTPUT_ROWS` (10) wrapped rows, dim, behind a `│ ` gutter; when rows are cut, a
  first row `… N earlier rows`. The `/expand <n>` footer appears whenever any of call, output or
  diff is cut. `n` is the entry's `ordinal`
  (counted from the session start; `/copy [n]` counts from the latest, so the two differ;
  `/expand` alone means the latest).
- `edit`/`write` with a diff: the header row `path  +A -R` and up to `DIFF_ROWS` (20) rows from
  the preview, reusing one row builder shared with the approval prompt (move `diff_rows` out of
  `approval.rs` into `ui`-neutral shared code, say `rust/fontaine-tui/src/diff.rs`; the prompt and
  the card then cannot drift). The shared builder takes a row cap and a footer parameter (the prompt's
  footer is hardcoded at `approval.rs:331` and uncapped); the card passes `DIFF_ROWS` and the
  `/expand` footer. Beyond the cap: `… N more changed lines` plus the `/expand` footer.
- Failed: the existing ` ✗ failed: …` suffix stays on the first row; the output block follows
  (error text is usually the useful part). Denied and Interrupted rows are unchanged (no output).
- Colours come from `Theme` (`success`/`error` for diff signs, dim for output), as elsewhere.
  Glyphs: the `│` gutter has an ASCII form in `Glyphs` (`|`).
- Settling happens through `entry_lines`, so `commit`, `commit_within` and `live_lines` pick the card
  up unchanged; the row count of a settled entry is what `commit_within` budgets, which already
  handles multi-row entries. A pending row stays one line (output does not exist yet).
- Settling: `settle` (`transcript.rs:167`) pushes a clone of the whole entry as a follow-up when a
  Pending entry was already written to scrollback; that follow-up prints the full card (the simplest
  option; no header-only variant). So output and diff are attached to the entry before `settle`
  runs: the `ToolResult` handler stores the output, then calls `set_status`. `ToolPreview` always
  precedes `ToolResult` (the biased feed drain at `turn.rs:196-211` guarantees it), so a preview
  never arrives after settle.
- Screen-reader mode: `linear` keeps one `label`led line per call and, for a call with output, adds the
  capped output as plain lines without the gutter glyph. Its sanitising filter at `linear.rs:318`
  still applies.

### `/expand [n]`

New command in `rust/fontaine-tui/src/commands.rs`: prints the full call, the full retained output
(up to the 64 KiB cap, with a clipped note) and the full diff of call `n` (default latest) as a
new transcript lines of a new `Entry` variant (shaped like `Entry::Shell`: the pre-rendered block
is printed as is), so it lands in scrollback like everything else. An
unknown `n` is an error line. It does not touch the model's context and adds no tool. A `Command` variant plus
a `BUILTINS` entry (`commands.rs:77`) register it in `/help`; the handler lives in
`rust/fontaine-tui/src/ui/commands/`.

### Diff for every `edit`/`write`

The diff must be computed before the tool runs and must exist for calls that were not prompted:

- In the permission pipeline (`permissions.rs`), the returned `GateDecision::Allow` is post-processed
  in `check` (~:1728-1740, which wraps `adjudicate`; the single point, not per site) for `edit`/`write`,
  awaiting the preview on a blocking task, reusing the preview `ask_human` already built if it exists. It uses
  the final Allow args (hooks can rewrite them) and is skipped when `tool_use_id` is `None`. It is
  published to the TUI keyed by `tool_use_id`. `SessionState::publish` (`gate.rs:642`) feeds
  `DecisionFeed { rx: UnboundedReceiver<DecisionRecord> }` (`gate.rs:195`), and the record is
  audited, so the preview does **not** go into the record. `DecisionFeed::next`/`try_next` keep
  returning `DecisionRecord`; a parallel unbounded channel of `(tool_use_id, DiffPreview)` sits
  beside the decision channel. Other consumers of the feed (`fontaine-evals/src/none.rs:149`,
  `fontaine-core/tests/audit_telemetry.rs:49`) are unaffected.
- `turn.rs` drains the preview channel next to the decision feed (~:196, :206-211, :278) and in the
  interrupt drain at `ui/turn.rs:371`, forwarding `SessionEvent::ToolPreview { id, preview }`.
  `Transcript::on_event` attaches it to the `Entry::Tool` with that id only (`find_tool`,
  `transcript.rs:143`, falls back to the latest Pending by tool name, so it is not used here:
  subagent previews on the root feed would attach to the wrong card); unknown ids are ignored. A preview that arrives before its `ToolCall` entry is likewise ignored
  (no diff on the card); tool calls run one at a time (`gate.rs:624-625`), so the single per-agent
  `current_call` slot is enough.
  Headless (`headless.rs:459`) and `child_runner.rs:378` ignore the new variant, like `Usage`.
  A diff computed from the result text was rejected: the tool result carries no hunks.
- Display-only, like the approval preview: it feeds no decision, guardian, hook or ledger, and a
  failure to compute it just means no diff on the card. A call that is denied computes nothing.
- Cost: one extra file read per allowed edit/write (≤ 1 MiB, in-scope only), off the async thread.

### Display safety

The tool output reaches the screen, and the TUI does not sanitise tool result text today (only
`linear.rs:318` strips control characters; `redact` is not used in `fontaine-tui`). `preview::sanitize`
(`preview.rs:160`) is private and single-line (it trims trailing `\n`/`\r` and blanks every control
character including `\n`), so it cannot be exposed as is. `pub fn display_text` in core applies, per
line (split on `\n`; `\r` handled by reimplementing in core the same `\r` semantics as `linear::terminal_text`, which is `pub(crate)` in `fontaine-tui` so core cannot call it), tab
expansion, control blanking and `redact::text`. It is applied to both `call` and `output` once at
store time, not at paint time. No regex matching on tool arguments for any decision: this is purely display.

## Implementation Steps

1. `fontaine-core/src/preview.rs`: add per-line `display_text`; unit tests for tabs,
   escapes, redaction and a multi-line input. Add `SessionEvent::ToolPreview`; handle it in `headless.rs` and
   `child_runner.rs` (ignore).
2. `fontaine-core/src/permissions.rs` + `turn.rs`: compute the preview for allowed `edit`/`write`
   and forward it as `ToolPreview` (parallel preview channel as above); drain it in the `ui/turn.rs` interrupt path. Reuse the prompt's preview.
3. `fontaine-tui/src/diff.rs`: move `diff_rows` from `approval.rs`, adding the row-cap and footer parameters; `approval.rs` calls it. Tests move.
4. `transcript.rs`: new `Entry::Tool` fields, capped/clipped output, `call` builder, card rendering
   in `entry_lines`, `ToolPreview` handling, `ordinal` counter, lookup for `/expand`. `entry_lines` (`transcript.rs`) and `linear::label` (`linear.rs`) need arms for the new `Entry` variant.
5. `commands.rs`: `Command` variant and `BUILTINS` entry for `/expand [n]`; handler in `ui/commands/`, wiring in `ui/mod.rs`.
6. `linear.rs`: capped output lines for screen-reader mode; `theme.rs`: the `Glyphs` gutter glyph.
7. Docs: `docs/tui.md` (transcript paragraph at line 22;
   the approval-diff paragraph at ~:176-184, which says the diff appears only in the prompt and not for other tools;
   screen-reader mentions are scattered ~:259, :443, :539, the commands table ~:298),
   `docs/index.md` (slash command list at :32, add `/expand`),
   `docs/terminal.md` (the `tool:` rows in the table ~:161-162), `CHANGELOG.md`. Record the Decisions
   line below in the master plan's `## Decisions`.

## Files to Modify

- `rust/fontaine-core/src/preview.rs`, `permissions.rs`, `gate.rs`, `turn.rs`, `child_runner.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-tui/src/transcript.rs`, `approval.rs`, `linear.rs`, `commands.rs`, `theme.rs`,
  `ui/mod.rs`, `ui/turn.rs`, `ui/tests.rs`, `ui/commands/`
- new `rust/fontaine-tui/src/diff.rs`
- `docs/tui.md`, `docs/index.md`, `docs/terminal.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)
- `rust/fontaine-tui/examples/live_area.rs` (print cards for the manual check)

## Decisions

- 2026-10-26: No key is rebound; Ctrl-O stays the task list, because a transcript view (c) needs a key and an alternate screen (#222).

## Documentation

`docs/tui.md` (transcript description, the approval-diff paragraph, `/expand` in the commands table, screen-reader behaviour),
`docs/index.md` (add `/expand` to the slash command list),
`CHANGELOG.md`, and the master plan's Decisions.

## Testing Strategy

All unit tests in `transcript.rs`/`diff.rs`/`preview.rs` against constructed inputs:

- A one-line short call with no output renders exactly as today (golden check of existing tests).
- A 12-row command shows 3 rows and `… (+9 rows)`; output of 30 rows shows `… 20 earlier rows`
  and the last 10; a failed call keeps the `failed:` suffix and its output.
- Output with escape sequences, tabs and a credential-shaped string comes out blanked/redacted
  (the display-safety invariant).
- Output over the 64 KiB cap is clipped with the flag set.
- `commit`, `commit_within` and `live_lines` give the same rows for a card (the existing equality tests
  extended to a tool entry with output), including a card released pending and settled via follow-up (the follow-up prints the card once, with output and diff, not twice).
- A `ToolPreview` event attaches to the right entry by id only (a same-named Pending call with a different id is untouched); unknown ids are ignored; a preview before the result shows in the settled follow-up.
- `display_text` on multi-line input keeps line breaks, handles `\r`, and redacts per line; applied to both call and output.
- `/expand` prints full call/output/diff, defaults to the latest, errors on an unknown `n`.
- Pipeline: an `edit` allowed by rule or by accept-edits mode emits a preview; a denied one does not;
  an allowed edit's audit record has the same schema keys as before (see `tests/permissions.rs:470`; the preview is not in it). Linear mode golden for a call
  with output.

## Manual Verification

In a real tmux (`tasks/design/testing.md`, "Reproducing terminal layout bugs"): run a turn that calls
`shell` with a 15-line output and an `edit`, by extending `examples/live_area.rs` to print cards; confirm the cards land in scrollback without the live
area overwriting them and `/expand` lines print above the live area. Not automated because the layout
interaction with the real terminal is what the `TestBackend` cannot show.
