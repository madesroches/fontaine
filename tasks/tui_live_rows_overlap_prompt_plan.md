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
- `Screen::draw_frame` (`fontaine-tui/src/screen.rs:~175`) commits what no longer fits
  (`commit_within`), computes the live area's height `wanted = max(content, free)` and, when it
  differs from `self.height`, calls `rebuild`. `rebuild` (`screen.rs:~215`) moves the cursor to
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

Add a dev-only terminal emulator to `fontaine-tui` (`vt100`, exact-pinned like every other
dependency; check `cargo deny check` and its license against `rust/deny.toml`). A test helper
drives a real `Screen<Anchored<CrosstermBackend<SharedBuf>>>` where `SharedBuf` collects the
written bytes, feeds the bytes to the emulator after each `render`, and exposes two screens:

- the model screen: ratatui's `Terminal` buffer (what `TestBackend` shows today);
- the real screen: the emulator's visible rows.

Cursor-position queries are already answered locally by `Anchored::arm`; `Anchored` falls through
to the inner backend otherwise, so the helper answers those from the emulator's cursor.

### 2. Find the divergence

Sweep scenarios through the helper and compare the emulator's rows to the model's after every
frame; the first frame where they differ is the bug. The sweep covers terminal heights 6..24 and
widths 20..80, a transcript full enough that the viewport starts at the bottom, and 1..4 calls
started one at a time and in one batch, each completing in different orders. Candidate causes
to check, most likely first:

1. A rebuild at the bottom row: the newline-scroll in `compute_inline_size` runs from the
   viewport's old top, so any row the emulator scrolls differently from ratatui's model (for
   example the cursor sitting in the deferred-wrap state in the last column after the previous
   frame) leaves the new viewport offset from where ratatui believes it is.
2. Rows exactly `width` columns wide: `line + " …"` fills the last column, and the next frame's
   diff skips cells it believes are unchanged.
3. Ambiguous-width glyphs (`•`, `…`) counted as one column by ratatui and `truncate`, drawn as
   two by the terminal, shifting the rest of the row and the cells after it.
4. The synchronized-update markers going through a different `Stdout` handle than the
   backend's, so the begin/end markers and the frame can interleave through the shared
   `LineWriter`, which flushes at each newline `append_lines` writes.

### 3. Fix at the cause

The fix is not designed until step 2 names the cause; whichever it is must keep the properties
in `screen.rs`'s module comment (live area ends on the bottom row, settled rows written once).
Fix direction per candidate: (1) anchor the new viewport with an absolute cursor move to the
bottom row before the newlines, or scroll with an explicit region; (2) leave the last column
empty in `entry_lines`'s `Tool` arm; (3) measure with `unicode-width` in `truncate` and
`wrap`, which `ratatui` already depends on; (4) write the markers through the backend's writer.
If the cause is a terminal-specific behavior that cannot be modeled, the fallback is a full
repaint after every rebuild: clear each viewport row and redraw every cell, not only the diff.

### 4. Regression test

The reproducing scenario from step 2 becomes an automated test on the emulator helper that
asserts, after every frame, that the emulator's rows equal the model's and that the input
box's top border, input row and bottom border are intact in order above the status row. It is
named for issue 217. Alongside it, a `TestBackend` test for any fix that lives in the model
(truncation by display width, last-column handling).

## Implementation Steps

1. Add the `vt100` dev-dependency and the `SharedBuf` + emulator helper
   (`fontaine-tui/src/screen.rs` tests, or a `tests/common` module if the integration tests need
   it); confirm `cargo deny check` passes.
2. Write the sweep from Design 2 and run it until it fails; record the failing frame and the
   cause under `## Decisions`.
3. Implement the fix for that cause (Design 3).
4. Add the regression test (Design 4) and the model-level test if applicable; run the full sweep
   green.
5. Run the existing `fontaine-tui` tests, clippy and fmt.

## Files to Modify

- `rust/Cargo.toml`, `rust/fontaine-tui/Cargo.toml` (dev-dependency)
- `rust/fontaine-tui/src/screen.rs` (fix and tests, if the cause is in `rebuild`)
- `rust/fontaine-tui/src/transcript.rs`, `rust/fontaine-tui/src/lib.rs` (only if the cause is
  in row width or the synchronized-update write path)

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

- Emulator sweep and regression test (Design 1, 2, 4). This is automated rather than manual
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
