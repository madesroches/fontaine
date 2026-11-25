# Accept One Word of Ghost Text Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/788

## Overview

When dimmed ghost text is shown in the input box (a prompt suggestion or a history completion), the forward-word keys (Ctrl-Right, Alt-Right, Alt-F) accept only the next word of it. The rest stays dimmed and can be accepted a word at a time or all at once with Tab / Right. Today the only option is to accept the whole line and then delete the words you don't want. fish and zsh-autosuggestions work this way.

## Current State

- `App::accept_ghost` (`rust/fontaine-tui/src/app.rs` ~839) accepts on plain Tab or Right only. It returns `false` for any key carrying Shift, Alt or Ctrl, and for any key bound in the user's keymap. It inserts the whole ghost text, resets the back-off counters when the ghost was a suggestion (`suggestion.take()`, `unused_suggestions = 0`, `backed_off_turns = 0`) and calls `history.reset()`. Its caller (`handle_key` ~1952) then runs `refresh_completion` / `refresh_slash` and returns `Action::None`. When it returns `false`, the key reaches `message_key` → `InputBuffer::handle_key`, where Ctrl/Alt-Right and Alt-F call `word_right` (`input.rs` ~421, ~434). At the end of the buffer, `word_right` has no effect.
- `App::ghost_text` (~779) returns the suggestion when the input is empty. Otherwise, when the cursor is at the end, the text was not Esc-dismissed and `history_completion` is on, it returns `history.completion(text)`.
- `App::handle_input` (~1395) dismisses the suggestion (counting it unused) whenever the input is non-empty after any input. A suggestion therefore survives only in an empty input.
- `InputBuffer::word_end_after` (`input.rs` ~292) defines a word as alphanumeric runs: skip separators, then skip word chars. This matches Claude Code's documented Alt+F boundaries ("a run of letters and digits").
- Test `modified_arrows_and_mapped_keys_do_not_accept` (`app.rs` ~5405) asserts that Ctrl/Alt/Shift-Right do not accept anything.
- Claude Code (interactive-mode docs, "Prompt suggestions"): Tab or Right places the whole suggestion, and typing dismisses it. Nothing is documented for accepting part of it, and Alt+F only moves the cursor. Fontaine's history completion is already a deviation from Claude Code (plan Decisions, TUI, 2026-11-13 #620).

## Design

### Keys

A word-accept key is any of:

- Right with Ctrl or Alt (and no Shift);
- Alt-F (`KeyCode::Char('f' | 'F')` with Alt only).

These are the same keys that already mean "forward one word" in the input. A key bound in the user's keymap is still excluded (`self.keymap.lookup(key).is_some()`), exactly as for Tab/Right. With no ghost text showing, the key falls through to `word_right` as it does today.

### Word extent

Add a free function in `input.rs`, reusing the buffer's notion of a word:

```rust
/// Length in chars of the first word of `text`, with the separators before it: what
/// forward-word would move over.
pub fn next_word_len(text: &str) -> usize
```

The skip logic is shared with `word_end_after`. Factor it into a private helper over `&[char]` that both functions call, so the two can never disagree. If the ghost text has no word char (for example `"..."`), the whole text is accepted.

### Accepting part of a history completion

No new state is needed. After `"fix the p"` + `"arser"` is extended to `"fix the parser"`, the entry that was shown is the newest one extending the old text. It also extends the new text, so it is still the newest match. `ghost_text` keeps showing the rest of the same entry with no extra bookkeeping.

### Accepting part of a prompt suggestion

A suggestion is shown only in an empty input, and `handle_input` drops it once the input is non-empty. Its remainder therefore needs its own state:

```rust
/// The unaccepted rest of a partly accepted prompt suggestion, valid while the input
/// still holds exactly `anchor`.
suggestion_rest: Option<SuggestionRest>,

struct SuggestionRest { anchor: String, rest: String }
```

- The first word-accept on a suggestion takes `suggestion` (counted as used: back-off reset, exactly as a full accept), inserts the word, and stores `SuggestionRest { anchor: <input text after insert>, rest }` when `rest` is non-empty.
- `ghost_text`: when `ghost_allowed()`, `suggestion_rest` is `Some`, `input.text() == anchor` and the cursor is at the end, return `rest`. This check comes before the history-completion branch, so the suggestion's remainder outranks history.
- Word-accept and Tab/Right on a `rest` ghost work the same as on any ghost. A further word-accept moves `anchor` forward. A full accept clears `suggestion_rest`.
- Any other edit changes the text, so the anchor no longer matches and the rest disappears without counting as unused (the suggestion was already used). Clear `suggestion_rest` lazily at the anchor check, and also in `reset_suggestions`, when a turn starts (`set_running(true)`) and on submit, so a stale rest can never revive.
- Esc while a `rest` ghost is showing clears `suggestion_rest` and returns `Action::None`, mirroring Esc on a suggestion. Add this to the Esc block in `handle_key` (~1914) next to the empty-input suggestion case.

### Code shape

Generalize `accept_ghost(key)`:

```rust
enum GhostAccept { All, Word }

fn ghost_accept_kind(&self, key: KeyEvent) -> Option<GhostAccept>
```

`ghost_accept_kind` returns `All` for plain Tab/Right, `Word` for the keys above, and `None` otherwise, including keymap-bound keys. `accept_ghost` computes the accepted slice (`ghost` or its first `next_word_len` chars), inserts it, resets the back-off when the source was `suggestion`, records or updates or clears `suggestion_rest`, and calls `history.reset()`. The caller in `handle_key` is unchanged.

## Implementation Steps

1. `input.rs`: factor the word-skip into a helper over `&[char]`, add `pub fn next_word_len(text: &str) -> usize`, and keep `word_end_after` behavior identical.
2. `app.rs`: add the `SuggestionRest` struct and the `suggestion_rest` field (initialized `None`). Add `GhostAccept` / `ghost_accept_kind`, extend `accept_ghost`, add the rest branch in `ghost_text`, add Esc handling, and clear the rest in `reset_suggestions`, in `set_running(true)` and on submit.
3. Tests (see Testing Strategy). Replace `modified_arrows_and_mapped_keys_do_not_accept` with a test that Shift-Right still does not accept, Ctrl/Alt-Right accept one word, and a mapped Ctrl-Right does not.
4. Docs (see Documentation).
5. Master plan: append a dated line at the end of the `### TUI` Decisions subsection of `tasks/fontaine_harness_plan.md`. It records the deviation: Claude Code documents no partial accept of a prompt suggestion; Fontaine accepts one word with Ctrl/Alt-Right and Alt-F, following fish.

## Files to Modify

- `rust/fontaine-tui/src/input.rs`
- `rust/fontaine-tui/src/app.rs`
- `docs/tui.md`
- `tasks/fontaine_harness_plan.md` (Decisions, TUI)

## Trade-offs

- **Keys**: reusing the forward-word keys (as fish does) rather than adding a new binding. At the end of the input those keys currently do nothing, so the change takes nothing away and needs no new key to learn. A configurable keymap action was rejected: the TUI keymap has no input-editing actions, and Claude Code's word shortcuts are not remappable either.
- **Suggestion remainder**: an anchored `SuggestionRest` rather than keeping `suggestion` alive while the input is a prefix of it. The prefix approach would also keep the suggestion when the user types its next letters, which contradicts "typing dismisses" (the Claude Code behavior Fontaine copies). It would also need changes in `handle_input`'s dismissal path.
- **Word definition**: alphanumeric runs (the buffer's and Claude Code's Alt+F definition) rather than whitespace-delimited words. Path-heavy history entries accept in smaller steps, but the key does exactly what it does in the input itself.

## Documentation

`docs/tui.md`:
- Key table: in the Right row, add that Ctrl-Right / Alt-Right accept one word of the dimmed text when it is showing (the row currently says they "still move by word"). Mention Alt-F where the table lists it, if it does.
- "Prompt suggestions and history completion": after "Tab or Right fills the input…", add one sentence. Ctrl-Right, Alt-Right or Alt-F accepts only the next word, and the rest stays dimmed. Accepting a word counts as using the suggestion.

## Testing Strategy

Unit tests in `app.rs`'s `ghost` module and `input.rs`, no live services:

- `next_word_len`: `" the parser"` → 4, `"arser"` → 5, `"..."` → 3, `"src/foo"` → 3, `""` → 0. Also an existing `word_right` test still passes, which shows the factoring preserved behavior.
- History completion: type `"fix"` with history `"fix the parser"`. Ctrl-Right gives input `"fix the"` and ghost `" parser"`. Alt-F then gives `"fix the parser"` and no ghost.
- Suggestion: `"run the tests"`, with `unused_suggestions = 2`. Alt-Right gives input `"run"`, ghost `" the tests"`, `suggestion` `None` and `unused_suggestions == 0`. Ctrl-Right gives `"run the"`. Tab gives `"run the tests"`, then Enter submits it.
- Suggestion rest dies on edit: after a word accept, typing `"x"` leaves no ghost (even after Backspace back to the anchor text, the rest is cleared or the anchor has moved: assert no ghost reappears) and `unused_suggestions` stays 0.
- Esc on a rest ghost clears it without being the first press of a double Esc.
- A turn start clears the rest.
- Shift-Right does not accept. A keymap-bound Ctrl-Right does not accept (resolve a keymap with `ctrl+right`, as `a_bound_ctrl_alt_chord_still_fires_with_the_list_focused` does for its chord).
- With no ghost, Ctrl-Right in the middle of text still moves by word (regression guard for the fall-through).
- Vim Insert: Ctrl-Right accepts a word. Vim Normal: no ghost, so nothing is accepted.

## Open Questions

None.
