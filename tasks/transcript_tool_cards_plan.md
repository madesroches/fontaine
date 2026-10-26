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
- `Ctrl-O` toggles the task panel (`app.rs:622`, `docs/tui.md`); the plan's Decisions of 2026-10-25
  record that. Mouse capture is off, so hover is not available.

## Design

### Settle-time card, expand by command

Of the issue's options: **(a)** render a capped card when the call settles, plus **(b)** a command that
re-prints the full call as new lines. **(c)** (a transcript view drawn from memory) is not built: it
needs an alternate screen or a key that collides with Ctrl-O, and (b) gives the same reach without
either. No key is rebound.

### Data (`transcript.rs`)

`Entry::Tool` gains `call: String` (the full call text), `output: Option<ToolOutput>` and
`diff: Option<DiffPreview>`:

```
pub struct ToolOutput { pub text: String, pub clipped: bool }   // text capped at OUTPUT_KEEP_BYTES
```

- `call` is built from the same `SUMMARY_KEYS` lookup as `summarize`, but whole (`command`, `path`,
  `url`...; falls back to the compact JSON of args, as `summarize` does). `summary` stays the
  one-line form used by the pending row, the denial rows and `linear`.
- `output` is the `ToolResult` text, sanitised (below) and capped at 64 KiB, keeping head and tail
  halves with a `clipped` flag, so a 100 MB `cat` cannot grow memory. Entries are already kept for
  the session (`Transcript.entries`), so this adds a bounded amount per call.
- `todo_write`, `read`, and tools with empty output get no output block (a card is only drawn when
  there is something beyond the one-line form).

### Card layout

Settled `Done`/`Failed` call, width `w`:

```
• shell  (call, first 3 lines, then "… (+N lines)")      ✓
    python3 -m unittest -q 2>&1 | tail -40
    ...
  │ last 10 output rows (dim; leading "… N earlier lines" when cut)
  │ Ran 38 tests in 0.12s
  expand: /expand 3
```

- Call: up to `CALL_ROWS` (3) wrapped rows under the `•` row, then `… (+N lines)` using the count of
  hidden wrapped rows. A single short line renders exactly as today (no behaviour change for the
  common case).
- Output: the last `OUTPUT_ROWS` (10) wrapped rows, dim, behind a `│ ` gutter; when rows are cut, a
  first row `… N earlier rows` and the footer `/expand <n>`. `n` is the call's ordinal among tool
  calls this session (a stable, short handle; `/expand` alone means the latest).
- `edit`/`write` with a diff: the header row `path  +A -R` and up to `DIFF_ROWS` (20) rows from
  the preview, reusing one row builder shared with the approval prompt (move `diff_rows` out of
  `approval.rs` into `ui`-neutral shared code, say `rust/fontaine-tui/src/diff.rs`; the prompt and
  the card then cannot drift). Beyond the cap: `… N more changed lines` plus the `/expand` footer.
- Failed: the existing ` ✗ failed: …` suffix stays on the first row; the output block follows
  (error text is usually the useful part). Denied and Interrupted rows are unchanged (no output).
- Colours come from `Theme` (`success`/`error` for diff signs, dim for output), as elsewhere.
  Glyphs: the `│` gutter has an ASCII form in `Glyphs` (`|`).
- Settling happens through `entry_lines`, so `commit`, `commit_within` and `live_lines` pick the card
  up unchanged; the row count of a settled entry is what `commit_within` budgets, which already
  handles multi-row entries. A pending row stays one line (output does not exist yet).
- Screen-reader mode: `linear` keeps one `label`led line per call and, for a call with output, adds the
  capped output as plain lines without the gutter glyph. Its sanitising filter at `linear.rs:318`
  still applies.

### `/expand [n]`

New command in `rust/fontaine-tui/src/commands.rs`: prints the full call, the full retained output
(up to the 64 KiB cap, with a clipped note) and the full diff of call `n` (default latest) as a
notice-like block of new transcript lines, so it lands in scrollback like everything else. An
unknown `n` is an error line. It does not touch the model's context and adds no tool. Registered in
`/help`.

### Diff for every `edit`/`write`

The diff must be computed before the tool runs and must exist for calls that were not prompted:

- In the permission pipeline (`permissions.rs`), once a call to `edit`/`write` is **allowed** (by a
  human, a rule or a mode), compute the preview on a blocking task, reusing the one `ask_human` already
  built if it exists, and publish it to the TUI keyed by `tool_use_id`. Today the decision feed
  (`SessionState::publish`, `turn.rs:197`) carries `DecisionRecord`s only, and the record is
  audited, so the preview does **not** go into the record. Generalise the feed item to an enum
  `FeedItem::{Decision, Preview{ id, preview }}` (or add a parallel preview slot); the exact shape is
  chosen on reading `SessionState::publish`/`next_decision`.
- `turn.rs` forwards it as `SessionEvent::ToolPreview { id, preview }`; `Transcript::on_event`
  attaches it to the matching `Entry::Tool` (found by id). Headless (`headless.rs:459`) and
  `child_runner.rs:378` ignore the new variant, like `Usage`.
- Display-only, like the approval preview: it feeds no decision, guardian, hook or ledger, and a
  failure to compute it just means no diff on the card. A call that is denied computes nothing.
- Cost: one extra file read per allowed edit/write (≤ 1 MiB, in-scope only), off the async thread.

### Display safety

The tool output reaches the screen, so it passes the same sanitising as other tool output: reuse
`preview::sanitize` (tabs expanded, control characters blanked, `redact::text` applied), exposed as a
`pub fn display_text` in core, and apply it once when the result is stored, not at paint time. The
`call` text goes through the same function (a command may carry a credential the approval prompt
already redacts). No regex matching on tool arguments for any decision: this is purely display.

## Implementation Steps

1. `fontaine-core/src/preview.rs`: expose the sanitiser as `display_text`; unit tests for tabs,
   escapes, redaction. Add `SessionEvent::ToolPreview`; handle it in `headless.rs` and
   `child_runner.rs` (ignore).
2. `fontaine-core/src/permissions.rs` + `turn.rs`: compute the preview for allowed `edit`/`write`
   and forward it as `ToolPreview` (feed item generalised as above). Reuse the prompt's preview.
3. `fontaine-tui/src/diff.rs`: move `diff_rows` from `approval.rs`; `approval.rs` calls it. Tests move.
4. `transcript.rs`: new `Entry::Tool` fields, capped/clipped output, `call` builder, card rendering
   in `entry_lines`, `ToolPreview` handling, tool ordinal lookup for `/expand`.
5. `commands.rs`: `/expand [n]`; `app.rs`/`ui` wiring as the other transcript-printing commands.
6. `linear.rs`: capped output lines for screen-reader mode; `Glyphs`: gutter glyph.
7. Docs: `docs/tui.md` (transcript paragraph at line 22, the commands table, screen-reader
   section), `docs/terminal.md` if it describes tool rows, `CHANGELOG.md`. Record the Decisions below
   in the master plan's `## Decisions`.

## Files to Modify

- `rust/fontaine-core/src/preview.rs`, `permissions.rs`, `turn.rs`, `child_runner.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-tui/src/transcript.rs`, `approval.rs`, `linear.rs`, `commands.rs`, `theme.rs`, `app.rs`
- new `rust/fontaine-tui/src/diff.rs`
- `docs/tui.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Settle-time card + `/expand` vs a transcript view (c):** (c) needs a key; Claude Code's Ctrl-O
  is already the task list here, and an alternate-screen view fights the scrollback-first design.
  `/expand` writes to scrollback, so it is searchable and copyable like everything else.
- **Always-on cards vs a setting:** capped cards cost at most ~25 rows per call; no config key until
  someone asks (an off switch would be a second rendering path to test).
- **Compute the diff in the TUI from the result text:** rejected: the file has already changed and the
  tool result carries no hunks.

## Decisions

- Cards are capped at 3 call rows, 10 output rows and 20 diff rows; the numbers are constants so the
  cap can change without touching layout code.
- Output text is stored capped at 64 KiB per call so memory stays bounded over a long session.
- No key is rebound: Ctrl-O stays the task list (2026-10-25).

## Documentation

`docs/tui.md` (transcript description, `/expand` in the commands table, screen-reader behaviour),
`CHANGELOG.md`, and the master plan's Decisions.

## Testing Strategy

All unit tests in `transcript.rs`/`diff.rs`/`preview.rs` against constructed inputs:

- A one-line short call with no output renders exactly as today (golden check of existing tests).
- A 12-line command shows 3 rows and `… (+9 lines)`; output of 30 rows shows `… 20 earlier rows`
  and the last 10; a failed call keeps the `failed:` suffix and its output.
- Output with escape sequences, tabs and a credential-shaped string comes out blanked/redacted
  (the display-safety invariant).
- Output over the 64 KiB cap is clipped with the flag set.
- `commit`, `commit_within` and `live_lines` give the same rows for a card (the existing equality tests
  extended to a tool entry with output), including a card released pending and settled via follow-up.
- A `ToolPreview` event attaches to the right entry by id; late or unknown ids are ignored.
- `/expand` prints full call/output/diff, defaults to the latest, errors on an unknown `n`.
- Pipeline: an `edit` allowed by rule or by accept-edits mode emits a preview; a denied one does not;
  the audit record is byte-identical to before (preview is not in it). Linear mode golden for a call
  with output.

## Manual Verification

In a real tmux (`tasks/design/testing.md`, "Reproducing terminal layout bugs"): run a turn that calls
`shell` with a 15-line output and an `edit`; confirm the cards land in scrollback without the live
area overwriting them and `/expand` prints below the prompt. Not automated because the layout
interaction with the real terminal is what the `TestBackend` cannot show.

## Open Questions

- Shape of the generalised decision feed (enum item vs separate channel): settled on reading
  `SessionState::publish` during step 2.
- Whether the numeric handle for `/expand` should be shown as the call's ordinal or its tool-use id
  prefix; ordinal is assumed.
