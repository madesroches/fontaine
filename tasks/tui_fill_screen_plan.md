# TUI Fill Screen Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/74

## Overview

Since #72 the TUI draws inline: settled transcript lines go into the terminal's scrollback and only
a live area (the unsettled transcript tail, any open panel, the input box and the status bar) is
redrawn. That live area is exactly as tall as its content and starts at the shell's cursor row, so
`fontaine` opens as a few rows in the middle of the shell's output. This plan makes the TUI own the
whole terminal from startup, with the input box and status bar on the bottom rows, and keeps it
that way across commits, panels opening and closing, and resizes. It stays inside the inline model,
so tmux selection, wheel scrolling and search keep working.

## Current State

- `fontaine-tui/src/screen.rs` `Screen::new` builds `Terminal` with `Viewport::Inline(0)` at the
  cursor row. `Screen::render` (`screen.rs:150`) commits settled lines
  (`app.transcript.commit(width)`), asks `view::height(app, width, terminal_height)` for the
  content height, rebuilds the terminal when that differs from `self.height`, then
  `insert_before`s the committed lines in 512-row chunks and draws.
- `Screen::rebuild` (`screen.rs:170`) moves the cursor to the viewport top, clears after it, and
  builds a new `Terminal` with `Viewport::Inline(height)`, answering ratatui's cursor query locally
  through `Anchored::arm`.
- ratatui (`ratatui-core-0.1.2/src/terminal/inline.rs`):
  - `compute_inline_size` appends `height - 1` newlines from the cursor row, so a viewport that
    does not fit below the cursor scrolls the screen up until it ends on the bottom row; rows
    scrolled off the top go to the terminal's history.
  - `insert_before_scrolling_regions`: when there are free rows below the viewport, it draws the
    inserted lines at the viewport top and moves the viewport down (up to the free rows); the rest
    scroll the region above the viewport up. When the viewport is as tall as the terminal, each
    line is pushed straight into history through a one-row region at row 0.
  - `autoresize` → `resize` recomputes an inline viewport from the cursor at its fixed height.
- Consequences today: on startup the live area sits wherever the shell's cursor was. It grows
  downward as lines commit until it reaches the bottom row. When a panel closes, the viewport
  keeps its top row and the freed rows under it stay blank until later commits fill them
  (`shrinking_and_committing_in_one_render_at_the_bottom_keeps_every_line_once`). A terminal made
  taller leaves blank rows below the live area.
- `view::draw` (`view.rs:59`) lays the frame out as transcript tail (`Min(0)`), overlay, input,
  status bar. Extra rows in the frame go to the transcript tail area, which draws top-aligned, so
  a frame taller than its content shows the live tail right under the committed lines, then
  blank rows, then the panel, input box and status bar at the bottom.
- `Screen::clear_live` clears from the viewport top down and leaves the cursor there for exit.
- Claude Code 2.1.309 (captured with `script` in a 30-row pty) neither enters the alternate screen
  nor clears the screen at startup; it renders from the cursor row.

## Design

**Rule:** the live area always ends on the terminal's bottom row. Its height is
`max(content, rows from the frontier to the bottom)`, where the frontier is the row right below the
last committed line on screen (the viewport top). Rows the content does not need are padding,
drawn by `view::draw` as blank space between the transcript tail and the panel/input box. Padding
is consumed by later commits before anything scrolls, so the transcript stays top-aligned and
contiguous and no blank holes enter scrollback.

**Startup.** `Screen::new` reads the terminal size and builds `Viewport::Inline(terminal height)`.
`compute_inline_size` then scrolls everything above the cursor into the terminal's history (it is
kept, not erased) and places the viewport at row 0, covering the whole screen. No explicit clear.
`self.height` starts at the terminal height.

**Render.** With `H` the terminal height after `autoresize`, `top` the viewport top, `n` the
number of committed lines and `content = view::height(app, width, H)`:

```
pre = max(content, (H - top).saturating_sub(n)).min(H)
if pre != self.height { rebuild(pre) }        // rebuild keeps `top`, scrolls up if pre doesn't fit
insert_before(lines)                          // fills the free rows below, then scrolls
draw
```

- `n == 0`: `pre = max(content, H - top)`. This covers a growing panel (taller than the free
  rows → the rebuild scrolls the transcript up), a shrinking one (no rebuild: the extra rows become
  padding), and a terminal made taller (rebuild to reach the new bottom row).
- `n > 0` with padding: `pre` leaves exactly `min(n, padding)` free rows below the viewport, so
  `insert_before` writes the lines at the frontier and moves the viewport down to end on row `H`;
  any lines beyond the padding scroll the region above as today.
- After every render: `area.bottom() == H` and `area.height == max(content, H - area.top())`.
- At most one rebuild per render, as today. Rebuilds become more frequent only while padding
  exists and lines commit (the first screenful, and after a panel closes).
- When `content == H` and lines commit, ratatui's full-height path pushes them straight into
  history; nothing changes there.

**View.** No change: `view::draw` already gives extra rows to the transcript tail area. `view::height`
keeps returning the content height.

**Exit.** No change: `clear_live` clears from the viewport top (the frontier), so the shell prompt
returns right under the transcript.

**Resize.** `autoresize` runs first as today; the rule above then rebuilds to reach the new bottom
row. Narrowing clears the screen (ratatui's `resize`), after which the live area fills the whole
screen from row 0.

## Implementation Steps

1. `fontaine-tui/src/screen.rs`
   - `Screen::new`: read `backend.size()?.height`, build `Viewport::Inline(height)`, set
     `self.height = height`. Update its doc comment (the live area starts as the whole screen).
   - `Screen::render`: compute `pre` as above (before inserting) and rebuild when it differs from
     `self.height`; keep the chunked `insert_before` and the draw. Update the module doc comment to
     state the bottom-row rule.
2. Update `screen.rs` unit tests whose expectations assumed a content-height live area (see
   Testing Strategy) and add the new ones.
3. Check the integration harness (`tests/common/mod.rs`) and `tests/tui.rs` still pass; `Ui::live`
   now includes padding rows, which the existing `contains` assertions tolerate.
4. Docs: `docs/tui.md` (see Documentation).
5. Master plan: add a dated line to `tasks/fontaine_harness_plan.md` `## Decisions`.

## Files to Modify

- `rust/fontaine-tui/src/screen.rs`
- `docs/tui.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Pad the live area (chosen)** vs. **anchor a content-height live area to the bottom row and
  leave blank rows above it**: anchoring leaves a gap between the last transcript line and the
  input box, or, if the transcript is shifted down to close the gap after a panel shrinks, blank
  holes in scrollback. Padding keeps the transcript top-aligned and contiguous with no holes.
- **Scroll the shell's output into history at startup (chosen)** vs. **clear the screen
  (`ESC[2J`)**: clearing loses that output in terminals that do not save a cleared screen to
  history; scrolling keeps it everywhere. It also comes for free from ratatui's inline sizing.
- **Fill from the cursor row without scrolling** (keep the shell's output on screen above the live
  area): does not give the TUI the whole screen, which is what the issue asks for.
- **Alternate screen / `Viewport::Fullscreen`**: rejected by #70 (breaks tmux selection and
  scrollback).

## Decisions

## Documentation

- `docs/tui.md`: the intro paragraph says the UI takes the whole terminal on start (the shell's
  earlier output scrolls into the terminal's history), that the transcript reads top-down with the
  input box and status bar on the bottom rows, and that this holds across resizes. The narrowing
  paragraph says the live area is redrawn over the whole screen instead of "at the top".

## Testing Strategy

Unit tests in `screen.rs` on `Screen<TestBackend>`:

- `startup_takes_the_whole_screen_and_keeps_earlier_output_in_history`: a `TestBackend` with text
  on rows 0..3 and the cursor on row 4; after `Screen::new` + `render`, the area is the full
  terminal rect, those rows are in `scrollback()`, and the status bar is on the last row with the
  input box right above it.
- `first_commits_fill_from_the_top`: two committed lines land on rows 0 and 1 (not in scrollback);
  the area still ends on the bottom row.
- `the_live_area_always_ends_on_the_bottom_row`: a scripted sequence (notices, a streaming reply
  that commits row by row, an approval opening and closing, input growing to several rows, enough
  lines to scroll) asserting after every render `area.bottom() == H` and
  `area.height == max(view::height(..), H - area.top())`.
- `committed_lines_stay_contiguous`: after the same sequence, scrollback plus the rows above the
  live area, with trailing blank rows trimmed, hold every committed line exactly once and in order
  with no blank row between them (pins "no holes").
- `a_taller_terminal_is_filled_on_the_next_render`: `TestBackend::resize` to more rows, render,
  area ends on the new bottom row.
- Rework `the_live_area_grows_with_a_prompt_and_shrinks_without_leftovers` (the idle area now fills
  the screen, so assert the panel appears and leaves no text behind rather than height changes)
  and `shrinking_and_committing_in_one_render_at_the_bottom_keeps_every_line_once` (the bottom
  assertion holds for every `late`).

The `tests/tui.rs` integration suite runs unchanged as a regression check of the wiring.

## Manual Verification

Real terminals differ in how they handle the startup scroll and resizes, which `TestBackend` does
not model.

1. In tmux with `mouse on`, run some shell commands, then `cargo run -p fontaine`: the screen is
   blank except the input box and status bar on the bottom rows; scrolling up (wheel / copy mode)
   shows the earlier shell output.
2. Send a message: the reply starts on the top row and grows downward; once the screen is full,
   it scrolls; mouse selection works.
3. Trigger an approval prompt and answer it: the panel opens above the input box and closes
   without leaving blank rows in the transcript when scrolled back.
4. Make the pane taller and shorter (`tmux resize-pane`): the input box stays on the bottom row.
5. Quit: the shell prompt appears right under the transcript.

## Open Questions
