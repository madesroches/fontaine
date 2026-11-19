# Ticket Dialog QR Size Message Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/708

## Overview
The remote ticket dialog says "The terminal is too small to show the QR code." for every QR that does not fit, whether the width or the height fell short, and never says how much is needed. On a wide but short terminal the message looks wrong. The dialog should name the dimension that fell short and the size it needs, so the user can enlarge the window.

## Current State
- `rust/fontaine-tui/src/ticket_prompt.rs` `body()` calls `Grid::render(width, height - text_rows)` and maps every `None` to the fixed "too small" reason (line 95).
- `rust/fontaine-tui/src/qr.rs` `Grid::render` returns `None` when `side > max_width || side.div_ceil(2) > max_height`, with `side = width + 2 * QUIET_ZONE`.
- `view.rs` `overlay_in` gives `body()` the rows left after `body_rows` subtracts the input box, queue, agent list, todos and footer from the terminal height, so a long ticket (about 39 QR rows plus 3 text rows) needs a terminal of roughly 44-50 rows. A 190-column terminal can be short enough to fail on height only.
- The width is not the limit in the reported case; the code needs `side` columns (about 73-81).

## Design
1. `qr.rs`: add `Grid::size(&self) -> (usize, usize)` returning `(columns, rows)` the rendered code needs: `(side, side.div_ceil(2))`. `render` uses it for its fit check, so the two cannot disagree.
2. `ticket_prompt.rs`: when a grid exists but `render` returns `None`, build the reason from `Grid::size` and the rows/columns the body was given, naming only the dimension(s) that fall short. `needed_rows` is `size().1 + text_rows`:
   - height short: "The terminal is too short for the QR code: it needs at least N more rows."
   - width short: "The terminal is too narrow for the QR code: it needs at least N more columns."
   - both: the two clauses joined in one line. `N` is the shortfall, a minimum: an expanded todo or agent list can absorb added terminal height. "1 more row" and "1 more column" are singular. The reason becomes an owned `String`; the `NO_COLOR` reason stays as is.
3. When the payload does not encode at all (`grid` is `None`), the reason is "The ticket is too long for a QR code." (before, it shared the "too small" message).
4. Not changed: quiet zone, error-correction level, glyphs, `body_rows`.

## Implementation Steps
1. Add `Grid::size` in `rust/fontaine-tui/src/qr.rs` and use it in `render`.
2. Replace the fixed reason in `ticket_prompt.rs` `body()` with the computed one (reason as `String`).
3. Update the assertions at `ticket_prompt.rs:167` (incidental; drop it) and `:186` (change to the "too short" phrase), and add the new tests below.
4. Update docs and `CHANGELOG.md`.

## Files to Modify
- `rust/fontaine-tui/src/qr.rs`
- `rust/fontaine-tui/src/ticket_prompt.rs`
- `docs/remote.md`, `docs/tui.md` (fallback wording)
- `CHANGELOG.md`

## Trade-offs
- A message with figures was chosen over fitting the code into fewer rows: the module count fixes the rows at `side / 2`, and shrinking it risks scans.
- Dropping the "Valid for…" and "Waiting…" lines to gain 3 rows was considered and rejected: they are the only context beside the code and 3 rows seldom decide the fit.

## Documentation
`docs/tui.md` (the `/remote-control` row) and `docs/remote.md` (QR section): say the dialog reports the rows and columns it needs when the code does not fit.

## Testing Strategy
Unit tests in `ticket_prompt.rs` (long ticket, 639 chars, as in the existing tests; skipped under `NO_COLOR` like them):
- wide but short (width 190, height 20): the reason names rows and the needed count, and does not blame the width. This pins the reported bug.
- narrow but tall (width 40, height 80): the reason names columns.
- both short: names both.
- fits: no reason, as today.
- an oversized payload that does not encode (`sample_payload` far beyond QR capacity): the "too long for a QR code" reason.
- A unit test in `qr.rs` that `size()` agrees with the `render` boundary (fits at exactly `size`, `None` one row or column below).

## Open Questions
None.
