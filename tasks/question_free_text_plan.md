# Question free-text answers Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/295

## Overview

The TUI question dialog (`ask_user_question`) already offers an "Other" row that accepts typed
text, but it reads as a hidden extra: the user must know to navigate past the last option, and
once they do, the dialog swaps to a footer field that looks like the dialog dismissed. This plan
makes free text a first-class part of the dialog: the textbox is always visible on the "Other"
row, typing a character without a binding drops straight into it, and text typed alongside a chosen option is
submitted as a note on that choice.

## Current State

- `rust/fontaine-tui/src/question.rs` — `QuestionPanel`, the dialog under the `question` overlay.
  Rows are the question's options plus a final "Other" row (`rows()` = options + 1). The dialog
  has two modes: option navigation, and a `typing` mode entered with Enter/Space on the "Other"
  row, in which keystrokes go to an `InputBuffer` and the field renders in the *footer*
  (`footer()` shows `Other: <text>▏`). Printable characters in navigation mode do nothing except
  the `j`/`k`/`1`-`9` bindings and Space (which toggles a multi-select option / opens the
  field on "Other"); the field is invisible until typing mode starts.
- `view.rs` (~line 350) renders the panel with `field: panel.field()`, drawing the typing field
  below the overlay like the other dialogs' input fields.
- `rust/fontaine-tools/src/ask_user.rs` — `QuestionAnswer` carries `selected: Vec<String>` and
  `other: Option<String>`; the doc comment states a single-select answer sets *exactly one* of
  the two. `render()` already joins both into the answer line (`"x, y, z"`), so a reply that sets
  both is displayed fine end to end.
- `docs/tui.md` "Questions from the model" documents the current keys; the footer hint strings
  live in `QuestionPanel::footer()`.
- Multi-select already combines toggled options with "Other" text (`commit_other`), so the
  note-on-choice mechanic exists for one of the two question kinds.

Issue #295 (filed after the "Other" row shipped in #177) asks that a free-form answer always be
reachable by typing, and its comment asks that a note be attachable to a chosen option.

## Design

The dialog loses its separate typing mode. The highlight row *is* the mode: when the highlight
sits on the "Other" row, printable keys edit the field; anywhere else they keep their current
bindings.

### `QuestionPanel` changes (`rust/fontaine-tui/src/question.rs`)

- **Always-visible inline field.** The "Other" row renders as `4. Other: <text>` with the cursor
  block shown whenever the highlight is on that row. The footer no longer renders the field;
  `field()` returns `None` and the overlay stops drawing a separate input box.
- **Type to answer.** A printable character pressed while the highlight is on an option row moves
  the highlight to the "Other" row and inserts the character, so typing a word immediately
  becomes a custom answer (the issue's "typing to override the selection"). Bound keys keep
  their meaning in navigation mode: `1`-`9` jump to a row, `j`/`k` move, Space toggles a
  multi-select option. While the highlight is on the "Other" row, *all* printable characters —
  including digits, `j`, `k` and Space — insert into the field; arrows and Tab still move the
  highlight off it. On the "Other" row, Backspace/Delete/Home/End and Ctrl/Alt-modified keys
  keep their `InputBuffer` meaning, as the typing mode gives them today; only plain arrows and
  Tab move the highlight.
- **Note on a choice.** Leaving the "Other" row (arrows/Tab) keeps the text. Enter on an option
  row submits the selected label(s) plus any text in the field as a note. Enter on the "Other"
  row submits the text alone for single-select; for multi-select it keeps combining the
  toggled options with the text, as `commit_other` does today. An answer with neither a
  selection nor text is still ignored.
- **Esc.** On the "Other" row, Esc moves the highlight back to the first option row (text kept);
  anywhere else Esc declines the call as today.
- **Grace period unchanged.** Enter/Esc/Char stay blocked for `ANSWER_GRACE` after the dialog
  opens, so keystrokes typed ahead during a turn cannot answer; arrows still navigate.
- The `typing` flag, `is_typing()`, and `commit_other` go away. `is_typing()` has no production
  call sites — `view.rs` consumes only the already-`Option` `field()` — and its only call site is
  an assertion in the `app.rs` test (~line 1850), which step 5 reworks.

### Answer model (`rust/fontaine-tools/src/ask_user.rs`)

- Relax the `QuestionAnswer` doc comment: a single-select answer may set *both* `selected` (one
  label) and `other` (a note); "at least one" replaces "exactly one". No type, wire, or
  `render()` change — both were already joined into the result line.

### Rendering details

- The "Other" row already participates in `body()`'s windowing and wrapping; the field text
  wraps/clips like any row content. Model-written text stays run through `clean()`; the user's
  own typed text renders as today.
- `footer()` hints update: navigation mode mentions that typing starts a custom answer; the
  typing-mode footer disappears with the mode.

## Mockups

- `tasks/question_free_text_mockups/inline-other-field.html` — the dialog with the empty inline
  field on the "Other" row, and mid-typing with the highlight on that row. Single option; the
  trade-off it records is visibility (the field is always on screen) versus the row taking one
  more line even when unused.

## Implementation Steps

1. `question.rs`: rework `handle_key_at` — remove the typing-mode branch; printable chars on an
   option row jump to the "Other" row and insert; on the "Other" row they all insert; digits/`j`/
   `k`/Space keep navigation/toggle meaning only on option rows.
2. `question.rs`: rework `confirm()`/`finish()` — Enter on an option row includes `other_text()`
   when non-empty; Enter on the "Other" row submits the text alone for single-select and keeps
   combining toggled options with the text for multi-select; drop `commit_other`.
3. `question.rs`: `body()` renders `Other: <text>` on the last row with the caret when
   highlighted; `footer()` and `field()` updated; `is_typing`/`typing` removed.
4. `ask_user.rs`: update the `QuestionAnswer` doc comment.
5. Update the existing `question.rs` tests to the new interaction model and add: type-to-answer
   jump, note attached on Enter (single-select), Esc leaving the "Other" row keeps text, digits
   typed on the "Other" row stay in the text, grace period still blocks Char, multi-select
   combining on Enter at the "Other" row, and the inline field fitting the row width. Also
   rework the app.rs test `esc_while_typing_other_only_leaves_the_field` to the new model: type a
   character to land on the "Other" row, assert Esc moves the highlight back to the options
   without answering (there is no row accessor, so assert via `body()`'s highlight), and a second
   Esc declines.
6. `docs/tui.md` "Questions from the model": key table and the paragraph on answers — typing
   starts a custom answer, text can accompany a choice as a note.
7. `docs/guardian.md` (~line 75): acknowledge that a note attached to a choice is recorded the
   same way as standalone "Other" text — `fontaine-core` appends every `answer.other` as an
   `Other: "<text>"` user message regardless of whether options were also selected, so the
   guardian reads a note-on-choice exactly like standalone "Other" text (no code change).
8. `CHANGELOG.md` entry (via the PR step).

## Files to Modify

- `rust/fontaine-tui/src/question.rs`
- `rust/fontaine-tui/src/app.rs` (rework the `esc_while_typing_other_only_leaves_the_field` test)
- `rust/fontaine-tools/src/ask_user.rs` (doc comment only)
- `docs/tui.md`, maybe `docs/guardian.md`
- `CHANGELOG.md`

## Trade-offs

- **Highlight-as-mode vs. a dedicated "custom answer" key.** Claude Code starts a custom answer
  from typing; making the highlight the mode gives the same effect with one concept instead of
  two (option list + field), at the cost of digits/`j`/`k` being un-typeable as a first character
  until the highlight reaches "Other" — a word can always be started with any other letter, or
  the row reached with arrows.
- **Note-on-Enter vs. a separate note shortcut (the issue comment's `n`).** Enter already means
  "submit"; making any field text accompany the selection reuses the existing
  `selected` + `other` shape and the existing multi-select behavior, with no new key to learn.
- **Field always visible vs. on focus only.** An always-drawn `Other: ` row costs one line even
  when unused and answers the "did the dialog dismiss?" impression directly; it also advertises
  the capability the issue says users miss.

## Decisions

- 2026-11-04: No separate `n` shortcut; typed text alongside a chosen option rides the existing
  `selected` + `other` answer shape (issue maintainer's live feedback confirmed the inline-field
  direction during design).

## Documentation

- `docs/tui.md` "Questions from the model" — key table and answer semantics.
- `docs/guardian.md` "what the guardian sees" — acknowledge that a note attached to a choice is
  recorded identically to standalone "Other" text (every `answer.other` becomes an
  `Other: "<text>"` user message), so the guardian reads both the same way.

## Testing Strategy

Unit tests in `question.rs` (all behavior is reachable with constructed key events — per the
verification-tier rule, no manual step substitutes for these): type-to-answer, note-on-Enter,
Esc row-leaving, digit typing on the "Other" row, grace-period blocking, multi-select combining,
inline-field width. `fontaine-tools` tests are untouched (no behavior change). Docs edits carry
no tests.

## Manual Verification

In a real tmux (`cargo run -p fontaine` from `rust/`), ask the model something that triggers
`ask_user_question` (or use `/ship-issue`-style flows) and check that the inline caret renders on
the "Other" row when highlighted and that typed text wraps/clips within the row width. Not
automated because caret placement and row wrapping in a live terminal are layout, not logic
(see `tasks/design/testing.md`, "Reproducing terminal layout bugs").

