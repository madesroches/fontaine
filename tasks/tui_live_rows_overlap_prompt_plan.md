# TUI Live Rows Overlap Prompt Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/217

## Overview

While tool calls are in flight, the TUI's live transcript rows are sometimes drawn over the prompt
box instead of above it (issue #217): the box's top border and input row are replaced by tool rows,
and one row shows a stale, missing cell. The live rows must stay above the box, which keeps its
full border and input row. The cause is not yet known, because the obvious reproduction in ratatui's
`TestBackend` passes. The plan therefore first builds an oracle that can see the divergence: a
terminal emulator fed the exact bytes the real backend writes. It then fixes the divergence it
finds and pins it with a regression test.

## Current State

- `view::draw` (`fontaine-tui/src/view.rs:86`) lays the frame out as transcript (`Min(0)`),
  overlay, queued, todos, input (`Length`), status row. `draw_transcript` (`view.rs:431`) shows
  the last `area.height` live rows, bottom-aligned, so the layout itself cannot overlap the input.
- `Screen::draw_frame` (`fontaine-tui/src/screen.rs:~172`) commits what no longer fits
  (`commit_within`), computes the live area's height `wanted = max(content, free)` and, when it
  differs from `self.height`, calls `rebuild`. `rebuild` (`screen.rs:~203`) moves the cursor to
  the viewport top, clears after it, builds a new `Terminal` with `Viewport::Inline(wanted)`,
  and answers ratatui's cursor query locally (`Anchored::arm`). ratatui then appends
  `wanted - 1` newlines from that row (`ratatui-core` `compute_inline_size`), so a viewport that
  does not fit scrolls the terminal up until it ends on the bottom row.
- The row for an in-flight call is one `Line` per call (`transcript.rs` `entry_lines`, `Tool`
  arm): `truncate("• {tool} {summary}", width - 2)` by `chars().count()`, then ` …` and an empty
  detail. Counting `chars` rather than display columns is only right for width-1 glyphs; `•`, `…`
  and `✓` are ambiguous-width in some terminals and locales.
- `run` (`lib.rs:~497`) wraps every `Screen::render` in `BeginSynchronizedUpdate` /
  `EndSynchronizedUpdate` written through a separate `std::io::stdout()` handle from the one the
  `CrosstermBackend` owns.
- Existing screen tests (`screen.rs` tests) all run on `TestBackend`, which models ratatui's own
  idea of the screen. A throwaway test on it (40x10, scrollback full, three `shell` calls started
  one render at a time) kept the input box intact on every frame, so a pure ratatui-model bug
  is ruled out for that scenario. What `TestBackend` cannot show is what a real terminal does with
  the bytes (scrolling by newline, DECSTBM regions, tmux, ambiguous widths, auto-wrap at the last
  column, synchronized-update handling), which is where the report came from (tmux).

## Design

### 1. An emulator oracle (diagnosis, kept as test infrastructure)

Add a dev-only terminal emulator to `fontaine-tui` (`vt100` `=0.18.1`, re-check at
implementation time; exact-pinned like every other dependency; check `cargo deny check` and its
license against `rust/deny.toml`). `Anchored<CrosstermBackend<SharedBuf>>` alone still makes real
tty calls: `Anchored` falls through to the inner backend for `size()`, `window_size()` and an
unarmed `get_cursor_position()` (`screen.rs:96-110`), and `CrosstermBackend` implements them with
`crossterm::terminal::size()` (tty ioctl) and `crossterm::cursor::position()` (stdin round trip);
`Screen::new` and every `draw_frame` call `size()`. So the helper defines a small test `Backend`
wrapper around `CrosstermBackend<SharedBuf>` that overrides `size`, `window_size` and
`get_cursor_position` from the emulator, and drives a real `Screen<Anchored<ThatWrapper>>`. The wrapper must also forward `append_lines`,
`scroll_region_up` and `scroll_region_down` to the inner backend: `ratatui-core`'s `append_lines`
default is a no-op, which would silently drop the scrolling newlines, and `scrolling-regions` is
enabled. `Screen` needs a `placeholder: fn() -> B`, so the helper wraps `Anchored<Wrapper>` and
creates the shared `SharedBuf` / emulator state through an `Arc` inside that fn (or a static).
`SharedBuf` collects the written bytes; the helper feeds them to the emulator after each `render`
and exposes two screens:

- the model screen: ratatui's `Terminal` buffer (what `TestBackend` shows today);
- the real screen: the emulator's visible rows.

The full sweep (Design 2) is a one-off local diagnostic, not CI; the helper stays as test
infrastructure for the regression test and a small strided matrix.

### 2. Find the divergence

Sweep scenarios through the helper and compare the emulator's rows to the model's after every
frame; the first frame where they differ is the bug. The full sweep covers terminal heights
6..24 and widths 20..80, a transcript full enough that the viewport starts at the bottom, and
1..4 calls started one at a time and in one batch, each completing in different orders; it is an
`#[ignore]` test run locally. Candidate causes to check, most likely first:

1. A rebuild at the bottom row: `rebuild` already moves the cursor to the viewport top with an
   absolute `set_cursor_position` and clears after it, but the newline-scroll in
   `append_lines` / `compute_inline_size` runs from there, so any row the emulator scrolls
   differently from ratatui's model leaves the new viewport offset from where ratatui believes
   it is.
2. Rows exactly `width` columns wide: `line + " …"` fills the last column, and the next frame's
   diff skips cells it believes are unchanged.
3. Ambiguous-width glyphs (`•`, `…`) counted as one column by ratatui and `truncate`, drawn as
   two by the terminal, shifting the rest of the row and the cells after it.

State `vt100`'s handling of DECSTBM scroll regions and scrollback must be checked in Step 1,
because ratatui is built with `scrolling-regions`.

### 3. Fix at the cause

The fix is not designed until step 2 names the cause; whichever it is must keep the properties
in `screen.rs`'s module comment (live area ends on the bottom row, settled rows written once).
Fix direction per candidate: (1) scroll with an explicit region instead of newlines; (2) leave
the last column empty in `entry_lines`'s `Tool` arm; (3) measure with `unicode-width` (which
`ratatui-core` already depends on) via one width helper that replaces every `chars().count()` width
measurement in `transcript.rs` (`truncate`, `prefixed_body`, assistant row wrapping, the detail
truncate) and is used by `wrap` (defined in `input.rs:197`). If the cause is a terminal-specific behavior that cannot be modeled, the fallback is a full
repaint after every rebuild: clear each viewport row and redraw every cell, not only the diff.

### 4. Regression test

The reproducing scenario from step 2 becomes an automated test on the emulator helper that
asserts, after every frame, that the emulator's rows equal the model's and that the input
box's top border, input row and bottom border are intact in order above the status row. It is
named for issue 217. CI also runs a small strided matrix (every 4th height, every 10th width,
1..3 calls). Alongside it, a `TestBackend` test for any fix that lives in the model
(truncation by display width, last-column handling).

## Implementation Steps

1. Add the `vt100` (`=0.18.1`) dev-dependency and the `SharedBuf` + wrapper-backend + emulator
   helper (`fontaine-tui/src/screen.rs` tests, or a `tests/common` module if the integration
   tests need it); check `vt100`'s DECSTBM and scrollback handling; confirm `cargo deny check`
   passes.
2. Write the full sweep from Design 2 as an `#[ignore]` test and run it until it fails; record the
   failing frame and the cause under `## Decisions`. Add that specific failing scenario as a normal
   (non-ignored) test so it is red before the fix.
3. Implement the fix for that cause (Design 3); the step 2 scenario test goes green.
4. Keep the strided matrix (Design 4) in CI and the model-level test if applicable; run the full
   sweep locally green.
5. Run the existing `fontaine-tui` tests, clippy and fmt.

## Files to Modify

- `rust/Cargo.toml`, `rust/fontaine-tui/Cargo.toml` (dev-dependency; under candidate (3) also
  `unicode-width` in `[workspace.dependencies]`, exact-pinned to the version `ratatui-core`
  resolves, and in fontaine-tui `[dependencies]`)
- `rust/fontaine-tui/src/screen.rs` (fix and tests, if the cause is in `rebuild`, ~line 203;
  `draw_frame` is ~line 172)
- `rust/fontaine-tui/src/transcript.rs` and `rust/fontaine-tui/src/input.rs` (`wrap`) (only if the
  cause is in row width)
- `CHANGELOG.md` (fix entry)

## Trade-offs

- A new dev-dependency is accepted because the bug lives in the gap between ratatui's model
  and a real terminal, which no existing test tier reaches. Guessing a fix from the issue's
  hypothesis without the oracle would have shipped an untestable change.
- Not chosen: replacing the inline viewport with a different layout strategy. The inline model
  is what keeps tmux selection and scrollback working.

## Decisions

- 2026-10-26: a `TestBackend` reproduction of the issue's scenario did not fail, so the plan
  starts with the emulator oracle rather than a presumed fix.

## Documentation

No user docs change: the fix restores documented behavior. `CHANGELOG.md` gets a fix entry.

## Testing Strategy

- Emulator regression test plus a strided matrix in CI; the full sweep is a local `#[ignore]`
  diagnostic (Design 1, 2, 4). This is automated rather than manual
  because the failure is silent: nothing errors, the screen is just wrong, and it depends on
  terminal height, scrollback state and call timing. It catches what `TestBackend` cannot:
  how a real terminal scrolls and wraps the bytes ratatui writes.
- Unit tests on `TestBackend` for model-level fixes.

## Manual Verification

1. In a tmux pane with full scrollback, run `fontaine`, and ask for two long shell calls in
   parallel (for example `sleep 20` with a long argument). Expect both rows above the prompt
   box, which keeps its border and input row. This confirms the fix in the terminal the report
   came from; the emulator is a model of terminals, not tmux itself.

## Open Questions

- Which candidate in Design 2 is the cause; answered by running the sweep.
