# Android Connect Screen Layout Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/721

## Overview

The Android app's scan/paste screen draws under the status bar, has no spacing or hierarchy, shows
no scan affordance and uses bare full-width bars for the paste field and button. This plan applies
the system-bar and cutout insets to every screen, adds margins and a type hierarchy, replaces the
scan text with a framed scan card that states whether the camera is live, and styles the paste
field and buttons as controls. The pairing flow, `App` state machine and camera pipeline do not
change.

## Current State

- `rust/fontaine-android/src/android/mod.rs` `render` (`:245`) calls `ui::draw` with
  `Imgui::display_size()`; `ui::draw` (`ui.rs:80`) places one window at `(0,0)` covering the whole
  display, so nothing keeps content out of the status/navigation bars or a display cutout.
- `Imgui::new` (`mod.rs:182`) uses `igStyleColorsDark` plus `ScaleAllSizes(scale)`; default window
  padding is 8 px at scale 1, so margins are tiny and the default font is small.
- `ui::scan` (`ui.rs:152`) is two `text` lines, an "Allow camera" button only while the camera is
  not ready, a separator, and `field_box`, which is a bare colored text line with no frame.
  Buttons are `igButton` with width `avail_width()`.
- The camera feeds `rqrr` only; `docs/android.md` and `camera.rs` state no frame is shown or stored.
- `jni_bridge.rs` has `with_activity` and one helper per Java call; no insets call exists.
  `AndroidManifest.xml` sets `adjustResize`; `targetSdkVersion` is 34, so the app is not forced
  edge-to-edge, yet the issue's screenshot shows content under the bar (NativeActivity's surface
  covers the status bar on that device), so insets must be read rather than assumed.
- Portable (host-tested) modules: `app.rs`, `model.rs`, `link.rs`, `scan.rs`, `store.rs`, `keys.rs`.
  `ui.rs` and `mod.rs` compile only for Android and are covered by the NDK clippy job.

## Design

### Insets

New portable module `rust/fontaine-android/src/layout.rs` (host-tested):

- `Insets { top, left, right, bottom }` in physical pixels, plus `Insets::ZERO`.
- `Insets::clamped(self, display: (f32, f32))`: negative values become 0, and each axis is capped
  at 40 % of the display on that axis so a bad value from the platform never collapses the window.
- `Insets::to_logical(self)` is not needed: ImGui coordinates here are physical pixels already.
- `safe_rect(display, insets) -> (pos, size)`: the window rectangle the UI draws into.
- `margin(scale) -> f32`: the outer content margin, `16.0 * scale`.

Reading the insets, in `jni_bridge.rs`:

- `system_insets(app) -> Option<Insets>`: `activity.getWindow().getDecorView().getRootWindowInsets()`;
  null (not attached yet) returns `None`. On API 30+ call `getInsets(systemBars() | displayCutout())`
  with `WindowInsets$Type`; below 30 use `getSystemWindowInsetTop/Left/Right/Bottom`. When the soft
  keyboard is up, `adjustResize` already shrinks the surface, so the bottom inset stays the
  navigation bar only (the IME type is deliberately not requested).
- `Activity` keeps `insets: Insets` and refreshes it each frame at most every 500 ms and on every
  `MainEvent::WindowResized`, `ContentRectChanged` and `Resume` (cheap JNI call, but not per frame).
  Until the first successful read, fall back to `android.content_rect()` (`left/top` as the inset,
  display minus `right/bottom` as the opposite insets).
- `ui::draw` takes the `Insets` and `margin`; it positions the window at the safe rect and sets
  `WindowPadding = (margin, margin)` for the frame, so everything inside is inset from the bars and
  the screen edge. The clear color still fills the whole surface, so the bars sit on the app
  background.

### Style and hierarchy

In `Imgui::new` (`mod.rs`), after `igStyleColorsDark`, set once, before `ScaleAllSizes`:
`FrameRounding = 8`, `WindowRounding = 0`, `FramePadding = (14, 12)`, `ItemSpacing = (10, 12)`,
`FrameBorderSize = 1`, a visible `Border` color for the field outline, and a distinct
`Button`/`ButtonHovered`/`ButtonActive` accent. `ScaleAllSizes(scale)` then scales them. Also set
the font size through `FontSizeBase` (the field `FontScaleDpi` already uses) so body text is not
tiny; headings use `FontSizeBase * 1.4` pushed with `igPushFont(null, size)` in `ui.rs`.
The monospace look is the bundled ImGui default font and stays.

In `ui.rs`, small helpers keep the screen code declarative: `heading(text)` (larger, bright),
`caption(text)` (dim, wrapped), `gap(px)` (`igDummy`), `primary_button(label)` and
`secondary_button(label)` (full width, `FramePadding` taller than default, accent vs. neutral
`Button` color pushed and popped), and `field_frame(field, hint, taps)` (below).

### Scan screen

Layout, top to bottom, all inside the safe rect with the outer margin:

1. Notice, when present (unchanged colors), with a `gap`.
2. `heading("Connect to a Fontaine host")`, then a `caption` line with the one-sentence action.
3. A scan card: a child window with border (`ImGuiChildFlags_Borders`), square, width
   `avail_width()` capped at 75 % of the safe height. Inside, centered:
   - camera live: a "Scanning" label with a pulsing dot (alpha from `igGetTime()`), and the line
     "Point the camera at the code in the terminal.";
   - camera off, not denied: "Camera is off", and a `primary_button("Allow camera")` inside the
     card;
   - denied: "Camera access is off" and the existing denied wording, with the same button.
   This makes the scan state and its one action visible at the top of the screen. No camera image
   is drawn (Decisions).
4. `caption("Or paste a pairing link or ticket")` as the section label.
5. `field_frame`: a bordered, rounded, full-width frame (`igBeginChild` with border, height of one
   line plus frame padding) holding the typed text or the dim hint "tap Paste or type here"; a tap
   inside it pushes `Tap::FocusField` as today.
6. `secondary_button("Paste")`, and `primary_button("Use this")` only when the field is non-empty.
   Both full width, with `ItemSpacing` between.
7. When paired hosts exist: `caption("Paired hosts")` and one `secondary_button` per host.

The scrollable content is wrapped in a child window sized to the remaining height so a short or
landscape display scrolls instead of clipping.

Other screens (Confirm, Pairing, Error, Session) get the safe rect and margin for free; Confirm and
Pairing use `heading` for their first line and `primary_button`/`secondary_button`. The session
layout's `display.1 - footer - …` height math uses the safe height, not the raw display height.

### Where the testable logic lives

`layout.rs` holds only arithmetic (clamping, safe rect, margin, card side length
`card_side(avail_width, avail_height)`), so it is host-tested; `ui.rs` and `jni_bridge.rs` stay thin
wrappers over unsafe calls with no logic worth a test.

## Implementation Steps

1. Add `rust/fontaine-android/src/layout.rs` with `Insets`, `safe_rect`, `margin`, `card_side` and
   unit tests; declare `pub mod layout;` in `lib.rs`.
2. Add `system_insets` to `jni_bridge.rs`.
3. In `mod.rs`: add `insets` to `Activity`, refresh it per the cadence above, pass it to `render`
   and `ui::draw`; set the style fields in `Imgui::new`.
4. In `ui.rs`: apply the safe rect and window padding in `draw`; add the helpers; rewrite `scan`
   with the card and framed field; use `heading` and the button helpers on Confirm, Pairing, Error;
   switch the session height math to the safe height.
5. Update `docs/android.md` (Pair section and the scan-screen description).
6. Add the Decisions lines to `tasks/fontaine_harness_plan.md` and a `CHANGELOG.md` entry.
7. Run the Android NDK clippy as CI does (`.github/workflows/android.yml`) plus the host checks.

## Files to Modify

- `rust/fontaine-android/src/layout.rs` (new), `src/lib.rs`
- `rust/fontaine-android/src/android/jni_bridge.rs`, `mod.rs`, `ui.rs`
- `docs/android.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- Insets via `getRootWindowInsets` vs. `AndroidApp::content_rect()` alone: `content_rect` is the
  fallback only because on some devices and API levels it reports the full window when the surface
  is drawn behind the bars, which is the reported bug.
- Insets via `AndroidManifest`/theme (`fitsSystemWindows`) vs. in-app: the activity has no Java code
  (`hasCode=false`), so a theme cannot be authored without a resource build step.
- Live viewfinder vs. a status card: see Decisions.

## Decisions

- The scan screen shows a scan card with the camera's state, not a live camera image. Reason: the
  camera pipeline deliberately shows and stores nothing, a viewfinder needs a YUV-to-GL texture path
  that cannot be verified without a device, and the issue accepts "a clear Scan button" as the
  alternative; the viewfinder stays a follow-up.
- Insets are re-read on resume, resize and every 500 ms rather than every frame, and the IME inset is
  not requested because `adjustResize` already shrinks the surface.

## Documentation

`docs/android.md`: describe the scan screen (scan card, paste field, paired hosts) in the Pair
section. No new page, so `SUMMARY.md` and `index.md` do not change.

## Testing Strategy

- Unit tests in `layout.rs`: negative insets clamp to zero; an oversize inset is capped at 40 % of
  the axis; `safe_rect` of zero insets is the full display; `safe_rect` with a 120 px top inset and
  a 48 px bottom inset on 1080x2400 starts at y = 120 with height 2232; `card_side` is limited by
  both the width and 75 % of the height and never negative on a tiny display. The status-bar bug in
  the issue is pinned by the `safe_rect` case.
- `ui.rs`/`jni_bridge.rs` are Android-only FFI and compile-checked by the NDK clippy in `android.yml`.

## Manual Verification

1. Install the PR's debug APK (`android.yml` artifact) on a phone with a status-bar cutout, open
   the app: the heading sits fully below the clock and icons, with equal side margins; expected
   because the safe rect is applied. Not automated: it needs a device and a real window.
2. With the camera permission off: the card shows "Camera is off" and "Allow camera"; the paste
   field has an outline and the Paste button has margin. Rotate to landscape: content scrolls.
3. Pair from a host QR: the live state reads "Scanning" and the flow is unchanged.

## Open Questions

None.
