# TUI Windows Scrollback Loss Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/543

## Overview
On Windows, user messages never reach reachable scrollback, and switching agents leaves scrollback
empty or unusable (issue #543). Linux/tmux is healthy. The issue has no root cause yet, so this
plan is diagnosis-first: capture the exact bytes the TUI emits for a user-message commit and for an
agent switch, find which sequence Windows' console host (ConPTY / Windows Terminal) mishandles,
then change the emission for that case and pin it with a regression test.

## Current State
`fontaine-tui/src/screen.rs` keeps an inline viewport and writes settled transcript rows into the
terminal's own scrollback via ratatui `insert_before` (`draw_frame`, ~l.430-510).

- **User message rows** are `"> text"` (bold) followed by a blank `Line::default()`
  (`transcript.rs` `Entry::User`, ~l.1094). Assistant rows have no trailing blank row.
- **Agent switch** (`App::view`, `app.rs:509`) sets `reprint = true`. `Screen::reprint`
  (`screen.rs:525`) calls `purge()` (`ESC[2J` then `ESC[3J`, `screen.rs:189`), rebuilds the
  viewport at row 0 with the full screen height, and the whole transcript is recommitted
  (`Transcript::recommit`, `transcript.rs:246`).
- **Top-of-screen commits**: when the viewport's top is row <= 1 (always true right after a
  reprint, and whenever the transcript fills the screen), `draw_frame` takes the
  `one_row_region` path: ratatui's scroll region would be a one-row invalid DECSTBM, so the whole
  screen scrolls, blank cells of the inserted rows are flagged `Modifier::HIDDEN` (SGR 8) so they
  are written as spaces, and the viewport is rebuilt and repainted.
- Tests use `TestBackend` and a vt100 harness in `screen.rs`; neither models ConPTY.
  `tasks/design/testing.md` "Reproducing terminal layout bugs" documents the tmux repro; there is
  no Windows equivalent.

## Hypotheses (ranked; step 1 decides)
Both symptoms share one path: committing rows with the viewport at the top of the screen
(`one_row_region`). The agent switch is exactly that path for every row of the transcript, and a
fast-typing user's message is committed that way once the screen is full.

1. **`one_row_region` full-screen scroll + `HIDDEN` blanks** is mishandled by ConPTY: the user
   message row (adjacent to a blank HIDDEN row) is overwritten or not pushed to scrollback, while
   non-blank-adjacent assistant rows survive.
2. **`ESC[2J` + `ESC[3J` ordering** in `purge` on Windows Terminal/ConPTY discards the scrollback
   written afterwards, or ConPTY replays the clear lazily (after the recommitted rows).
3. **Cursor-position/`Anchored` assumption**: on Windows the rebuilt terminal's armed anchor
   differs from the real cursor, so inserted rows land on the live area and are repainted over.

## Design
1. **Reproduce and capture (no code change).** Build `cargo build -p fontaine-tui --example
   live_area` and run it in Windows Terminal at a small size; compare scrollback against the
   tmux result. Capture the bytes of one frame (`ESC[?2026h` ... `ESC[?2026l`) for (a) a
   user-message commit with the viewport at the top and (b) a `reprint`. If `live_area` does not
   exercise a user message or an agent switch, extend the example (it is test tooling, no product
   behavior change) so it does.
2. **Bisect the cause** by toggling one variable at a time in the example: drop `HIDDEN`
   (use plain spaces), emit `ESC[3J` before `ESC[2J`, split the purge into its own frame, avoid the
   full-screen scroll. The first toggle that restores scrollback on Windows names the cause.
3. **Fix at the emission point**, narrowly, in `screen.rs` (`one_row_region` handling and/or
   `Framed::purge`), keeping Linux/tmux output unchanged unless the new form is equally correct
   there. A Windows-only branch is acceptable only if no portable form works; record it in the
   master plan's `## Decisions`.
4. **Pin it.** Add a vt100-harness test reproducing the offending byte pattern that the harness can
   express (the harness already rejects `ESC[2J` without a following `ESC[3J`; extend its
   rejection set with whatever pattern step 2 identifies, e.g. a concealed attribute on
   inserted rows). Add transcript-level tests that a user message and an agent-switch reprint
   put every user row into `scrollback()` exactly once.

## Implementation Steps
1. Reproduce on Windows Terminal; write findings (bytes, which hypothesis) into this plan's
   `## Decisions`.
2. Bisect per Design step 2; choose the fix.
3. Implement in `rust/fontaine-tui/src/screen.rs` (and `transcript.rs` only if the cause is in
   row generation).
4. Add the regression tests (Testing Strategy).
5. Extend `tasks/design/testing.md` "Reproducing terminal layout bugs" with the Windows steps.
6. `CHANGELOG.md` entry; run the Essential Commands, including the Windows clippy target check.

## Files to Modify
- `rust/fontaine-tui/src/screen.rs` (fix, harness checks, tests)
- `rust/fontaine-tui/examples/live_area.rs` (if it needs user-message / reprint scenarios)
- `rust/fontaine-tui/src/transcript.rs` (only if the cause is row generation)
- `tasks/design/testing.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (`## Decisions`)

## Trade-offs
- **Diagnosis-first vs. fix-by-guess**: the issue records no confirmed cause and `TestBackend`
  cannot show it; a speculative change risks regressing the Linux scrollback that works.
- **Windows-only branch vs. portable fix**: prefer one emission for all platforms to keep the
  vt100 harness and tmux checks meaningful.

## Decisions
(none yet: filled in by the diagnosis step)

## Documentation
`tasks/design/testing.md` gains a Windows repro section; no `docs/` page describes this behavior.

## Testing Strategy
- vt100-harness regression test pinning the byte pattern that Windows mishandles (names issue
  #543); it must fail before the fix.
- Unit tests: after a user message committed at the top of the screen and after `App::view`
  switches, each user row appears exactly once in `scrollback()`, in order.
- Existing tmux-oriented harness tests stay green, proving Linux output is unchanged.
- The real ConPTY behavior cannot be reached by an automated test in CI (no Windows pty harness
  in the suite); a silent regression there is visible immediately in use, so it stays manual.

## Manual Verification
On Windows Terminal (80x12 and a full-size window), `cargo run -p fontaine`, no provider needed
for the `live_area` example:
1. Run `target\debug\examples\live_area.exe`; scroll the terminal's scrollback up. Expected: every
   `note N` / user line appears once, none missing. (Not automated: needs a real ConPTY.)
2. In a real session send several messages until the first scrolls off, scroll back. Expected: all
   user messages visible.
3. Spawn a subagent, switch to it and back (agents panel), scroll back. Expected: the full
   transcript is in scrollback once.

## Open Questions
- Does the bug reproduce in legacy conhost as well as Windows Terminal? (Decided by step 1; fix
  targets both if so.)
