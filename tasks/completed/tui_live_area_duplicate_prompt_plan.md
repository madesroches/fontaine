# TUI Live-Area Duplicate Prompt Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/279

## Overview

Issue #279: during a running turn a stale copy of the live area (popup, task line, message box) is
left in the terminal's history above the live one. Three defects were reported; the root cause of the
first is established: a terminal **resize**. Not the `/` menu and not the refused Enter. The second
defect, a refused slash command printing its "a turn is running" notice into scrollback once per
Enter, is real and fixed by making the notice transient. The third, the message box's bottom border
lacking its closing `┘`, is a reflow artifact of the stale copy, not a drawing bug.

## Current State

- Root cause, reproduced in tmux and in the vt100 harness: `Screen::draw_frame`
  (`rust/fontaine-tui/src/screen.rs`) begins with `self.terminal.autoresize()`. For an inline
  viewport, ratatui 0.30's `Terminal::resize` (a) emits `ESC[2J` when the terminal got narrower and
  (b) always clears from the viewport's top (move there, `ESC[J`). The live area is nearly always the
  whole screen (viewport top row 0), so both are erases from the top-left; tmux answers by moving
  every visible row into its history (the behaviour `erase_rows` already works around), leaving the
  old live area in scrollback, and its reflow of those rows scrambles them.
- Every resize tried duplicates (80x30 to 80x20, to 79x30, 119x30 to 59x30, 59 to 119, 119 to 59 to
  58). No-resize sweeps (heights 6-30, widths 40-100, with and without a task list, in vt100 and real
  tmux) never do. The issue capture's transcript rows are 115-118 columns wide while its boxes are 59,
  so the width changed (tmux zoom or split).
- `Screen::reprint` (`screen.rs`) already handles a full redo: purge (`ESC[2J` then `ESC[3J`),
  `rebuild_at(0, height)`, transcript recommitted at the current width. It runs when `app.reprint` is
  set; nothing sets it on a resize.
- The render loop draws once per loop iteration, after handling one input (`ui/mod.rs`,
  `ui/turn.rs`); each `Input::Resize` is one such iteration. Draws are not coalesced per event batch,
  but `Screen` reads the current terminal size, so queued stale resize events add no reprint once the
  size has been seen.
- The refusal notice is `self.transcript.notice(...)` in the `Some(Command::Loop(_)) |
  Some(Command::Other { .. })` arm of the running-turn submit (`app.rs:1327-1329`). A transcript
  entry is committed to scrollback, so each Enter adds a line; the input is deliberately left in
  place, so the user presses Enter again. Since #303 `/guardian-enable` mid-turn is queued, not
  refused; unknown commands and `/loop` still are, so the issue's "popup open, Enter twice" detail is
  incidental.
- Existing coverage: the vt100 `emulated::Harness` scenarios (`screen.rs` `emulated` module, which
  fails any erase from the top-left corner) and the `live_area` example. Neither resizes.

## Design

### 1. Reprint on resize

`Screen` keeps a `size: Size` set in `new()`. `draw_frame` compares `terminal.size()` with it before
anything else; on a change it stores the new size and sets `app.reprint`, so the existing reprint
path runs (purge, `rebuild_at(0, height)`, transcript recommitted at the new size), and
`autoresize()` moves after the reprint, where the rebuilt terminal leaves it nothing to resize. A
comment states why: ratatui's inline resize erases from the viewport top, which tmux turns into
history.

### 2. Truncated bottom border

Established as a reflow/selection artifact of the stale rows: the box ends in `┘` at 59 columns with
the popup open. Add one cheap guard test drawing the message box at 59 columns with the popup open,
asserting the last row ends in `┘`. No fix is expected.

### 3. Transient refusal notice

The refusal ("a turn is running; wait for it or press Esc to interrupt") must not enter the
transcript. Show it as a one-line, dimmed notice that replaces the hint segment of the status line
(`statusbar.rs`); it is checked first in the hint chain, ahead of the viewed-state, running and idle
hints, and the other segments are kept (the status line already clips on a narrow terminal).
`statusbar::render` takes `&App`, so no `view.rs` plumbing is needed. State lives on `App` (an
`Option<String>`), not in the transcript, so nothing is committed and repeated Enters leave one
line. Clear it at the top of `handle_input` (`app.rs:625`), before `dispatch_input`, so keys and
paste (which never reaches `handle_key`) both clear it, and in `settle_turn` (`ui/turn.rs`). Render
it only while `app.running`: `ui/goal.rs:43` and `ui/turn.rs:398` clear `running` without
`settle_turn`, and the notice must not outlive the turn. Clearing before dispatch matters: the
refusal is set in `queue_during_turn` (`app.rs:1308`), reached from `handle_key`, so clearing at the
end would wipe it at once. The input and popup stay. The sibling shell-run refusal stays a
transcript notice.

## Implementation Steps

Reproduction is done; the work is:

1. `screen.rs`: add the `size` field and the resize check in `draw_frame` (design 1).
2. Port the harness changes: `Harness::resize(w, h)` (vt100 `set_size`); the `Sink` also flags an
   `ESC[2J` not followed by `ESC[3J` in the same write; `popup_scenario(width, height, todos,
   extra_notices, resizes)`; test `issue_279_a_resize_leaves_no_copy_of_the_live_area_in_history` over
   several sizes and resize sequences; an `#[ignore]` no-resize sweep `issue_279_sweep`.
3. `examples/live_area.rs`: a `popup` mode (the `/` menu typed open mid-turn with a task list, a
   finished shell call and extra transcript notices, then held drawing) for the tmux resize repro.
4. Add the border guard test (design 2).
5. Replace the transcript notice at `app.rs:1327` with the transient status (design 3); render it in
   `statusbar.rs`; clear it in `handle_input` and `settle_turn`; render only while `app.running`.
6. Update the existing test at `app.rs:1935-1943`; add tests for the transient behavior.
7. `docs/tui.md` (~line 226): the refusal of unknown slash names and `/loop` is a transient status
   hint, not a transcript line. Note that a resize reprints the history.
8. `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-tui/src/screen.rs`: resize fix, harness changes, regression test, sweep.
- `rust/fontaine-tui/examples/live_area.rs`: `popup` mode.
- `rust/fontaine-tui/src/app.rs`, `statusbar.rs`: transient refusal status.
- `rust/fontaine-tui/src/view.rs`: border guard test only.
- `docs/tui.md`, `CHANGELOG.md`.
- `tasks/fontaine_harness_plan.md` `### TUI` Decisions: the resize deviation line.

## Trade-offs

- Transient notice rather than deduplicated transcript entries: a deduplicated notice would still
  be permanent history for a keypress's feedback.
- Reprint on every size change rather than patching ratatui's resize: reuses a tested path and keeps
  one definition of a clean screen; the cost is a scrollback purge and transcript rewrite per change.

## Decisions

- 2026-10-31: The border test is a guard only; the missing `┘` was a reflow artifact of the stale rows.
- 2026-10-31: AFK: The shell-run refusal ("a shell command is running; wait for it or press Esc to interrupt", `nothing_is_sent_or_queued_during_a_shell_run`) stays a transcript notice; it is outside #279's resize defect. Follow-up: make it transient too.
- 2026-10-31: AFK: The transient notice is checked first in the status hint chain and replaces only the hint segment; other segments stay and narrow-terminal clipping is accepted, as the status line already clips.
- 2026-10-31: AFK: A resize purges the scrollback and rewrites the transcript on every size change, height-only included, with no debounce, reusing the reprint path. The loop draws once per input event, not per batch, so a window-edge drag reprints per distinct size; the cost is accepted for correctness. Claude Code's resize behaviour was not verified (`claude -p` cannot show it); this is a deviation to confirm and, if it differs, to reconcile.

## Testing Strategy

- Harness regression test for the resize (a bug that happened in the wild; vt100 emulation is the
  tier that reproduces it, tmux is the manual confirmation): after each resize no chrome row is in
  history, every note appears exactly once, and no `ESC[2J` goes unpaired with `ESC[3J`. The
  `#[ignore]` no-resize sweep documents that fixed sizes were never the cause.
- Border guard test at 59 columns with the popup open.
- Unit tests: refusal sets the transient status and leaves the transcript unchanged; repeated Enter
  leaves one status; it clears on the next input event (key or paste) and on turn end, and does not
  render when `running` is false; it takes the hint segment and keeps the others.

## Manual Verification

1. From `rust/`, run `cargo build -p fontaine-tui --example live_area`, then in a detached tmux
   (`env -u TMUX tmux -L repro -f /dev/null new-session -d -s repro -x 80 -y 30
   target/debug/examples/live_area popup`) run `tmux -L repro resize-window -t repro -x 59 -y 30`
   (and back), then `tmux -L repro capture-pane -p -t repro -S - -E -`; expect one `message ·` box
   and each note once. Not automated because tmux's handling of erase and scroll differs from the
   vt100 model.
