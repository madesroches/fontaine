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
     previous buffer makes the next `draw` repaint everything. ratatui's diff skips blank cells, so
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

Two independent changes; the first alone makes the flash invisible in terminals that paint per
read chunk, the second removes the blank state altogether.

### 1. One write per frame

- Add a backend in `screen.rs` that wraps `CrosstermBackend<BufWriter<W>>` (implements `Backend`
  and `Anchor`, replacing the plain `Anchored<CrosstermBackend<Stdout>>` in `lib.rs` and the
  `live_area` example; `Anchored` stays as the cursor-answering wrapper around it).
- `Backend::flush` on that backend becomes a no-op while a frame is open. `Screen` gains
  `begin_frame` / `end_frame` hooks on the `Anchor` trait (no-ops for `TestBackend`):
  `begin_frame` queues `BeginSynchronizedUpdate` into the buffer, `end_frame` queues
  `EndSynchronizedUpdate` and flushes once. `Screen::render` and `commit_all` call them around
  `draw_frame`, so the sync markers travel in the same write as the frame, and `lib.rs` and the
  example drop their own `execute!` sync calls.
- The buffer is flushed even when `draw_frame` errors, so a failed frame does not leave a
  dangling begin-sync.
- Other writers of the terminal (`clear_live`, `resume`, the `Suspend` closure) flush as before;
  the frame hooks are only used by `render`/`commit_all`.

### 2. Overwrite instead of blank

Replace the blank-then-repaint in `rebuild` with an in-place overwrite: the freshly built
`Terminal` must repaint every cell of the viewport including blanks, without a preceding erase.
The mechanism is chosen by a short spike in the emulated harness, in this order of preference:

1. Make ratatui's previous buffer unequal to every cell, so `Buffer::diff` emits blanks as
   explicit spaces (or a clear-to-end-of-line per row), e.g. by drawing one sentinel frame
   whose cells differ from any real cell into the new terminal's buffer before the real draw
   (only the second draw's bytes reach the terminal if the sentinel draw is discarded by
   building it against a `TestBackend`-style sink is not possible, so the spike must show a
   workable route through ratatui's public API).
2. A one-off direct write of the viewport rows by `Screen` (cursor move, content, clear to end of
   line per row) before handing the area to ratatui's diff.
3. If neither is workable without forking ratatui, keep `erase_rows` and rely on change 1 alone,
   recording that in `## Decisions`.

The `one_row_region` pre-insert erase exists so the row `insert_before` scrolls over does not keep
stale text; the same overwrite approach applies, and the second erase before the repaint goes
away with it. Whatever route is chosen must keep the invariant that no stale cell survives
(existing `paint`-based harness tests) and must not erase from the top-left in a single clear
(tmux scrollback, 78b3535).

## Implementation Steps

1. Frame buffering: add the buffered backend and the `begin_frame`/`end_frame` hooks in
   `rust/fontaine-tui/src/screen.rs`; move sync markers out of `rust/fontaine-tui/src/lib.rs`
   (`run`, ~497) and `rust/fontaine-tui/examples/live_area.rs`; wire `LiveScreen` and both
   `Screen::new` call sites to the new backend.
2. Test for step 1 (red first): extend the `emulated` harness `Sink` to count `write` calls and
   record the bytes of each; assert one `render` that changes the live area height and one that
   commits settled lines with the viewport at row 0 each produce exactly one write beginning with
   `\x1b[?2026h` and ending with `\x1b[?2026l`.
3. Spike for change 2 in the harness; pick the mechanism and record it under `## Decisions`.
4. Implement change 2 in `rebuild`/`draw_frame`. Test (red first): after every `render` in the
   existing height-change and commit scenarios, replay the frame's bytes in prefixes (split at
   every escape-sequence boundary) into a fresh emulator and assert no live row that holds content
   both before and after the frame is ever blank in an intermediate state (the flash this issue
   reports).
5. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p
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

- Buffering alone is the safe floor: it works without any terminal feature and is mechanical. It
  does not remove the blank state, only makes the terminal see it and its repaint together.
- Keeping ratatui's diff across a height change (issue's first direction) is blocked because
  `Viewport::Inline` cannot be resized in place; hence the overwrite-in-place approach on the
  rebuilt terminal.
- Not chosen: detecting mode 2026 support and branching. It adds a query round trip (which
  `Anchored` exists to avoid) and still leaves unsupported terminals flashing.

## Documentation

No `docs/` page describes frame painting; no doc change beyond the changelog.

## Testing Strategy

- Unit/emulated tests as in Implementation Steps 2 and 4: one write per frame with sync markers
  inside it, and no intermediate blank for rows that keep content. These are the regression tests
  for #220; the emulator sees bytes, which `TestBackend` cannot.
- Existing `emulated` and `TestBackend` screen tests stay green (stale-cell and scrollback
  invariants).

## Manual Verification

1. In tmux (a version without sync support, or `tmux set -g` as available), run
   `cargo run -p fontaine-tui --example live_area` and watch rows being added: expect no visible
   flashing. Not automated because only a human can judge what a real terminal paints.

## Open Questions

- Which in-place overwrite route (Design 2) is workable through ratatui's public API; settled by
  the spike in step 3.
