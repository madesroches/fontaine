# Android IME Input (Swipe Typing and Composition) Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/751

## Overview

The Android app's soft keyboard cannot swipe-type, show suggestions or drag the spacebar to move the
cursor, because `NativeActivity`'s content view returns no `InputConnection` and the IME falls back
to a key-events-only mode. This plan adds a small Java layer: an `Activity` subclass hosting an
invisible text-editor view whose `InputConnection` mirrors the prompt/paste `Field` (text, selection,
composing region) and forwards IME edits to Rust over JNI. The portable `Field` model gains a
composing region and a bidirectional, UTF-16-aware sync with the IME, all unit-tested on the host.

## Current State

- `rust/fontaine-android/AndroidManifest.xml`: `android:hasCode="false"`, activity
  `android.app.NativeActivity`.
- `rust/fontaine-android/build-apk.sh`: `cargo ndk` → `aapt2 link` (manifest only) → `zip lib` →
  `zipalign` → optional debug sign. No Java step.
- `src/android/mod.rs`: `Activity::input()` handles `InputEvent::KeyEvent` (the only path that works
  today); the `TextEvent`/`TextAction` branches and every `set_text_input_state` call are dead under
  android-activity 0.6.1's NativeActivity backend (the calls are no-ops). `show_soft_input` /
  `hide_soft_input` act on the window's decor view, which has no `InputConnection`.
- `src/field.rs`: `Field { text, anchor, cursor }`, byte offsets on char boundaries, NUL stripped.
  `src/keys.rs`: `Key::Enter` submits a non-empty field, so the field is a single logical line that
  wraps on screen, never one holding newlines.
- `src/android/jni_bridge.rs`: `with_activity` runs a closure with the attached `JNIEnv` and the
  activity object; the activity object is the instance of the class named in the manifest.
- `docs/android.md` (line ~69) documents swipe typing as unsupported.
- `.github/workflows/android.yml`, `release.yml` and `dev.yml` all build the APK through
  `build-apk.sh`, but on different runner images: `android.yml` on `ubuntu-latest` (JDK 17), `release.yml`
  and `dev.yml` (`build-android`) on `ubuntu-22.04` (default JDK 11); none uses `setup-java`.

## Design

```
 IME (Gboard)  <──InputConnection──>  FontaineActivity.ImeView (Java, UI thread)
                                           │  mirror Editable (text, selection, composing span)
                       nativeImeState(...) │ ▲ setImeState(...)  showKeyboard()/hideKeyboard()
                                           ▼ │
                      static latest-wins slot (Rust, Mutex)  ◄── Activity::input() drains per frame
                                           │
                                  ImeSync ─┴─ Field (bytes) ⇄ ImeState (UTF-16)
```

### Java layer (`rust/fontaine-android/java/dev/fontaine/remote/FontaineActivity.java`)

- `FontaineActivity extends NativeActivity`. After `super.onCreate`, adds a 1×1 transparent `ImeView`
  with `addContentView`; the view calls `setFocusableInTouchMode(true)` and `requestFocus()` after `addContentView`
  (and again in `showKeyboard()`), since a merely focusable view cannot take focus in touch mode and
  `InputMethodManager` could not serve it.
  Touches still reach the native input queue (NativeActivity takes the window's queue), so the view
  never sees them.
- `ImeView.onCheckIsTextEditor()` is true. `onCreateInputConnection(EditorInfo)` sets
  `inputType = TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_CAP_SENTENCES`, `imeOptions = IME_ACTION_SEND |
  IME_FLAG_NO_EXTRACT_UI | IME_FLAG_NO_FULLSCREEN`, `initialSelStart/End` from the mirror, and
  returns a `BaseInputConnection(view, true)` subclass whose `getEditable()` returns the shared
  mirror `SpannableStringBuilder`. The base class then implements `commitText`, `setComposingText`,
  `setComposingRegion`, `deleteSurroundingText`, `deleteSurroundingTextInCodePoints` (API 24),
  `replaceText` (API 34; the app targets SDK 34), `setSelection`, `finishComposingText` and the
  `getText*` queries against the mirror. The subclass overrides each mutator to call `super` and
  then mark the mirror dirty, and overrides `performEditorAction(IME_ACTION_SEND/DONE/GO)` to report a submit. The subclass counts batch depth
  itself in overridden `beginBatchEdit`/`endBatchEdit` (`BaseInputConnection` keeps none;
  `endBatchEdit` returns depth > 0). While the count is above 0 mutators only mark the mirror dirty; the report to Rust happens once when
  the count returns to 0, and immediately after a mutator that runs while the count is 0. It calls
  `InputMethodManager.updateSelection(view, selStart, selEnd, compStart, compEnd)` from the mirror at those same points,
  since `BaseInputConnection` does not and the IME's cursor/composing tracking would go stale.
  `sendKeyEvent` is left to the base class, which re-dispatches the key to the window, so it
  reaches Rust as the existing `InputEvent::KeyEvent` (keys, Backspace and Enter from IMEs that
  still use them).
- `FontaineActivity` has `static { System.loadLibrary("fontaine_android"); }`: NativeActivity opens the
  library only to find `ANativeActivity_onCreate`, and ART resolves `Java_*` symbols only from
  libraries loaded through `System.loadLibrary`.
- Reporting calls `static native void nativeImeState(String text, int selStart, int selEnd,
  int compStart, int compEnd, int gen)` (composing −1/−1 when absent; `gen` is the generation of the
  last Rust push Java applied, 0 before any) and `static native void nativeImeSubmit()`.
- Rust-driven methods: `setImeState(String, int, int, int, int, int gen, boolean wholesale)` replaces the mirror and the
  selection/composing spans, stores `gen` as the generation echoed by every later report, then posts
  `InputMethodManager.restartInput` only when `wholesale` (submit-clear, paste/`replace_all`, cut, as
  `TextView.setText` does) and `updateSelection` otherwise (hardware key, IME DEL/digit key event, tap),
  so incremental edits keep the IME's suggestion, shift and autocorrect-undo state; `showKeyboard()` does `requestFocus()` then
  `showSoftInput` on `ImeView` on the UI thread, (no `toggleSoftInput` fallback: deprecated since API 31, and `showSoftInput` already works under NativeActivity);
  `hideKeyboard()` calls `hideSoftInputFromWindow`. All hop to the UI thread with
  `runOnUiThread`.
- Manifest: `android:name="dev.fontaine.remote.FontaineActivity"`, drop `android:hasCode="false"`.

### Build (`build-apk.sh`)

After `cargo ndk`, compile the Java: `javac --release 8 -cp android.jar -d classes` over the `find java -name '*.java'` list,
then `$tools/d8 --lib android.jar --min-api 26 --output dex` over the `find classes -name '*.class'` list, and add `classes.dex`
at the APK root (`zip -j` into `base.apk` before `zipalign`). The script selects JDK 17 explicitly (the
newest build-tools' `d8` may need it): `JAVA_HOME_17_X64` when set, else both `java -version` and `javac -version` must report
>= 17; otherwise it fails with a clear message. It exports `JAVA_HOME` set to the chosen JDK and puts
`$JAVA_HOME/bin` first on `PATH` before running javac and d8, since the build-tools `d8` wrapper ends in
`exec java` and ignores `JAVA_HOME`. All three workflows therefore get the same toolchain. After the build, assert the staged `libfontaine_android.so` exports
`Java_dev_fontaine_remote_FontaineActivity_nativeImeState` and `..._nativeImeSubmit` (NDK
`llvm-nm -D`, found as the workflows do: `ANDROID_NDK_HOME`, else the newest `$ANDROID_HOME/ndk/*`, at
`toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm`), so a symbol dropped by the linker fails the build, not the first launch.

### Portable model (`src/field.rs`, new `src/ime.rs`)

- `Field` gains `composing: Option<Range<usize>>` (bytes). It is cleared by every mutator that is not
  an IME apply (`collapse_to`, `select`, `replace_all`, `clear` cover them all), so an app-side
  edit, tap or paste ends the composition.
- `ime.rs` (host-testable, no Android types):
  - `ImeState { text: String, sel: (usize, usize), composing: Option<(usize, usize)> }`, offsets in
    UTF-16 code units as Java reports them.
  - `utf16_to_byte(text, units) -> usize` and `byte_to_utf16(text, byte) -> usize`; an offset inside a
    surrogate pair rounds down to the char start; out-of-range offsets clamp.
  - `ImeState::apply_to(&self, &mut Field)` and `ImeState::from_field(&Field)` convert between UTF-16
    and byte offsets and call the two `Field` methods below, which live in `field.rs` (its text,
    anchor, cursor and `stripped()` are private to that module, and the public setters clear
    `composing`).
- `field.rs` (byte offsets only; `ime.rs` has already converted from/to UTF-16):
  - `Field::apply_ime(text: &str, anchor: usize, cursor: usize, composing: Option<Range<usize>>)`:
    strips NUL, shifting every offset by the NULs before it, sets text, anchor/cursor (selection
    `sel.0` = anchor, `sel.1` = cursor) and composing (empty or inverted ranges become `None`).
  - `Field::ime_state() -> (String, usize, usize, Option<Range<usize>>)`: the reverse, from the field.
  - `ImeSync { last: Option<ImeState>, gen: u32, wholesale: bool }`: `incoming(&mut self, &mut Field, ImeState, report_gen: u32)`
    applies the state, then records `field.ime_state()` converted back (the normalized state after NUL
    stripping, surrogate rounding and composing normalization, not the raw input) as `last`, so a
    normalized state is never echoed back. Convergence rule: `gen` counts pushes Rust has sent. A
    report with `report_gen == gen` was built after Java applied the latest push and is recorded as
    above. A report with `report_gen < gen` was built on a stale mirror: it is still applied (the IME's edit is
    not lost) but `last` is reset to `None`, so `outgoing` re-pushes the Field and Java converges.
    `outgoing(&self, &Field) -> Option<ImeState>` returns the field's state only when it differs from
    `last`; it does not mutate, so a second call without `mark_sent` returns the same state. The caller
    pushes it tagged `gen + 1` and the `wholesale()` flag, then calls `mark_sent(&mut self, ImeState)`
    only after a successful JNI push, which sets `last`, increments `gen` and clears the flag, so a failed push is retried next frame.
    `note_wholesale(&mut self)` sets the flag; the Activity calls it on submit-clear, paste/`replace_all`
    and cut, and a stale-report reset also sets it (the IME must restart on the corrected text). Tap and key
    edits leave it clear. The Activity calls `incoming`
    for drained IME state (with its generation) and `outgoing` once per frame after all input, so a field change from a tap,
    paste, hardware key or submit reaches the IME, and an IME change is never echoed back.

### Android glue (`src/android/ime_bridge.rs`, `mod.rs`, `jni_bridge.rs`, `ui.rs`)

- `ime_bridge.rs`: the two exported `#[unsafe(no_mangle)] extern "system"` JNI functions write into a
  `static Mutex<Pending>` (locked with `.expect("...")` like the crate's other locks; the critical sections are plain
  assignments and `take`, so the lock cannot poison)
  and `take()` drains it. The `Pending { state: Option<(ImeState, u32)>, submit: bool }` slot type lives in
  portable `ime.rs` (latest state wins; submit is a sticky flag).
  Java-side calls (`set_state`, `show`, `hide`) go through `jni_bridge::with_activity`, clearing
  exceptions as the other bridge calls do.
- `mod.rs`: `input()` drains `ime_bridge::take()` after the event loop: the drained IME state is applied via
  `ImeSync::incoming` where `typed` is today (before the `pressed` loop, so key events, taps, paste and
  submit-clear handled later in the same `input()` change the Field after the IME state, and `outgoing`
  at the end of the frame pushes the result), and the IME submit flag is ORed into `submitted`
  before the existing `submitted && !self.field.is_empty()` check. Remove the dead `TextEvent`/`TextAction` branches and every
  `set_text_input_state` call; replace `show_soft_input(true)`/`hide_soft_input(false)` with the
  bridge's `show`/`hide`; `frame()` pushes `ImeSync::outgoing` to Java after input handling and
  after taps. Hardware-key and `KeyEvent` handling is unchanged. Clearing the field on submit
  flows through the same `outgoing` check, so the IME's mirror is reset too.
- `ui.rs`: in `field_frame`, when the field is not empty, draw the composing underline along the bottom
  edge of each box returned by `Layout::selection_boxes(composing)` (as the selection is drawn at
  ui.rs:535-546) so the in-progress word is visible, since the field is drawn by ImGui and not by a Java text widget.

## Implementation Steps

1. `src/field.rs`: add `composing`, its accessor and clearing, and `apply_ime`/`ime_state` (byte
   offsets; `ime.rs` does the UTF-16 conversion); update existing mutators and the
   stale header comment (lines 1-2); update the `src/keys.rs` header comment (lines 1-2).
2. `src/ime.rs` and `src/lib.rs`: `ImeState`, conversions (`apply_to`, `from_field`), `ImeSync`, `Pending`, tests.
3. `java/dev/fontaine/remote/FontaineActivity.java`: activity, `ImeView`, connection, native hooks.
4. `AndroidManifest.xml`: subclass name, `hasCode` removed.
5. `build-apk.sh`: javac + d8 + dex packaging + symbol-export check; update its header comment (add
   javac, d8, llvm-nm to the tools/requirements).
6. `src/android/ime_bridge.rs` plus `mod.rs`/`jni_bridge.rs` changes (`with_activity` becomes
   `pub(super)`); remove the dead `TextEvent`/`TextAction` branches, the `set_text_input_state` calls
   (mod.rs:610, :671, :681), `hide_soft_input` (:693), the `typed` variable and the
   `TextInputState`/`TextInputAction` imports (:15).
7. `ui.rs`: composing underline.
8. Docs, changelog, master-plan edits in `tasks/fontaine_harness_plan.md`: dated (#751) Decisions lines at
   the end of `### Build and release` (the subclass-over-GameActivity choice; the Java/dex build step), an
   amending line for the #721 line at ~:503 whose reason "the activity has no Java code to apply
   fitsSystemWindows" no longer holds, and the workspace-layout line at ~:234 gaining the Java
   `FontaineActivity`, the `ime` module and the javac/d8 build.

## Files to Modify

- `rust/fontaine-android/{AndroidManifest.xml, build-apk.sh}`
- `rust/fontaine-android/java/dev/fontaine/remote/FontaineActivity.java` (new)
- `rust/fontaine-android/src/{field.rs, lib.rs, ime.rs (new)}`
- `rust/fontaine-android/src/android/{mod.rs, jni_bridge.rs, ime_bridge.rs (new), ui.rs}`
- `docs/android.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions, #721 line amendment,
  workspace-layout line for `fontaine-android`)

## Decisions

- AFK: Chose the custom `NativeActivity` subclass over the issue's preferred GameActivity, as the
  issue lists both and this avoids a pinned AndroidX dependency closure and an event-loop rewrite
  that cannot be verified in CI.
- AFK: The editor declares `TYPE_CLASS_TEXT | CAP_SENTENCES` with a Send action, not the issue's
  multi-line flag, because the field's Enter submits and the model never holds newlines; a
  multi-line editor makes keyboards insert newlines instead of sending.
- AFK: Rust-to-Java pushes carry a generation counter that Java echoes in every report; a report from an older generation is applied but resets `last` so the field is re-pushed (converges without reverting a hardware key typed in between).
- AFK: `restartInput` only for wholesale pushes (submit-clear, paste/`replace_all`, cut), flagged on the push; incremental key and tap edits use `updateSelection` only.
- AFK: The `Pending` lock uses `.expect("...")` rather than poison recovery, per AGENTS.md; its critical sections cannot panic.
- AFK: `ImeSync::incoming` records the normalized `field.ime_state()` as `last`; `outgoing(&self)` is pure and `mark_sent(&mut self, ImeState)` commits a successful push.

## Documentation

- `docs/android.md`: replace the "swipe typing … not supported" paragraph with what works (swipe
  typing, suggestions, spacebar cursor drag, composition underline).
- `CHANGELOG.md` `[Unreleased]` entry (#751).

## Testing Strategy

Host unit tests (run on every OS in `ci.yml`):
- `ime.rs`: UTF-16↔byte conversion for ASCII, BMP multi-byte (`é`), astral chars (emoji, surrogate
  pairs), offsets inside a pair, out-of-range offsets; `ImeState::apply_to`/`from_field` round-trip.
- `ImeSync`: no outgoing after `incoming` of the same state; outgoing after a tap/paste/clear
  changes the field; unchanged field yields nothing; `incoming` of a state needing normalization (a NUL,
  a mid-surrogate-pair offset, an inverted composing range) yields no `outgoing`; a second `outgoing`
  without `mark_sent` returns the same state, and after `mark_sent` none; wholesale flag set by
  `note_wholesale`, cleared by `mark_sent`.
- Race interleaving: push gen 1 (field changed by a hardware key), then an `incoming` report built on the
  old mirror with `report_gen` 0 arrives: the field takes the report, `outgoing` yields the field again
  (wholesale); a report with the current gen leaves `outgoing` empty.
- `Pending`: latest of several stored states (with their generation) wins; the submit flag is set and cleared by `take`.
- `field.rs`: every non-IME mutator clears `composing`; `apply_ime` sets it, with selection, empty and
  inverted composing, and NUL stripped with offsets shifted; `ime_state` round-trips with `apply_ime`.
- Swipe-typing scenario as a sequence: compose a word, replace it from a suggestion, commit, then a
  tap-driven `move_to` resets composing.

CI (`android.yml`, `release.yml`, `dev.yml`) compiles the Java, dexes it and checks the exported JNI symbols while building
the debug APK; a Java compile error or dropped symbol fails the PR. No automated test can drive a
real IME.

## Manual Verification

Install the debug APK from the PR's `android.yml` artifact on a phone with Gboard (not automatable:
needs a real IME and touch).
1. Connect screen: tap the paste field, swipe a word. Expect the word to appear underlined while
   composing and commit on the next word or space.
2. Session prompt: swipe-type a sentence, tap a suggestion in the strip over a word. Expect the
   replacement at the cursor.
3. Drag the spacebar left and right. Expect the cursor to move in the field.
4. Hold-select a word in the field, then type. Expect the selection to be replaced; select with the
   keyboard's selection gesture and expect the field's selection to follow.
5. Tap-to-place cursor, drag-select, Cut/Copy/Paste menu, hardware keyboard keys, the Send key.
   Expect all to behave as before and Send to submit the prompt.
