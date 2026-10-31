# TUI Live-Area Duplicate Prompt Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/279

## Overview

Issue #279: during a running turn with the slash-command popup open, a stale copy of the live area
(popup, task line, message box) is left behind instead of the live area being redrawn in place.
The same capture shows two further defects: a refused slash command prints its "a turn is running"
notice into scrollback once per Enter, and the message box's bottom border lacks its closing `┘`.
This plan reproduces the duplication first (the root cause is not yet established), fixes it at its
cause, and makes the refusal notice transient.

## Current State

- `Screen::draw_frame` (`rust/fontaine-tui/src/screen.rs:392`) sizes the inline viewport to
  `max(view::height, rows free below the viewport top)`. When the wanted height differs from the
  current one it calls `rebuild(wanted)`, which re-creates the ratatui `Terminal` at the same top
  row (`rebuild_at`, `screen.rs:489`) and marks the next draw as a full repaint (`repaint`).
- `view::chrome` / `view::height` (`view.rs:51`, `view.rs:89`) count the overlay (the `/` menu is
  one overlay, `view.rs:428`), queue, agent list, task list, message box and status line. Opening
  or closing the popup changes the height, so every keystroke that changes the match count
  triggers a rebuild.
- Held-back transcript rows are released with `commit_within(over)` so the chrome fits the screen
  (`screen.rs:407`).
- Existing coverage: the vt100 `emulated::Harness` scenarios (`screen.rs:1462` onward) and the
  `live_area` example. None opens the `/` menu, or any overlay, while tool rows are live.
- The refusal notice is `self.transcript.notice(...)` in the `Some(Command::Loop(_)) |
  Some(Command::Other { .. })` arm of the running-turn submit (`app.rs:1327`). A transcript entry is
  permanent and is committed to scrollback, so each Enter adds a line. The input is deliberately
  left in place, so the user presses Enter again and gets another.
- The popup opening mid-turn is intended: `/` completion is shared with the idle state, and several
  commands run mid-turn (`Command::runs_mid_turn`, `commands.rs:105`).
- The captured screen in the issue shows a single popup, task line and box; the stale copy was
  seen by the reporter in the live terminal and may only be visible in tmux scrollback.

## Design

### 1. Reproduce, then fix the duplication

Reproduce before changing anything:

1. Add a vt100 harness scenario that starts live tool rows, then types `/` and further characters
   (menu opens, narrows, closes) and presses the refused Enter, at heights 6–24, with a task list
   present and without. Every frame is already checked against the emulator, including the rule
   that fails any erase from the top-left corner.
2. If the harness does not reproduce, extend `examples/live_area.rs` with the same sequence and run
   it in tmux as `tasks/design/testing.md` ("Reproducing terminal layout bugs") describes, reading
   the `ESC[?2026h`…`ESC[?2026l` frames via `script` to see which sequence leaves the stale rows.
   Capture the terminal size the reporter used if it can be inferred; otherwise sweep heights.
3. Fix at the cause found (candidates, in order of likelihood: the viewport grows past the screen
   bottom on `rebuild` so ratatui scrolls the old live rows into history instead of overwriting
   them; a rebuild-then-`insert_before` ordering when a notice is committed in the same frame as a
   height change; the one-row-region path at `screen.rs:431`). Pin the repro as a regression test
   named for issue 279 in the harness; the tmux steps remain the manual check.

### 2. Truncated bottom border

Add one test drawing the message box at about 59 columns with the popup open, asserting the box's
last row ends in `┘` (`draw_input`, `view.rs:575`, uses `Borders::ALL`, so the missing corner should
be a paste trim). Fix only if the test fails.

### 3. Transient refusal notice

The refusal ("a turn is running; wait for it or press Esc to interrupt") must not enter the
transcript. Show it as a one-line, dimmed notice that replaces the hint segment of the status line
(`statusbar.rs` composes the hint as the tail of the line); `statusbar::render` takes `&App`, so no
`view.rs` plumbing is needed. State lives on `App` (an `Option<String>`), not in the transcript, so
nothing is committed to scrollback and repeated Enters leave one line. Clear it at the top of
`handle_key` (`app.rs:685`), before dispatch, and in `settle_turn` (`ui/turn.rs`, where `running`
is set to false at turn end). Clearing before dispatch matters: the refusal is set in
`queue_during_turn` (`app.rs:1327`), reached from `handle_key`, so clearing at the end would wipe it
at once. Behavior otherwise unchanged: the input stays, the popup stays.

## Implementation Steps

1. Add the harness scenario (popup open/narrow/close and refused Enter while tool rows are live,
   across heights); run it; if green, repro in tmux via the example (design 1.2).
2. Fix the root cause in `screen.rs` (or `view.rs` if the height is miscomputed); keep the new test.
3. Add the border test (design 2).
4. Replace the transcript notice at `app.rs:1327` with the transient status (design 3); render it
   in `statusbar.rs`; clear it at the top of `handle_key` and in `settle_turn`.
5. Update the existing test at `app.rs:1940` and add tests for the transient behavior.
6. `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-tui/src/screen.rs`: fix and regression test.
- `rust/fontaine-tui/src/view.rs`: only if the height or border is miscomputed.
- `rust/fontaine-tui/src/app.rs`, `statusbar.rs`: transient refusal status.
- `rust/fontaine-tui/examples/live_area.rs`: popup sequence for the tmux repro, if needed.
- `CHANGELOG.md`.

## Trade-offs

- Transient notice rather than deduplicated transcript entries: a deduplicated notice would still
  be permanent history for a keypress's feedback.

## Decisions

- 2026-10-31: If neither the harness nor tmux reproduces the duplication after the sweep, record the
  finding here, ship the other two fixes, and leave the primary item open with the evidence.
- 2026-10-31: If the border test passes, the missing `┘` is a paste trim and not a defect; record
  that result here.

## Testing Strategy

- Harness regression test for the duplicated live area (a bug that happened in the wild; vt100
  emulation is the tier that reproduces it, tmux is the manual confirmation).
- Unit tests: refusal sets the transient status and leaves the transcript unchanged; repeated
  Enter leaves one status; the status clears on input change and on turn end; border width at the
  issue's width with the popup open.

## Manual Verification

1. From `rust/`, run `cargo build -p fontaine-tui --example live_area`, then run it in a detached tmux (`env -u TMUX
   tmux -L repro -f /dev/null new-session -d -s repro -x 80 -y 12 target/debug/examples/live_area`),
   then `tmux -L repro capture-pane -p -t repro -S - -E -`; expect one `message ·` box and each
   note once. Not automated because tmux's handling of erase and scroll differs from the vt100
   model.
