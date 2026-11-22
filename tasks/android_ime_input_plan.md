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
  `hide_soft_input` act on NativeActivity's own content view, which has no `InputConnection`.
- `src/field.rs`: `Field { text, anchor, cursor }`, byte offsets on char boundaries, NUL stripped.
  `src/keys.rs`: `Key::Enter` submits a non-empty field, so the field is a single logical line that
  wraps on screen, never one holding newlines.
- `src/android/jni_bridge.rs`: `with_activity` runs a closure with the attached `JNIEnv` and the
  activity object; the activity object is the instance of the class named in the manifest.
- `docs/android.md` (line ~69) documents swipe typing as unsupported.
- `.github/workflows/android.yml`, `release.yml` and `dev.yml` all build the APK through
  `build-apk.sh`; the runners' JDK (already used for `keytool`) provides `javac`.

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
  with `addContentView`; the view is focusable and takes focus so `InputMethodManager` can serve it.
  Touches still reach the native input queue (NativeActivity takes the window's queue), so the view
  never sees them.
- `ImeView.onCheckIsTextEditor()` is true. `onCreateInputConnection(EditorInfo)` sets
  `inputType = TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_CAP_SENTENCES`, `imeOptions = IME_ACTION_SEND |
  IME_FLAG_NO_EXTRACT_UI | IME_FLAG_NO_FULLSCREEN`, `initialSelStart/End` from the mirror, and
  returns a `BaseInputConnection(view, true)` subclass whose `getEditable()` returns the shared
  mirror `SpannableStringBuilder`. The base class then implements `commitText`, `setComposingText`,
  `deleteSurroundingText`, `setSelection`, `finishComposingText` and the `getText*` queries against
  the mirror. The subclass overrides each mutator to call `super` and then report the mirror to
  Rust, and overrides `performEditorAction(IME_ACTION_SEND/DONE/GO)` to report a submit.
  `sendKeyEvent` is left to the base class, which re-dispatches the key to the window, so it
  reaches Rust as the existing `InputEvent::KeyEvent` (keys, Backspace and Enter from IMEs that
  still use them).
- Reporting calls `static native void nativeImeState(String text, int selStart, int selEnd,
  int compStart, int compEnd)` (composing −1/−1 when absent) and `static native void nativeImeSubmit()`.
- Rust-driven methods, each `synchronized` on the mirror and guarding against echo with a
  `programmatic` flag: `setImeState(String, int, int, int, int)` replaces the mirror and the
  selection/composing spans, then posts `InputMethodManager.updateSelection` (and `restartInput`
  when the text changed under a live composition); `showKeyboard()` requests focus on the view and
  `showSoftInput`; `hideKeyboard()` calls `hideSoftInputFromWindow`. All hop to the UI thread with
  `runOnUiThread`.
- Manifest: `android:name="dev.fontaine.remote.FontaineActivity"`, drop `android:hasCode="false"`.

### Build (`build-apk.sh`)

After `cargo ndk`, compile the Java: `javac --release 8 -cp android.jar -d classes <java/**/*.java>`,
then `$tools/d8 --lib android.jar --min-api 26 --output dex classes/**/*.class`, and add `classes.dex`
at the APK root (`zip -j` into `base.apk` before `zipalign`). Fail with a clear message when `javac`
is missing. After the build, assert the staged `libfontaine_android.so` exports
`Java_dev_fontaine_remote_FontaineActivity_nativeImeState` and `..._nativeImeSubmit` (NDK
`llvm-nm -D`), so a symbol dropped by the linker fails the build, not the first launch.

### Portable model (`src/field.rs`, new `src/ime.rs`)

- `Field` gains `composing: Option<Range<usize>>` (bytes). It is cleared by every mutator that is not
  an IME apply (`collapse_to`, `select`, `replace_all`, `clear` cover them all), so an app-side
  edit, tap or paste ends the composition.
- `ime.rs` (host-testable, no Android types):
  - `ImeState { text: String, sel: (usize, usize), composing: Option<(usize, usize)> }`, offsets in
    UTF-16 code units as Java reports them.
  - `utf16_to_byte(text, units) -> usize` and `byte_to_utf16(text, byte) -> usize`; an offset inside a
    surrogate pair rounds down to the char start; out-of-range offsets clamp.
  - `Field::apply_ime(&ImeState)`: strips NUL, shifting every offset by the NULs before it, converts
    offsets, sets text, anchor/cursor (selection `sel.0` = anchor, `sel.1` = cursor) and composing
    (empty or inverted ranges become `None`).
  - `Field::ime_state() -> ImeState`: the reverse, from the field.
  - `ImeSync { last: Option<ImeState> }`: `incoming(&mut self, &mut Field, ImeState)` applies and
    records the applied state as `last`; `outgoing(&mut self, &Field) -> Option<ImeState>` returns the
    field's state only when it differs from `last` and records it. The Activity calls `incoming`
    for drained IME state and `outgoing` once per frame after all input, so a field change from a tap,
    paste, hardware key or submit reaches the IME, and an IME change is never echoed back.

### Android glue (`src/android/ime_bridge.rs`, `mod.rs`, `jni_bridge.rs`, `ui.rs`)

- `ime_bridge.rs`: the two exported `#[unsafe(no_mangle)] extern "system"` JNI functions write into a
  `static Mutex<Pending { state: Option<ImeState>, submit: bool }>` (latest state wins; lock
  panics on poison per AGENTS.md because the contents feed field text) and `take()` drains it.
  Java-side calls (`set_state`, `show`, `hide`) go through `jni_bridge::with_activity`, clearing
  exceptions as the other bridge calls do.
- `mod.rs`: `input()` drains `ime_bridge::take()` after the event loop: `incoming` the state, set
  `submitted` for a submit. Remove the dead `TextEvent`/`TextAction` branches and every
  `set_text_input_state` call; replace `show_soft_input(true)`/`hide_soft_input(false)` with the
  bridge's `show`/`hide`; `frame()` pushes `ImeSync::outgoing` to Java after input handling and
  after taps. Hardware-key and `KeyEvent` handling is unchanged. Clearing the field on submit and
  switching screens flow through the same `outgoing` check, so the IME's mirror is reset too.
- `ui.rs`: draw an underline beneath `field.composing()` in `field_frame` so the in-progress word is
  visible, since the field is drawn by ImGui and not by a Java text widget.

## Implementation Steps

1. `src/field.rs`: add `composing`, its accessor and clearing; update existing mutators.
2. `src/ime.rs` and `src/lib.rs`: `ImeState`, conversions, `apply_ime`, `ime_state`, `ImeSync`, tests.
3. `java/dev/fontaine/remote/FontaineActivity.java`: activity, `ImeView`, connection, native hooks.
4. `AndroidManifest.xml`: subclass name, `hasCode` removed.
5. `build-apk.sh`: javac + d8 + dex packaging + symbol-export check.
6. `src/android/ime_bridge.rs` plus `mod.rs`/`jni_bridge.rs` changes; remove dead `TextEvent` code.
7. `ui.rs`: composing underline.
8. Docs, changelog, master-plan Decisions.

## Files to Modify

- `rust/fontaine-android/{AndroidManifest.xml, build-apk.sh}`
- `rust/fontaine-android/java/dev/fontaine/remote/FontaineActivity.java` (new)
- `rust/fontaine-android/src/{field.rs, lib.rs, ime.rs (new)}`
- `rust/fontaine-android/src/android/{mod.rs, jni_bridge.rs, ime_bridge.rs (new), ui.rs}`
- `docs/android.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions, workspace-layout
  line for `fontaine-android`), `tasks/design/platforms.md` if it describes the APK build
- `.github/workflows/android.yml` only if the JDK is not on `PATH` for `javac` (verified in step 5)

## Trade-offs

- **Custom `NativeActivity` subclass (chosen) vs `GameActivity`.** GameActivity extends
  `AppCompatActivity`, so the Gradle-free build would have to fetch and pin the
  `games-activity`/appcompat/core/fragment/lifecycle AAR closure, merge their resources with aapt2
  and ship them in a signed release APK, and the whole event loop moves to a different backend, none
  of it testable without a device. The subclass adds one Java file compiled by tools already on the
  runner, keeps the proven backend, and the IME logic that matters (offset conversion, sync) lives
  in portable Rust.
- **Mirror-`Editable` `BaseInputConnection` vs a hand-written `InputConnection`.** The base class
  implements the many text queries IMEs make; only mutations need forwarding.
- **Whole-state reports vs deltas.** Sending the full text/selection/composing after each mutation
  is idempotent and latest-wins, so a dropped or merged update cannot desynchronize the field;
  prompts are short.

## Decisions

- AFK: Chose the custom `NativeActivity` subclass over the issue's preferred GameActivity, as the
  issue lists both and this avoids a pinned AndroidX dependency closure and an event-loop rewrite
  that cannot be verified in CI.
- AFK: The editor declares `TYPE_CLASS_TEXT | CAP_SENTENCES` with a Send action, not the issue's
  multi-line flag, because the field's Enter submits and the model never holds newlines; a
  multi-line editor makes keyboards insert newlines instead of sending.

## Documentation

- `docs/android.md`: replace the "swipe typing … not supported" paragraph with what works (swipe
  typing, suggestions, spacebar cursor drag, composition underline) and note the Java activity
  in the build notes if the page has them.
- `CHANGELOG.md` `[Unreleased]` entry (#751).

## Testing Strategy

Host unit tests (run on every OS in `ci.yml`):
- `ime.rs`: UTF-16↔byte conversion for ASCII, BMP multi-byte (`é`), astral chars (emoji, surrogate
  pairs), offsets inside a pair, out-of-range offsets; `apply_ime` with selection, composing, empty
  and inverted composing, NUL stripped with offsets shifted; `ime_state` round-trip with `apply_ime`.
- `ImeSync`: no outgoing after `incoming` of the same state; outgoing after a tap/paste/clear
  changes the field; unchanged field yields nothing; latest of several incoming states wins.
- `field.rs`: every non-IME mutator clears `composing`; `apply_ime` sets it.
- Swipe-typing scenario as a sequence: compose a word, replace it from a suggestion, commit, then a
  tap-driven `move_to` resets composing.

CI (`android.yml`) compiles the Java, dexes it and checks the exported JNI symbols while building
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

## Open Questions

- Whether `showSoftInput` on the 1×1 view reliably shows the keyboard on every target (API 26–34)
  while NativeActivity holds the window's input queue; confirm in Manual Verification step 1 and
  fall back to `InputMethodManager.toggleSoftInput` only if it does not.
