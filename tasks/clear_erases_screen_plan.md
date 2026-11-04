# `/clear` Erases the Screen Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/207

## Overview
`/clear` empties the model's context and the transcript, but the old conversation stays visible: its
rows were already in the terminal (live area or scrollback) and the command only retires held-back
rows into scrollback. A user typing `/clear` expects what Ctrl-L does, the previous conversation
gone from view. Make `/clear` erase the screen and scrollback through the existing reprint path, so
it ends on the "cleared the conversation" notice over an empty screen.

## Current State
- `Ui::clear` (`rust/fontaine-tui/src/ui/commands/context.rs`) calls `runtime.clear_conversation()`,
  then `Transcript::clear()`, which moves held-back entries to `retired` so the screen writes them
  to scrollback (`transcript.rs`, `clear` / `take_retired`). Nothing is erased.
- Ctrl-L (`TuiAction::Reprint`, `app.rs`) and `App::view` set `app.reprint = true`. The next
  `Screen::draw_frame` (`screen.rs`) then calls `recommit()` (drops `retired`, rewinds `committed`)
  and `Screen::reprint()`, which purges screen and scrollback and writes the transcript again from
  the top row.
- The screen-reader renderer (`linear.rs`) forces `app.reprint = false`, so it never erases.
- `docs/tui.md` says `/clear` writes held-back rows to scrollback and that what was already written
  "stays in your terminal's scrollback".

## Design
In `Ui::clear`'s success arm, set `self.app.reprint = true` after the transcript is emptied and the
notice pushed. The next render purges the terminal and writes only the notice. `recommit()` already
discards `retired`, so the rows `Transcript::clear` retired are not written first; no change to
`Transcript` is needed. `viewing` reset (`App::view(None)`) already sets the flag, so the explicit
set only matters for the plain-clear path.

Screen readers keep their history, as for Ctrl-L: `linear.rs` ignores the flag.

No new keybinding or command: Ctrl-L remains the visual-only clear.

## Implementation Steps
1. `rust/fontaine-tui/src/ui/commands/context.rs`: set `self.app.reprint = true` in `clear`'s `Ok`
   arm; replace the comment about not reprinting (it describes the rows retired to scrollback, which
   no longer applies) with one stating why the erase is wanted.
2. `docs/tui.md`: update the scrollback paragraph (line ~18, `/clear` no longer writes held-back
   rows to scrollback), the view-switch paragraph (add `/clear` to what erases scrollback) and the
   Context section (the transcript is emptied and the screen and scrollback erased).
3. `CHANGELOG.md`: entry under Unreleased.

## Files to Modify
- `rust/fontaine-tui/src/ui/commands/context.rs`
- `docs/tui.md`
- `CHANGELOG.md`

## Trade-offs
- Screen-only erase keeping scrollback (the issue's first suggestion): needs a second erase
  primitive in `Framed`/`Screen`, and terminals disagree on whether erasing the screen pushes rows
  into history, so the result is inconsistent. Reusing the purge matches Ctrl-L and the view switch.
- A separate visual-clear command: Ctrl-L already is one.

## Decisions
- 2026-11-03: `/clear` erases scrollback as well as the screen, like Ctrl-L; the old conversation
  is not recoverable by scrolling. Claude Code's exact behavior here is unverified (`claude -p`
  could not observe it); if it keeps scrollback, revisit.

## Documentation
`docs/tui.md` (three spots above).

## Testing Strategy
- Unit test in `fontaine-tui` for `Ui::clear` (or the nearest existing harness that drives it): after
  a successful clear, `app.reprint` is true and the transcript holds only the notice.
- Screen-level test with `TestBackend` (existing reprint tests in `screen.rs`): render a
  conversation, clear the transcript with `reprint` set, render again, and assert the backend's
  screen and scrollback contain no rows of the old conversation.
- Existing `recommit` test already pins that retired rows are dropped.

## Manual Verification
In a real tmux, run `cargo run -p fontaine`, send a message, run `/clear`, and expect an empty
screen with only the "cleared the conversation" notice and nothing above it in tmux copy mode. Not
automated because tmux's history behavior is the thing under test (see `tasks/design/testing.md`,
"Reproducing terminal layout bugs").

## Open Questions
None.
