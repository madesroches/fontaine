# TUI Screen Flash Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/220

## Overview

The TUI flashes now and then when content is added to the chat (issue #220). Whenever the live
area changes height, or settled lines are committed with the viewport at row 0/1, `Screen` blanks
rows on the terminal and repaints them. A terminal that paints the blank intermediate state
shows a flash. The existing `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` wrapper only helps
where the terminal honors mode 2026 (older tmux and some WSL/ConPTY setups do not), and one frame
can reach the terminal as several chunks. The fix makes each frame reach the terminal as one
write, and stops blanking rows that are about to be repainted anyway.

## Current State

- `Screen::draw_frame` (`rust/fontaine-tui/src/screen.rs`) goes through `terminal.draw` (diffed
  writes) except in two cases:
  1. `wanted != self.height` calls `rebuild`, which runs `erase_rows` (per row: cursor move +
     clear line) from the viewport top to the bottom, then builds a fresh `Terminal` whose empty
     previous buffer makes the next `draw` repaint everything. ratatui's diff skips cells unchanged from its previous buffer, so
     the erase is what clears stale text under blank cells.
  2. `one_row_region` (viewport top <= 1 and lines being committed): `erase_rows`, `insert_before`,
     `rebuild` (second erase), then a full repaint.
- `lib.rs` (`run`, ~line 497) wraps each `render`/`commit_all` in sync begin/end through a
  separate `std::io::stdout()` handle. The backend is `Anchored<CrosstermBackend<Stdout>>`;
  `Stdout` is line-buffered, so escape-only output without a newline is flushed at ratatui's
  explicit `Backend::flush` calls, i.e. at several points per frame (`insert_before`, `draw`).
- `live_area` example and the `emulated` test harness (`screen.rs` tests, `vt100`) already model
  what a terminal does with the bytes, via a `Sink` that receives every `write`.
- Commit 78b3535 replaced a block erase with per-row cursor-move + clear (to keep tmux from moving
  the screen into scrollback when erasing from the top-left); that fix must stay.

## Design

Two independent changes; the first alone works without any terminal feature and makes the flash
invisible in terminals that paint per read chunk, the second removes the blank state altogether.

### 1. One write per frame

- Add a backend in `screen.rs` that wraps `CrosstermBackend<FrameWriter>` (implements `Backend`
  and `Anchor`, replacing the plain `Anchored<CrosstermBackend<Stdout>>` in `lib.rs` and the
  `live_area` example; `Anchored` stays as the cursor-answering wrapper around it).
- `FrameWriter` is a custom `Write` holding an unbounded `Vec<u8>` frame buffer (no `BufWriter`: its
  8KB default writes through when exceeded, and real stdout is a `LineWriter` that splits at
  newlines, so one trait-level write is not one `write(2)`). `CrosstermBackend` calls
  `writer.flush()` directly (`hide_cursor`, `show_cursor`, `set_cursor_position`, `clear_region`,
  `append_lines`, `scroll_region_*`), so gating lives in `FrameWriter::flush`: a no-op while a
  frame is open. `end_frame` writes the whole `Vec` once to a locked/raw stdout handle.
- `Screen` gains `begin_frame` / `end_frame` hooks on the `Anchor` trait (no-ops for
  `TestBackend`): `begin_frame` queues `BeginSynchronizedUpdate` into the buffer, `end_frame`
  queues `EndSynchronizedUpdate` and writes the buffer once. `Screen::render` and `commit_all` call
  them around `draw_frame`, so the sync markers travel in the same write as the frame, and `lib.rs`
  and the example drop their own `execute!` sync calls. `restore_terminal`'s
  `EndSynchronizedUpdate` stays as a safety net.
- A failed frame still calls `end_frame`, so it does not leave a dangling begin-sync.
- Other writers of the terminal (`clear_live`, `resume`, the `Suspend` closure) flush as before;
  the frame hooks are only used by `render`/`commit_all`.

### 2. Overwrite instead of blank

Replace the blank-then-repaint in `rebuild` with an in-place overwrite: the freshly built
`Terminal` must repaint every cell of the viewport including blanks, without a preceding erase.
The mechanism is a sentinel buffer swap (`current_buffer_mut`, `swap_buffers` and `Buffer::filled`
are public in ratatui-core 0.1.2). `rebuild` and `resume` only set a repaint flag; `draw_frame`
installs the sentinel immediately before `terminal.draw`, after all `insert_before` calls
(`insert_before`'s fullscreen branch would draw the sentinel row as `\x01` bytes):
`*terminal.current_buffer_mut() = Buffer::filled(area, Cell::new("\u{1}"))` then `swap_buffers()`.
The swap itself writes nothing, and the `draw` emits every cell, blanks included as spaces, so no
preceding erase is needed.

`erase_rows` is removed from `rebuild` only. The `one_row_region` pre-insert erase (before
`insert_before`) exists so the row `insert_before` scrolls over does not keep stale text; whether
it is dropped or kept is decided by the harness stale-cell tests. `clear_live` keeps its own
erase. Whatever route is chosen must keep the invariant that no stale cell survives
(existing `paint`-based harness tests) and must not erase from the top-left in a single clear
(tmux scrollback, 78b3535).

## Implementation Steps

1. Frame buffering: add the buffered backend and the `begin_frame`/`end_frame` hooks in
   `rust/fontaine-tui/src/screen.rs`; move sync markers out of `rust/fontaine-tui/src/lib.rs`
   (`run`, ~497) and `rust/fontaine-tui/examples/live_area.rs`; wire the new backend in at
   the `LiveScreen` alias (`lib.rs:339`), `Screen::new` (`lib.rs:430-432`, including the placeholder
   closure) and `live_area.rs` 19-27. The test harness (`Emu`, `Real`, `spare()`, `Harness::new` in
   `screen.rs`) wraps `CrosstermBackend<Sink>` and needs the same writer wrapper; the counting layer
   must sit above `Sink` (which splits at `\x1b[J`). The test at `screen.rs:~1166` needs the updated
   `Anchor` trait.
2. Test for step 1 (red first): extend the `emulated` harness `Sink` to count `write` calls and
   record the bytes of each; assert (at the writer-trait level, not `write(2)`) one `render` that changes the live area height
   and one that commits settled lines with the viewport at row 0 each produce exactly one write beginning with
   `\x1b[?2026h` and ending with `\x1b[?2026l`.
3. Implement change 2 in `rebuild`/`resume`/`draw_frame`. Test (red first): extend `Sink` with a recording mode that splits each
   frame at escape boundaries and feeds the same parser incrementally, asserting after each piece
   that no live row holding content both before and after the frame is blank (the flash this issue
   reports), in the existing height-change and commit scenarios.
4. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p
   fontaine-tui`; reproduce the old and new behavior in a real tmux with the `live_area` example
   (see `tasks/design/testing.md`, "Reproducing terminal layout bugs"), and add a `CHANGELOG.md`
   fix entry.

## Files to Modify

- `rust/fontaine-tui/src/screen.rs` (buffered backend, frame hooks, `rebuild`, `draw_frame`,
  harness and tests)
- `rust/fontaine-tui/src/lib.rs` (backend type, drop external sync calls)
- `rust/fontaine-tui/examples/live_area.rs`
- `CHANGELOG.md`

## Trade-offs

- Keeping ratatui's diff across a height change (issue's first direction) is blocked because
  `Viewport::Inline` cannot be resized in place; hence the overwrite-in-place approach on the
  rebuilt terminal.
- Not chosen: detecting mode 2026 support and branching. It adds a query round trip (which
  `Anchored` exists to avoid) and still leaves unsupported terminals flashing.

## Documentation

No `docs/` page describes frame painting; no doc change beyond the changelog.

## Testing Strategy

- Unit/emulated tests as in Implementation Steps 2 and 3: one write per frame with sync markers
  inside it, and no intermediate blank for rows that keep content. These are the regression tests
  for #220; the emulator sees bytes, which `TestBackend` cannot.
- Existing `emulated` and `TestBackend` screen tests stay green (stale-cell and scrollback
  invariants).

## Manual Verification

1. Follow the tmux steps in `tasks/design/testing.md`, "Reproducing terminal layout
   bugs" (commands run from `rust/`), then attach to the session (`tmux -L repro attach -t repro`)
   and watch the `live_area` example add rows: expect no visible flashing. Not automated because only a human can judge what a real terminal paints.

## Decisions

- 2026-10-26: Design 2 uses the sentinel buffer swap (`current_buffer_mut` + `swap_buffers`) rather than a direct row write or keeping `erase_rows`.
