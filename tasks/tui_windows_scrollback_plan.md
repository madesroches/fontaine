# TUI Windows Scrollback Loss Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/543

## Overview
On Windows, user messages never reach reachable scrollback, and switching agents leaves scrollback
empty or unusable (issue #543). Linux/tmux is healthy. The issue has no root cause yet, so this
plan is diagnosis-first: capture the exact bytes the TUI emits for a user-message commit and for an
agent switch, find which sequence Windows' console host (ConPTY / Windows Terminal) mishandles,
then change the emission for that case and pin it with a regression test.

## Current State
`fontaine-tui/src/screen.rs` keeps an inline viewport and writes settled transcript rows into the
terminal's own scrollback via ratatui `insert_before` (`draw_frame`, ~l.430-510).

- **User message rows** are `"> text"` (bold) followed by a blank `Line::default()`
  (`transcript.rs` `Entry::User`, ~l.1094). Assistant rows have no trailing blank row.
- **Agent switch** (`App::view`, `app.rs:509`) sets `reprint = true`. `Screen::reprint`
  (`screen.rs:525`) calls `purge()` (`ESC[2J` then `ESC[3J`, `screen.rs:189`), rebuilds the
  viewport at row 0 with the full screen height, and the whole transcript is recommitted
  (`Transcript::recommit`, `transcript.rs:246`).
- **Top-of-screen commits**: when the viewport's top is row <= 1 (always true right after a
  reprint, and whenever the transcript fills the screen), `draw_frame` takes the
  `one_row_region` path: ratatui's scroll region would be a one-row invalid DECSTBM, so the whole
  screen scrolls, blank cells of the inserted rows are flagged `Modifier::HIDDEN` (SGR 8) so they
  are written as spaces, and the viewport is rebuilt and repainted.
- Tests use `TestBackend` and a vt100 harness in `screen.rs`; neither models ConPTY.
  `tasks/design/testing.md` "Reproducing terminal layout bugs" documents the tmux repro; there is
  no Windows equivalent.

## Hypotheses (ranked; step 1 decides)
Both symptoms share one path: committing rows with the viewport at the top of the screen
(`one_row_region`). The agent switch is exactly that path for every row of the transcript, and a
fast-typing user's message is committed that way once the screen is full.

1. **`one_row_region` full-screen scroll + `HIDDEN` blanks** is mishandled by ConPTY: the user
   message row (adjacent to a blank HIDDEN row) is overwritten or not pushed to scrollback, while
   non-blank-adjacent assistant rows survive.
2. **`ESC[2J` + `ESC[3J` ordering** in `purge` on Windows Terminal/ConPTY discards the scrollback
   written afterwards, or ConPTY replays the clear lazily (after the recommitted rows).
3. **Cursor-position/`Anchored` assumption**: on Windows the rebuilt terminal's armed anchor
   differs from the real cursor, so inserted rows land on the live area and are repainted over.

## Design
1. **Reproduce and capture (no code change).** Build `cargo build -p fontaine-tui --example
   live_area` and run it in Windows Terminal at a small size; compare scrollback against the
   tmux result. Capture the bytes of one frame (`ESC[?2026h` ... `ESC[?2026l`) for (a) a
   user-message commit with the viewport at the top and (b) a `reprint`. Extend the example
   (it is test tooling, no product behavior change) with user-message and reprint scenarios; it
   currently drives only a notice, tool calls and assistant deltas.
2. **Bisect the cause** by toggling one variable at a time in the example: drop `HIDDEN`
   (use plain spaces), emit `ESC[3J` before `ESC[2J`, split the purge into its own frame, avoid the
   full-screen scroll. The first toggle that restores scrollback on Windows names the cause.
3. **Fix at the emission point**, narrowly, in `screen.rs` (`one_row_region` handling and/or
   `Framed::purge`), keeping Linux/tmux output unchanged unless the new form is equally correct
   there. A Windows-only branch is acceptable only if no portable form works; record it in the
   master plan's `## Decisions`, at the end of its TUI subsection.
4. **Pin it.** Add a vt100-harness test reproducing the offending byte pattern that the harness can
   express (the harness already rejects `ESC[2J` without a following `ESC[3J`; extend its
   rejection set with whatever pattern step 2 identifies, e.g. a concealed attribute on
   inserted rows). Extend the existing
   `viewing_an_agent_erases_the_scrollback_and_writes_only_that_transcript` with an
   every-user-row-once-in-order assertion on `scrollback()`.

## Implementation Steps
1. Reproduce on Windows Terminal; write findings (bytes, which hypothesis) into this plan's
   `## Decisions`. Only the final dated decision (a deviation) goes in
   `tasks/fontaine_harness_plan.md`'s `## Decisions`, at the end of its TUI subsection.
2. Bisect per Design step 2; choose the fix.
3. Implement in `rust/fontaine-tui/src/screen.rs` (and `transcript.rs` only if the cause is in
   row generation).
4. Add the regression tests (Testing Strategy).
5. Extend `tasks/design/testing.md` "Reproducing terminal layout bugs" with the Windows steps.
6. `CHANGELOG.md` entry; run the Essential Commands, including the Windows clippy target check.

## Files to Modify
- `rust/fontaine-tui/src/screen.rs` (fix, harness checks, tests)
- `rust/fontaine-tui/examples/live_area.rs` (user-message and reprint scenarios)
- `rust/fontaine-tui/src/transcript.rs` (only if the cause is row generation)
- `tasks/design/testing.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (`## Decisions`)

## Trade-offs
- **Diagnosis-first vs. fix-by-guess**: the issue records no confirmed cause and `TestBackend`
  cannot show it; a speculative change risks regressing the Linux scrollback that works.
- **Windows-only branch vs. portable fix**: prefer one emission for all platforms to keep the
  vt100 harness and tmux checks meaningful.

## Decisions
- AFK: conhost: validate the fix on both Windows Terminal and legacy conhost when both are reachable; otherwise validate on Windows Terminal and note conhost as unverified.
- 2026-11-09 byte-level findings (no real ConPTY reachable, so no Windows result; no fix applied):
  captured through the vt100 harness's `writes_of` at 30x8 with the viewport at the top.
  - Reprint frame: `ESC[?2026h ESC[2J ESC[3J ESC[1;1H ESC[1;1H` + 7 `LF` (rebuilt viewport), then for
    each settled row: `ESC[1;1H <styled text> ESC[8m <spaces> ESC[39m..ESC[0m ESC[1;1r ESC[1S ESC[r`,
    then the live rows repainted and `ESC[?2026l`. Purge order is `2J` then `3J` as designed, and
    both precede every inserted row in the same synchronized update.
  - User-message commit at the top: identical per-row form. `> text` is one such row and its
    trailing blank is a second row made only of `ESC[8m` plus spaces, so nothing in the bytes
    distinguishes it from other rows except the all-HIDDEN blank row adjacent to it.
  - Every settled row reaches history through `CSI 1 S` (SU) under an invalid `CSI 1;1 r`, never
    through a line feed. In the harness (vt100) and tmux these rows land in scrollback once and in
    order (new test `a_user_message_committed_at_the_top_of_the_screen_keeps_its_row_and_blank`,
    and the extended agent-switch test asserts every user row exactly once, in order).
  - Hypotheses 1 and 2 are not refuted or confirmed by the bytes; the unusual form (SU with a
    degenerate margin, SGR 8 spaces) is the candidate to bisect on Windows. No defect in the
    bytes themselves justified a change, so none was made. Bisecting steps remain for a run on
    Windows Terminal / conhost (see `tasks/design/testing.md`, `live_area user`).
- 2026-11-09 first fix attempt, reverted: `Anchored::scroll_region_up` turned the one-row region's
  `CSI 1;1 r` + `CSI n S` (whole-screen SU) into cursor-to-bottom + n line feeds, on the theory
  that SU does not feed scrollback in ConPTY. The `live_area user` example passed on Windows, but
  `cargo run -p fontaine` was just as broken (an installed `~/.cargo/bin/fontaine.exe` from before
  the change is not the cause: the user ran `cargo run`).
- 2026-11-09 ConPTY harness (a real ConPTY, no Windows Terminal window): pywinpty runs the binary
  at 60x12, a mock OpenAI-compatible SSE endpoint answers, and `pyte` rebuilds scrollback from the
  bytes ConPTY emits (the stream Windows Terminal receives). Config comes from
  `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR`. Eight messages are typed into the real app.
  - `live_area user` (fixed build): every `question N` once, in order.
  - Real app, original code: every `question N` once, but the assistant answer rows never reach
    scrollback (only the screen).
  - Real app, line-feed attempt: worse. Each question three times, and `Working…` and prompt-box
    rows leak into history, because a full-screen line feed also scrolls the live area up.
    Hence the revert. ConPTY re-renders the stream itself (cursor to row 2, a run of line feeds,
    then a full repaint), so source-level reasoning about it is unreliable; bisect by running.
  - The emulation is a proxy: it shows answers missing where the user reports user messages
    missing, so the harness may still differ from Windows Terminal's own scrollback.
  - Bisect in progress, behind a temporary `FONTAINE_BISECT` switch in `draw_frame`
    (`off`: skip the one-row path, `nohide`: plain-space blanks, `cap2`/`cap3`: viewport capped at
    height-2/-3 so the region is valid). Not part of the product; remove before the fix lands.
    Results (8 questions, 16 answer rows wanted, scrollback rebuilt from the ConPTY stream):

    | Variant | `question N` rows (once each wanted) | answer rows |
    |---|---|---|
    | base | 1 each | 2 |
    | `off` | 1 each | 9 |
    | `nohide` | 2 each (duplicated) | 9 |
    | `cap2` | 0 (all lost) | 2 |
    | `cap3` | 0 (all lost) | 3 |

    - A valid region (`cap2`/`cap3`) loses every user message: SU inside a valid DECSTBM region
      does not reach ConPTY's scrollback. This reproduces the reported symptom and agrees with the
      SU theory for the region case.
    - The one-row path keeps the user messages but drops most answer rows; `nohide` duplicates the
      user rows, so the concealed blanks do keep the rows from being drawn twice.
    - No variant restores everything. Open: why answers, committed through the same
      `insert_before`, fare worse than user messages in the one-row path (answers are committed
      when the live area overflows, user rows when retired; compare the frames).
- 2026-11-09 root cause. The `pyte` harness above is invalid: `pyte` implements neither `CSI S` nor
  `CSI T`, so it ignored every scroll ratatui emits, and the table above is noise. ConPTY 1.25
  (bundled by pywinpty, newer than WT 1.24's) forwards the program's bytes nearly verbatim, so the
  terminal's VT interpreter decides. A classic conhost window (same `AdaptDispatch` as Windows
  Terminal), read back with `ReadConsoleOutputCharacterW`, is the oracle: for a row scrolled off
  the top, LF at the bottom with no margins → scrollback; LF at the bottom of margins `1;N` →
  scrollback (the rest of the screen stays); `CSI S` with no margins, with `1;1` (invalid) or with
  `1;N` → **dropped**. Every settled row left the screen through SU, so on Windows it reached
  scrollback only when an unrelated line feed (e.g. a rebuild's `append_lines`) happened to push it.
  The first attempt only replaced the one-row case, which is why the real app stayed broken.
  - Fix: `Anchored::scroll_region_up` (the only path ratatui uses to scroll rows off, always
    `0..viewport_top`) sets margins `1;top`, puts the cursor on their last row and line-feeds `n`
    times, or for a one-row region line-feeds at the screen's bottom (a whole-screen scroll, which
    is what terminals already did with the ignored `1;1` margin). This is the form Codex CLI's
    `insert_history` uses. In tmux an LF at a region's bottom and an SU go through the same
    `grid_view_scroll_region_up`, so Linux output is unchanged in effect. Portable, no
    Windows-only branch, so no master-plan Decision.
  - Conhost run of the real app (60x12, mock provider, 6 questions): before, 5 of 6 questions and 10
    of 12 answer rows missing; after, every question once, all answer rows, no live-area rows in
    history. After a resize (the same reprint path as an agent switch): before, 4 of 5 questions
    lost; after, every row once, reflowed.
  - Pinned: the vt100 harness's sink panics on any `CSI n S`, which fails 11 existing harness tests
    with the old behavior, and
    `settled_rows_leave_the_screen_by_line_feeds_at_the_bottom_of_top_margins` checks both
    branches directly.

## Documentation
`tasks/design/testing.md` gains a Windows repro section; no `docs/` page describes this behavior.

## Testing Strategy
- vt100-harness regression test pinning the byte pattern that Windows mishandles (names issue
  #543); it must fail before the fix.
- Unit tests: extend `viewing_an_agent_erases_the_scrollback_and_writes_only_that_transcript` to
  assert every user row appears exactly once in `scrollback()`, in order; add a test for a user
  message committed at the top of the screen only if the existing commit tests (`screen.rs`
  ~l.684/697) do not cover it.
- Existing tmux-oriented harness tests stay green, proving Linux output is unchanged.
- The real ConPTY behavior cannot be reached by an automated test in CI (no Windows pty harness
  in the suite); a silent regression there is visible immediately in use, so it stays manual.

## Manual Verification
On Windows Terminal (80x12 and a full-size window):
1. `cargo build -p fontaine-tui --example live_area`, then run `target\debug\examples\live_area.exe`
   and, separately, `live_area.exe user`; scroll the terminal's scrollback up. Expected: with the
   default scenario every `note N` / `wordN` appears once; with `user` every `question N` appears
   once, in order, followed by a blank row. (Not automated: needs a real ConPTY.)

Steps 2-3 use `cargo run -p fontaine` with a configured provider.

2. In a real session send several messages until the first scrolls off, scroll back. Expected: all
   user messages visible.
3. Spawn a subagent, switch to it and back (agents panel), scroll back. Expected: the full
   transcript is in scrollback once.
