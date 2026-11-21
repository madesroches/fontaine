# Blank Screen After Pairing Dialogs Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/740

## Overview
After a device pairs through `/remote-control`, the chat is partly blank: transcript rows were
committed to scrollback while the tall ticket (QR) or pairing dialog was open, and closing the
dialog does not rewrite the transcript, so a gap sits above the input. Fix by making the
"rows were committed under an overlay" recovery apply to every overlay, in one place.

## Current State
- `Screen::draw_frame` (`rust/fontaine-tui/src/screen.rs` ~l.497) calls `commit_within` to fit the
  live area, and sets `app.committed_under_overlay` only if `app.agents` or `app.transcript_view`
  is open.
- Only `App::close_transcript` and the agents-panel `Close`/`View` arms (`app.rs` ~l.1512, ~l.1777)
  consume the flag and `view(...)` (reprint) the transcript.
- The ticket dialog closes at `app.rs:1581` (`TicketKey::Dismiss`), `ui/remote.rs:513`, and is
  replaced by a pairing prompt; the pairing prompt is cleared at `app.rs:1620`, `ui/remote.rs:493,
  683, 698`. None reprint. Other overlays covered by `App::covered` (approval, questions,
  elicitations, denied, ...) have the same latent gap.

## Design
Centralise rather than patch each close path (many, and new overlays would regress):
1. In `draw_frame`, set `committed_under_overlay` whenever rows are released while
   `app.covered()` or `app.agents`/`app.transcript_view` is open (extract an `overlay_open(app)`
   helper in `app.rs` covering all of these).
2. `Screen` remembers `overlay_was_open` from the previous frame. At the top of `draw_frame`,
   when it was open, no overlay is open now, and `committed_under_overlay` is set, call
   `app.view(app.viewing.clone())` (which sets `reprint` and clears the flag) before the existing
   `reprint` check. Update `overlay_was_open` each frame.
3. The existing agents-panel/transcript close paths stay as they are (they reprint immediately and
   clear the flag, so the central path is a no-op for them).

A dialog replaced by another (ticket → pairing) never reaches "no overlay open", so the rewrite
happens once, when the last one closes.

## Implementation Steps
1. `app.rs`: add `pub(crate) fn overlay_open(&self) -> bool` (covered || agents || transcript_view).
2. `screen.rs`: add `overlay_was_open: bool` to `Screen`; use `overlay_open` for the flag at
   ~l.499; add the transition check at the start of `draw_frame`.
3. Tests in `screen.rs` beside the agents-panel ones (~l.2103).
4. Reproduce in tmux per `tasks/design/testing.md` before and after, if feasible with the
   `live_area` example; otherwise rely on the vt100 harness tests.

## Files to Modify
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/screen.rs`
- `CHANGELOG.md`

## Trade-offs
- Per-close-path reprint (the issue's suggestion) touches ~7 sites and misses future overlays; the
  frame-transition check is one site. Cost: one bool on `Screen`.

## Testing Strategy
Unit tests with the vt100 harness (issue-reported bug, so pinned by regression tests):
- Ticket dialog open with a long transcript, forcing rows into scrollback, then dismissed: the
  last message is on the visible rows and each line appears exactly once overall.
- Same for a pairing prompt replacing the ticket dialog, then answered.
- A dialog that committed nothing does not set `reprint`.
- Existing agents-panel and transcript-view tests stay green.

## Documentation
None (internal behavior); add a CHANGELOG entry.

## Open Questions
- Whether the unrelated `job_cancel_placeholder` provider error in the issue deserves its own
  issue: out of scope here.
