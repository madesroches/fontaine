# Android Prompt Field Editing Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/727

## Overview

The Android app's prompt (and paste) field is a bare `String` that only supports append and
pop-last-char. This plan gives it a cursor and a selection, so the prompt can be edited anywhere,
a range can be selected and replaced, cut or copied, and typing and Backspace act at the cursor.
The field logic is a portable model with unit tests; the Android layer draws the cursor and
selection and turns taps, drags and a long-press menu into model operations.

Swipe typing, spacebar cursor dragging and IME composition are **not** in this plan: they need an
IME `InputConnection`, which the current build cannot provide (see Trade-offs and Decisions).

## Current State

- `rust/fontaine-android/src/keys.rs` — `apply(&mut String, Key)` appends a char or pops the last
  one; `Enter` asks to submit.
- `rust/fontaine-android/src/android/mod.rs` — `Activity::field: String` (lines ~486/502) and `FrameParams::field: &'a str` (~432); `input()` (~750)
  forwards only pointer 0 to ImGui as a mouse, turns `KeyEvent`s into `Key`s and applies them, and
  takes a `TextEvent` as a wholesale replacement of the field. `clear_field`, `Tap::PasteField` and
  `Tap::SubmitField` reset the field and call `set_text_input_state(default())`.
- `rust/fontaine-android/src/android/ui.rs` — `field_frame` (~303) draws the field as a framed,
  wrapped text child and reports `Tap::FocusField` (short tap) or opens a "Paste" popup (long
  press). `Frame::field` is `&str`.
- `rust/fontaine-android/src/android/jni_bridge.rs` — `clipboard_text` reads the clipboard; there is
  no writer.
- The APK is built without Gradle or Java (`build-apk.sh`: cargo-ndk + aapt2 + zipalign;
  `AndroidManifest.xml` has `android:hasCode="false"`, activity `android.app.NativeActivity`).
- `android-activity` 0.6.1's NativeActivity backend (`native_activity/mod.rs`): `set_text_input_state`
  is a documented NOP and no `InputEvent::TextEvent` is ever produced (only the GameActivity backend
  emits it). So the IME can only deliver `KeyEvent`s: no composition, no selection sync, no swipe
  typing, no spacebar cursor gesture. Those require a Java `InputConnection` or GameActivity, both
  of which add a Java/dex step to the build.

## Design

### Portable model (`rust/fontaine-android/src/field.rs`, new, exported from `lib.rs`)

```text
Field { text: String, anchor: usize, cursor: usize }   // byte offsets on char boundaries
```

- Selection is the range between `anchor` and `cursor`; empty when equal. `cursor` is the moving end.
- Operations, all clamping to char boundaries and never panicking on any input:
  `insert(&str)` / `insert_char(char)` (replaces the selection), `backspace()` (deletes the
  selection, else the char before the cursor), `delete_forward()`, `move_to(idx)`,
  `select(anchor, cursor)`, `select_all()`, `move_left/right(extend: bool)`, `selected() -> &str`,
  `cut() -> String` (returns selected text, deletes it), `replace_all(&str)` (paste into the empty
  paste field; cursor at end), `clear()`, `as_str()`, `is_empty()`, `cursor()`, `selection() ->
  Option<Range<usize>>`.
- `keys::apply` takes `&mut Field` instead of `&mut String`; `Key` gains `Left`, `Right`, `Delete`
  (D-pad and hardware keyboards deliver them; the soft keyboard does not). Behavior for existing
  keys is unchanged except that it now acts at the cursor.
- Paste (`Tap::PasteField`) inserts the clipboard text untrimmed at the cursor over the selection in
  the prompt field; the paste field keeps its current whole-replacement behavior with the text
  trimmed.
- A `TextEvent` (only produced by a future backend) keeps replacing the text, via `replace_all`,
  with the cursor taken from its `selection`, which is in UTF-16 code units: convert to a byte
  offset, then clamp to the text.

### Android layer

- **Hit-testing and drawing (`ui.rs`)**: `field_frame` takes `&Field`. It draws the wrapped text
  with the selection as a filled rect behind the selected glyph range and the cursor as a thin
  line (blinking is skipped: a steady line is enough and avoids forced redraws). Mapping a point to
  a byte index uses ImGui's text layout: wrap lines with `ImFont_CalcWordWrapPosition`, pick the
  line from `y`, then the char whose cumulative width spans `x`. The reverse
  (index to rect) uses the same wrap. Both live in one small helper pair so draw and hit-test
  cannot disagree. To match what ImGui draws, the helpers copy `ImFont::RenderText`'s line loop:
  wrap with `ImFont_CalcWordWrapPosition(font, size, text, end, wrap_width)` and
  `igImTextCalcWordWrapNextLineStart`, so blanks (spaces, tabs, one `\n`) after a wrap are skipped
  and indices in those runs map to the line end; measure with the unrounded `ImFont_CalcTextSizeA`
  (font `igGetFont()`, size `igGetFontSize()`), not `igCalcTextSize`, which rounds widths up, with
  `hide_text_after_double_hash` false; the wrap width is the child's own content width, not
  `width - 2*pad_x`; and the measured field height counts the empty line after a trailing `\n`.
  A NUL byte shifts offsets (`c()` strips it before drawing, and `clipboard_text` can return one),
  so `Field::insert` and `replace_all` strip `'\0'`.
- **Gestures** (pointer 0 only, as now):
  - short tap → `Tap::MoveCursor(idx)` and open the keyboard (replaces `Tap::FocusField`);
  - press-and-drag after the long-press threshold → `Tap::SelectRange(anchor, idx)` (anchor is the
    index under the press); a drag that starts before the threshold cancels the tap and the long
    press, as today (nothing scrolls);
  - a `UiState` flag tracks a drag-selection in progress; the existing open-popup-while-held branch
    is replaced by opening the menu at release;
  - long-press release without a drag → select the word under the finger and open the menu;
  - the menu (existing popup) offers **Cut / Copy / Paste / Select all**, each shown only when it
    applies (Cut/Copy need a selection; Paste needs clipboard text), emitted as `Tap::Cut`,
    `Tap::Copy`, `Tap::PasteField`, `Tap::SelectAll`.
  - Word selection is a portable helper on `Field` (`select_word_at(idx)`) so it is unit tested.
- **Clipboard write (`jni_bridge.rs`)**: `set_clipboard_text(&AndroidApp, &str)` via
  `ClipboardManager.setPrimaryClip(ClipData.newPlainText)`, mirroring `clipboard_text`'s attach and
  error handling. It returns `Option<()>` (from `with_activity`, which swallows JNI errors). On
  failure the selection is kept and the activity shows a `set_notice`. Cut reads `selected()`,
  writes, and calls `cut()` only if the write succeeded.
- **State reset**: unchanged: `clear_field` and the post-paste reset keep calling
  `set_text_input_state`.

## Implementation Steps

1. `field.rs`: the `Field` model and its unit tests; export in `lib.rs`.
2. `keys.rs`: switch `apply` to `&mut Field`, add `Left`/`Right`/`Delete`, update its tests.
3. `jni_bridge.rs`: `set_clipboard_text` returning `Option<()>`.
4. `ui.rs`: index↔point helpers, selection and cursor drawing, new gestures and menu; new `Tap`
   variants (`MoveCursor`, `SelectRange`, `Cut`, `Copy`, `SelectAll`); `Frame::field: &Field`.
5. `android/mod.rs`: `Activity::field: Field` and `FrameParams::field: &Field`; `input()` maps
   `Keycode::DpadLeft/Right`, `ForwardDel`; `handle_tap` handles the new taps (Copy writes the
   clipboard; Cut writes it and deletes the selection only if the write succeeded, else
   `set_notice` and the selection stays).
6. Docs and changelog.

## Files to Modify

- `rust/fontaine-android/src/field.rs` (new), `lib.rs`, `keys.rs`
- `rust/fontaine-android/src/android/{mod.rs,ui.rs,jni_bridge.rs}`
- `docs/android.md`, `CHANGELOG.md`

## Trade-offs

- **Stay on NativeActivity, field-model only (chosen)** vs. a Java `InputConnection` subclass vs.
  GameActivity. The two alternatives are the only routes to swipe typing, spacebar dragging and
  composition, but each adds a Java compile + dex step to a deliberately Gradle-free build
  (`hasCode=false`), changes the manifest and the dev/release workflows, and cannot be exercised in
  CI or without a device. The chosen route delivers every acceptance criterion with portable,
  unit-tested logic, and the model's cursor/selection are exactly what an `InputConnection` would
  later need to mirror, so it is the first step of either alternative rather than throwaway work.
- **ImGui `InputTextMultiline`** was rejected: it would put cursor and selection logic inside
  ImGui where the portable unit tests cannot reach, and the issue requires portable field logic.

## Decisions

- AFK: swipe typing, spacebar cursor dragging and IME composition are deferred to a follow-up issue
  (Java `InputConnection` or GameActivity); the plan covers only what NativeActivity can deliver.
  This leaves two "Wanted" bullets of the issue open while meeting its Acceptance section.
- No cursor blink; a steady cursor line.

## Documentation

- `docs/android.md`: describe tap-to-place, long-press/drag selection, the Cut/Copy/Paste/Select all
  menu, and state that swipe typing and spacebar cursor dragging are not supported.
- `CHANGELOG.md`: an `[Unreleased]` entry under `### Added`, at the end of the `#### TUI` subject section (the
  issue's `area:tui` label, per CONTRIBUTING.md); create the section if missing.

## Testing Strategy

Automated, in the portable part (`cargo test -p fontaine-android`):
- `Field`: insert/backspace/delete at start, middle, end; over a selection; with multi-byte chars
  (`é`, emoji) so offsets never split a char; `move_to`/`select` clamping out-of-range and
  mid-char indices; `select_all`, `select_word_at` (word, punctuation, whitespace, empty text);
  `cut`; `replace_all`; NUL stripping in `insert`/`replace_all`; left/right with and without extend; collapsing a selection on move.
- `keys::apply`: existing tests ported, plus typing/backspace at a cursor in the middle, replacing a
  selection, Left/Right/Delete.

Android-only code (`ui.rs`, `mod.rs`, `jni_bridge.rs`) is not unit-testable here; the CI check is
that it compiles and lints (`cargo ndk -t arm64-v8a --platform 26 clippy -p fontaine-android --all-targets -- -D warnings`,
the `apk` job in `.github/workflows/android.yml`).

## Manual Verification

On a device (build with `rust/fontaine-android/build-apk.sh out.apk --debug-sign`, install, pair
with a host), because gesture feel and text layout cannot be asserted without a screen:
1. In a session, type "hello world", tap between "hello" and "world": the cursor line appears there;
   type "X": it lands at the cursor.
2. Press Backspace: removes the char before the cursor, not the last char.
3. Long-press a word: it highlights and the menu shows Cut / Copy / Paste / Select all. Cut, then
   Paste elsewhere: text moves.
4. Drag from long-press across several wrapped lines: the selection follows the finger; typing
   replaces it.
5. Copy, open another app, paste: the copied text appears.
6. On the scan screen, paste a link and tap Use this: unchanged behavior.

## Open Questions

None blocking. Whether to schedule the `InputConnection` follow-up is the maintainer's call.
