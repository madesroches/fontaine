# Blank Screen After Pairing Dialogs Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/740

## Overview
After a device pairs through `/remote-control`, the chat is partly blank: transcript rows were
committed to scrollback while the tall ticket (QR) or pairing dialog was open, and closing the
dialog does not rewrite the transcript, so a gap sits above the input. Fix by making the
"rows were committed under an overlay" recovery apply to every one-off dialog (not the per-tool-call
approval, question and elicitation dialogs), in one place.

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
  elicitations, denied, ...) have the same latent gap; the per-call ones (approval, questions,
  elicitations) are left alone, see Design.

## Design
Centralise rather than patch each close path (many, and new overlays would regress):
1. In `draw_frame`, set `committed_under_overlay` whenever rows are released while
   `app.overlay_open()`. The helper is `(self.covered() && !self.approval.is_some() &&
   self.questions.is_empty() && self.elicitations.is_empty()) || self.plugin_prompt.is_some() ||
   self.setup_composition.is_some()`: `covered()` (app.rs:1518-1543) already includes agents and
   transcript_view; the per-tool-call dialogs (approval, questions, elicitations) are excluded so a
   non-auto session does not purge and reprint on nearly every close; `plugin_prompt` and `setup_composition` are dialogs drawn by `view::overlay_in`
   (view.rs:476,492) that `covered()` omits, and they count in `view::chrome`. The helper exists so
   the flag and the transition check share one definition of "a dialog covers the live area".
2. `Screen` remembers `overlay_was_open` from the previous frame. At the top of `draw_frame`,
   when it was open, no overlay is open now, and `committed_under_overlay` is set, set
   `app.reprint = true; app.committed_under_overlay = false;` directly, before the existing
   `reprint` check below, which recommits and reprints. Update `overlay_was_open` at the top of `draw_frame`, before any `?` return, so an early error return cannot leave it stale.
3. The existing agents-panel/transcript close paths stay as they are (they reprint immediately and
   clear the flag, so the central path is a no-op for them).
4. Reword the stale doc comment on `committed_under_overlay` (app.rs:281-282) to "while an overlay
   covered the live area".

## Implementation Steps
1. `app.rs`: add `pub(crate) fn overlay_open(&self) -> bool` (as defined in Design step 1); reword the `committed_under_overlay` doc comment.
2. `screen.rs`: add `overlay_was_open: bool` to `Screen`; use `overlay_open` for the flag at
   ~l.499; add the transition check at the start of `draw_frame`.
3. Tests in `screen.rs` beside the agents-panel ones (~l.2103).
4. Add a `pairing` mode to `rust/fontaine-tui/examples/live_area.rs` (and a sentence about it in
   the module doc comment, lines 1-7, which lists the modes): print notices, open the
   ticket dialog via `begin_ticket`, then a pairing prompt via `begin_pairing`, print more notices
   so rows commit under it, dismiss. Verify in real tmux per `tasks/design/testing.md`
   "Reproducing terminal layout bugs", before and after the fix: each line appears once with no
   blank run above the input.
5. Docs: `docs/tui.md`: add closing any dialog that pushed rows into scrollback to the list of what
   erases it (~l.20-25: it reprints the transcript, which erases the scrollback first), and widen
   ~l.1234-1236, which says only closing a tall transcript view does. `docs/remote.md` (l.26-27, where
   the ticket dialog closing is described): one sentence that closing the dialog redraws the transcript.
   `CHANGELOG.md`: entry under `## [Unreleased]` / `### Fixed`, ending "(#740)".

## Files to Modify
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/screen.rs`
- `rust/fontaine-tui/examples/live_area.rs`
- `docs/tui.md`
- `docs/remote.md`
- `CHANGELOG.md`

## Trade-offs
- Per-close-path reprint (the issue's suggestion) touches ~7 sites and misses future overlays; the
  frame-transition check is one site. Cost: one bool on `Screen`, and
  closing a one-off dialog after a commit purges scrollback and reprints the full transcript, the
  cost the agents-panel and transcript-view closes already pay. The per-call dialogs are excluded
  (they would pay it on nearly every approval), so a gap can still follow one in a long session.

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
- An approval dialog opened over a long transcript and then answered does not set `reprint`.
- Existing agents-panel and transcript-view tests stay green.

## Decisions
- 2026-11-21: the transition check sets `reprint`/`committed_under_overlay` directly instead of
  calling `app.view(...)`, which also unfocuses `agent_list` while it must stay focused under
  `covered()` dialogs.
- 2026-11-21: menus (search, completion, slash) and chrome growth outside dialogs are excluded from
  `overlay_open`: the gap is small (up to ~11 rows) and a purge/reprint on every menu close costs more.
- AFK: 2026-11-21: `overlay_open` excludes the per-tool-call dialogs (approval, questions, elicitations), like menus: a purge/reprint on nearly every approval close in non-auto modes costs more than the latent gap, which the issue does not report.
