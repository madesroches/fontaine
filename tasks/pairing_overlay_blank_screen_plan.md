# Blank Screen After Pairing Dialogs Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/740

## Overview
After a device pairs through `/remote-control`, the chat is partly blank: transcript rows were
committed to scrollback while the tall ticket (QR) or pairing dialog was open, and closing the
dialog does not rewrite the transcript, so a gap sits above the input. Fix by making the
"rows were committed under an overlay" recovery apply to every overlay, in one place.

Out of scope: the `job_cancel_placeholder` error in the issue is tracked by #717.

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
   `app.overlay_open()`. The helper is `self.covered() || self.plugin_prompt.is_some() ||
   self.setup_composition.is_some()`: `covered()` (app.rs:1518-1543) already includes agents and
   transcript_view; `plugin_prompt` and `setup_composition` are dialogs drawn by `view::overlay_in`
   (view.rs:476,492) that `covered()` omits, and they count in `view::chrome`. The helper exists so
   the flag and the transition check share one definition of "an overlay hides the live area".
2. `Screen` remembers `overlay_was_open` from the previous frame. At the top of `draw_frame`,
   when it was open, no overlay is open now, and `committed_under_overlay` is set, set
   `app.reprint = true; app.committed_under_overlay = false;` directly, before the existing
   `reprint` check below, which recommits and reprints. Update `overlay_was_open` each frame.
3. The existing agents-panel/transcript close paths stay as they are (they reprint immediately and
   clear the flag, so the central path is a no-op for them).
4. Reword the stale doc comment on `committed_under_overlay` (app.rs:281-282) to "while an overlay
   covered the live area".

A dialog replaced by another (ticket → pairing) never reaches "no overlay open", so the rewrite
happens once, when the last one closes.

## Implementation Steps
1. `app.rs`: add `pub(crate) fn overlay_open(&self) -> bool` (`covered() || plugin_prompt.is_some()
   || setup_composition.is_some()`); reword the `committed_under_overlay` doc comment.
2. `screen.rs`: add `overlay_was_open: bool` to `Screen`; use `overlay_open` for the flag at
   ~l.499; add the transition check at the start of `draw_frame`.
3. Tests in `screen.rs` beside the agents-panel ones (~l.2103).
4. Add a `pairing` mode to `rust/fontaine-tui/examples/live_area.rs`: print notices, open the
   ticket dialog via `begin_ticket`, then a pairing prompt via `begin_pairing`, print more notices
   so rows commit under it, dismiss. Verify in real tmux per `tasks/design/testing.md`
   "Reproducing terminal layout bugs", before and after the fix: each line appears once with no
   blank run above the input.

## Files to Modify
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/screen.rs`
- `rust/fontaine-tui/examples/live_area.rs`
- `CHANGELOG.md`

## Trade-offs
- Per-close-path reprint (the issue's suggestion) touches ~7 sites and misses future overlays; the
  frame-transition check is one site. Cost: one bool on `Screen`.

## Testing Strategy
Unit tests use the `screen(40,14)`/`everything()` TestBackend pattern (screen.rs:649-662, tests at
2103-2172), not vt100, asserting each `· n{i}` appears exactly once (issue-reported bug, so pinned
by regression tests):
- Ticket dialog open with a long transcript, forcing rows into scrollback, then dismissed: the
  last message is on the visible rows and each line appears exactly once overall.
- Same for a pairing prompt replacing the ticket dialog, then answered. The swap is done by hand
  between renders (`app.ticket_prompt = None; app.begin_pairing(crate::guardian_prompt::pairing(..))`;
  see app.rs:3523), since ui/remote.rs:503-513 is unreachable from screen tests.
- A dialog that committed nothing does not set `reprint`.
- Existing agents-panel and transcript-view tests stay green.

## Documentation
None (internal behavior); add a CHANGELOG entry.

## Decisions
- 2026-11-21: the transition check sets `reprint`/`committed_under_overlay` directly instead of
  calling `app.view(...)`, which also unfocuses `agent_list` while it must stay focused under
  `covered()` dialogs.
