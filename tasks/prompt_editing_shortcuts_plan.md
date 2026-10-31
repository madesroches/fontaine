# Prompt Editing Shortcuts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/257

## Overview

The TUI message box handles arrows, Home/End, Backspace/Delete and Ctrl-A/E/U/K/W/B/F only.
Ctrl-Left/Right and the other usual line-editing keys do nothing. This plan adds word movement
and word deletion, whole-buffer jumps, a one-entry kill buffer with Ctrl-Y, and the readline
aliases Ctrl-H, Ctrl-D (on a non-empty input), Ctrl-P/N and Ctrl-L. Word shortcuts use alphanumeric
words, except Ctrl-W, which keeps its whitespace boundary. Every `InputBuffer` gets the editing keys: the message box, the
deny-reason field and the question panel's "Other" field.

## Current State

- `rust/fontaine-tui/src/input.rs` — `InputBuffer` (a `Vec<char>` plus a char cursor).
  `handle_key` (line 166) matches `Char(c) if control` for a/e/u/k/w/b/f and returns `false` for
  any other control letter. `Char(_) if alt` returns `false`. The arrow arms ignore modifiers, so
  Ctrl-Left lands on the plain `Left` arm and moves one char, and Ctrl-Home moves to the start
  of the line.
- `delete_word` (line 142) defines a word by whitespace: it skips the whitespace before the
  cursor, then the non-whitespace before that. `'\n'` counts as whitespace.
- `kill_to_start`, `kill_to_end` and `delete_word` throw away what they remove. Nothing records
  it.
- `rust/fontaine-tui/src/app.rs`:
  - `App::handle_key` (line 685) runs overlays, approval, history search, completion and the
    slash menu, then reaches `message_key` (line 1156). That function handles Enter, Ctrl-J,
    Ctrl-R, Up/Down at the buffer edges (history, queued messages, agent-list focus) and passes
    every other key to `InputBuffer::handle_key`.
  - Ctrl-D (line 916) returns `Quit` when the app is idle and the input is empty, and `None`
    otherwise. With text in the box it does nothing.
  - Ctrl-T opens the agents panel and Ctrl-O toggles the task list (lines 861–869).
  - `App::reprint` (line 148): the next render erases the screen and its scrollback and writes
    the shown transcript again. `/clear` and the agents-panel close already set it. The linear
    (screen-reader) renderer ignores it (`linear.rs:394`).
- `approval.rs:170` and `question.rs:128` pass keys to their own `InputBuffer::handle_key`.
- Scrollback belongs to the terminal (`screen.rs` writes settled lines into it once), so the
  transcript has no PageUp/PageDown scrolling to add.
- crossterm 0.29 and the absence of `PushKeyboardEnhancementFlags` mean legacy key encoding on
  Unix. In crossterm's parser (`event/sys/unix/parse.rs`), `0x08` decodes as `Char('h')` +
  CONTROL and `0x7F` as `Backspace`. Most Unix terminals send `0x08` for Ctrl-Backspace, so it
  reaches the app as Ctrl-H. Ctrl/Alt + arrows, Home and End (`ESC[1;5D` and so on) decode with
  their modifiers. Alt+key (`ESC` + key) decodes as the key + ALT. The Windows backend reports
  Ctrl-Backspace as `Backspace` + CONTROL.

## Design

### Word definition

As in Claude Code, a word is a run of letters and digits (`char::is_alphanumeric`), so `_`, `.`
and `/` separate words. This holds for Ctrl/Alt-Left/Right, Alt-B/F/D, Alt/Ctrl-Backspace and
Ctrl-Delete. Ctrl-W keeps today's boundary: a run of non-whitespace chars. Movement and deletion
share one helper pair, parameterised by the boundary class:

```rust
/// Start of the word before `pos`: skip separators back, then word chars back.
fn word_start_before(&self, pos: usize, is_word: fn(char) -> bool) -> usize
/// End of the word after `pos`: skip separators forward, then word chars forward.
fn word_end_after(&self, pos: usize, is_word: fn(char) -> bool) -> usize
```

The alphanumeric shortcuts pass `char::is_alphanumeric`; Ctrl-W passes `|c| !c.is_whitespace()`.
`delete_word` (alphanumeric) is `kill(word_start_before(cursor, alnum)..cursor)`, and a separate
`delete_word_whitespace` backs Ctrl-W. Newlines are separators in both classes, so word movement
crosses lines.

### Kill buffer

`InputBuffer` gains `killed: String`. Every kill goes through one private helper:

```rust
/// Removes `range`, leaves the cursor at its start, and keeps the text for Ctrl-Y.
fn kill(&mut self, range: Range<usize>)
```

The helper only replaces `killed` when the range is non-empty, so a Ctrl-K at the end of the
buffer does not wipe what an earlier kill saved. It backs `kill_to_start`, `kill_to_end`,
`delete_word`, `delete_word_whitespace` and the new `delete_word_forward`. Char-level Backspace/Delete do not touch the
kill buffer, as in readline. `yank()` inserts `killed` at the cursor and leaves the cursor after
it. `take()` and `set_text` leave `killed` alone, so a kill carries over a submit, as in a shell.
There is one entry, consecutive kills replace it rather than append, and there is no Alt-Y ring (see Decisions).

### New `InputBuffer` operations

`word_left`, `word_right`, `delete_word_forward`, `delete_word_whitespace`, `buffer_start`, `buffer_end`, `yank`.

### `InputBuffer::handle_key` mapping

The modifier arms go before the plain ones. A key with both CONTROL and ALT takes its Ctrl
meaning, as the existing control arm (first in the match) already gives it (crossterm parses
ESC+key by re-parsing and adding ALT, so on a terminal whose Backspace is 0x08, Alt-Backspace
arrives as Ctrl-H+ALT and deletes one char).

| Key | Action |
|---|---|
| Ctrl-Left, Alt-Left, Alt-B | `word_left` |
| Ctrl-Right, Alt-Right, Alt-F | `word_right` |
| Alt-D, Ctrl-Delete | `delete_word_forward` |
| Alt-Backspace, Ctrl-Backspace | `delete_word` |
| Ctrl-W | `delete_word_whitespace` |
| Ctrl-Home / Ctrl-End | `buffer_start` / `buffer_end` |
| Ctrl-H | `backspace` |
| Ctrl-D | `delete` |
| Ctrl-Y | `yank` |

Alt letters are lowercased the way the control letters already are. Any other Alt+char still
returns `false`.

### App-level keys (`app.rs`)

- **Ctrl-D**: the quit branch only applies on an empty input: idle → `Quit`, running → `None`, as
  today. A non-empty input falls through to `message_key`, which deletes the char under the
  cursor.
- **Ctrl-P / Ctrl-N**: in `App::handle_key`, right after the questions branch (app.rs:854-857)
  and before the agent-list focus check, rewrite Ctrl-P to a plain `Up` and Ctrl-N to a plain
  `Down`, guarded by `!self.covered() && self.search.is_none()`, and recompute `control` from the
  rewritten key. A focused agent list then sees them as Up/Down, and everything after (completion
  and slash-menu selection, history recall on the edge lines, queued-message editing, agent-list
  focus) treats them as Up/Down. A panel or dialog never sees the rewrite.
- **Ctrl-L**: next to the Ctrl-T/Ctrl-O arms, and under the same `!typing_reason` guard, set
  `self.reprint = true` and return `Action::None`. This is the existing full redraw, which
  matches Claude Code's "clear the screen, keep the conversation". The linear renderer already
  ignores `reprint`, so a screen reader's history stays intact.

### Left out

- **Ctrl-T transpose**: Ctrl-T opens the agents panel.
- **Ctrl-_ undo** (Claude Code's undo key): low priority in the issue. It needs its own history design.
- **PageUp/PageDown**: the terminal owns the transcript scrollback.

## Implementation Steps

1. `input.rs`: add `killed`, `kill`, `word_start_before` and `word_end_after`. Reimplement
   `kill_to_start`, `kill_to_end` and `delete_word` on `kill`; `delete_word` moves from the
   whitespace boundary to the alphanumeric one. Add `word_left`, `word_right`,
   `delete_word_forward`, `delete_word_whitespace`, `buffer_start`, `buffer_end` and `yank`.
2. `input.rs`: extend `handle_key` with the mapping table above, with the modifier arms ahead of
   the plain arrow, Home, End, Backspace and Delete arms.
3. `app.rs`: narrow the Ctrl-D branch to an empty input. Add the Ctrl-P/N rewrite (after the
   questions branch, before the agent-list focus check) and the Ctrl-L reprint.
4. Tests (see Testing Strategy).
5. Docs (see Documentation). Add a `CHANGELOG.md` entry.
6. `tasks/fontaine_harness_plan.md`: add the two `## Decisions` lines below (kill buffer, no
   undo) and the Ctrl-W whitespace line at the end of the `### TUI` subsection.

## Files to Modify

- `rust/fontaine-tui/src/input.rs`
- `rust/fontaine-tui/src/app.rs`
- `docs/tui.md`
- `docs/terminal.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (`### TUI` Decisions lines only)

## Trade-offs

- **Ctrl-H as Backspace**: this follows readline and the issue. On most Unix terminals
  Ctrl-Backspace arrives as Ctrl-H (`0x08`), so there it deletes one char. Treating Ctrl-H as a
  word delete would break the readline meaning. Alt-Backspace and Ctrl-W give word deletion
  everywhere.
- **Ctrl-P/N rewritten to Up/Down at the app level rather than in `InputBuffer`**: history,
  queued messages and the menus live in `App`. Rewriting the key keeps a single code path for
  Up/Down instead of duplicating its guards.

## Decisions

- 2026-10-31: Ctrl-W deletes back to the previous whitespace while the other word shortcuts use
  alphanumeric words, matching Claude Code.
- 2026-10-31: the kill buffer holds a single entry with no Alt-Y paste cycle (Claude Code has
  one); one entry covers the common case.
- 2026-10-31: AFK: consecutive kills do not join; each non-empty kill replaces the single kill-buffer entry, so Ctrl-W twice then Ctrl-Y yanks only the last word (readline joins, Claude Code's docs do not say; #257 only needs the last killed text).
- 2026-10-31: no Ctrl-_ undo (Claude Code's undo key); it needs its own history design.

## Documentation

- `docs/tui.md`: replace the "Arrows, Home, End, Backspace, Delete, Ctrl-A/E/U/K/W/B/F" row
  (line 114) with rows for char editing, word movement and word deletion, buffer start/end, and
  kill/yank. Change the Ctrl-D row (line 112) to say it deletes the char under the cursor when
  the input has text. Add Ctrl-P/N to the `Up, Down` row (line 104) and the History section, and Ctrl-L as a redraw
  row.
- `docs/terminal.md`: widen the `## Option as Meta on macOS` sentence to cover Alt-B/F/D, and add
  Ctrl/Alt-arrows to the `## tmux` "need no setting" line. Add a short "Word movement and
  deletion" section with only the note that Ctrl-Backspace reaches Fontaine as Ctrl-H on most Unix
  terminals and so deletes one char, with Alt-Backspace or Ctrl-W as the word delete, linking to
  the macOS section.

## Testing Strategy

Unit tests in `input.rs`, next to the existing ones:

- `word_left` / `word_right` across several spaces, across a newline, and at both buffer ends.
- `delete_word_forward` and `delete_word` use the same boundaries (one test drives both
  directions over the same text).
- On the text `src/main.rs`, word movement and `delete_word` go by segment, while Ctrl-W
  removes the whole path.
- Kill then yank, for each kill kind. An empty kill keeps the previous entry. `take()` keeps the
  kill buffer. A yank with nothing killed changes nothing.
- `buffer_start` / `buffer_end` on a multi-line buffer, next to Home/End staying per line.
- A `handle_key` table test with a `KeyEvent` and the expected text and cursor for every row of
  the mapping, plus an unknown Alt+char returning `false`.

Tests in `app.rs`:

- Ctrl-D with text deletes the char under the cursor and does not quit. Ctrl-D on an empty idle
  input still returns `Quit`. Ctrl-D on an empty input while running still returns `None`.
- Ctrl-P / Ctrl-N recall older and newer history the same way Up/Down do (reusing the
  `up_and_down_walk_the_history_on_the_edge_lines_only` setup).
- Ctrl-N / Ctrl-P move the selection of a focused agent list like Down/Up.
- Ctrl-L sets `reprint`.
- Ctrl-Left in the message box moves the cursor by word, through the full `handle_input` path.

## Manual Verification

Real terminals encode modified keys in their own ways, and only a real terminal shows how its
bytes decode:

1. `cd rust && cargo run -p fontaine`. Type `one two three`. Ctrl-Left twice puts the cursor
   before `two`, and Alt-F puts it after `two`.
2. In tmux, run the same check. Ctrl-Left/Right and Alt-Left/Right move by word.
