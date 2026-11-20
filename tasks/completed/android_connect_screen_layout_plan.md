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
  `Imgui::display_size()`; `ui::draw` (`ui.rs:88`) places one window at `(0,0)` covering the whole
  display, so nothing keeps content out of the status/navigation bars or a display cutout.
- `Imgui::new` (`mod.rs:182`) uses `igStyleColorsDark` plus `ScaleAllSizes(scale)`; default window
  padding is 8 px at scale 1, so margins are tiny and the default font is small.
- `ui::scan` (`ui.rs:162`) is two `text` lines, an "Allow camera" button only while the camera is
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

- `Insets { top, left, right, bottom }` in physical pixels (the unit of ImGui coordinates here),
  plus `Insets::ZERO`.
- `Insets::clamped(self, display: (f32, f32))`: negative values become 0, and each inset but the bottom is capped
  at 40 % of the display on its axis (the bottom has only the axis total below, so a soft keyboard fits in landscape too) so a bad value
  from the platform never collapses the window. Opposing insets also total at most 80 % of the axis
  (the bottom, then the right, is reduced), so `safe_rect` keeps at least 20 % of each axis and never
  returns a 0 size, which `SetNextWindowSize` treats as auto-fit.
- `Insets::from_content_rect(left, top, right, bottom, display)`: takes plain `i32` values and the
  display size; `render` converts from `AndroidApp::content_rect()` so `layout.rs` imports no
  android-activity types.
- `safe_rect(display, insets) -> (pos, size)`: the window rectangle the UI draws into.
- `margin(scale) -> f32`: the outer content margin, `16.0 * scale`.

Reading the insets, in `jni_bridge.rs`:

- `system_insets(app) -> Option<RawInsets>` (raw `i32` top/left/right/bottom, no display size needed): `activity.getWindow().getDecorView().getRootWindowInsets()`;
  null (not attached yet) returns `None`. The API level comes from `AndroidApp::sdk_version()` (no new JNI call). On API 30+ call `getInsets(systemBars() | displayCutout() | ime())`
  with `WindowInsets$Type`; below 30 use `getSystemWindowInsetTop/Left/Right/Bottom`. The keyboard
  is part of the bottom inset because `NativeActivity`'s surface does not shrink under `adjustResize`.
- `Activity` keeps `insets: Option<RawInsets>` and `insets_read: Instant`, stores the raw values as read (no display-size conversion at read time), and refreshes it each frame at most every 500 ms and on every
  `MainEvent::WindowResized`, `ContentRectChanged` and `Resume` (cheap JNI call, but not per frame).
  The conversion happens in `render`, where the display size is valid after `new_frame`: it calls
  `app.content_rect()` and `Insets::from_content_rect(rect, Imgui::display_size())` (`left/top` as the
  inset, display minus `right/bottom` as the opposite insets). On API 30+ with a read the raw insets are used
  as is; below 30 the bottom is the larger of the raw bottom and the `content_rect` bottom inset; until the first successful read
  (`None`) the `content_rect` insets are used alone. An empty
  rect (`right == 0` or `bottom == 0`, which is what `android-activity` reports before the first
  `onContentRectChanged`) gives `Insets::ZERO`, not the whole display.
- `render` then applies `Insets::clamped` once per frame with that same frame's `Imgui::display_size()`
  (the display size may be unset before the first frame or stale after rotation at read time, hence no conversion there).
- `ui::draw` takes the clamped `Insets` and `margin`; it positions the window at the safe rect and sets
  `WindowPadding = (margin, margin)` for the frame, so everything inside is inset from the bars and
  the screen edge. The clear color still fills the whole surface, so the bars sit on the app
  background.

### Style and hierarchy

In `Imgui::new` (`mod.rs`), after `igStyleColorsDark`, set once, before `ScaleAllSizes`:
`FrameRounding = 8`, `FramePadding = (14, 12)`, `ItemSpacing = (10, 12)`,
`FrameBorderSize = 1`, `ScrollbarSize = 14`, a visible `Border` color for the field outline, and a distinct
`Button`/`ButtonHovered`/`ButtonActive` accent. `ScaleAllSizes(scale)` then scales them. Also set
`FontSizeBase = 18.0` (up from the default 13 px; in dp, since `FontScaleDpi` applies the density) so body text is not tiny; headings use `FontSizeBase * 1.4` pushed with `igPushFont(null, size)` in `ui.rs`.
Also set `io.MouseDragThreshold` to Android touch slop (8 dp x scale): ImGui's default 6 physical px is not
scaled by `ScaleAllSizes`, so finger jitter would cancel both tap and long-press.
The monospace look is the bundled ImGui default font and stays.

In `ui.rs`, small helpers keep the screen code declarative: `heading(text)` (larger, bright),
`caption(text)` (dim, wrapped), `gap(px)` (`igDummy`), `primary_button(label)` and
`secondary_button(label)` (full width, the global `FramePadding` unchanged, accent vs. neutral
`Button` color pushed and popped), and `field_frame(field, hint, taps)` (below).

### Scan screen

Layout, top to bottom, all inside the safe rect with the outer margin:

1. Notice, when present (unchanged colors), with a `gap`.
2. `heading("Connect to a Fontaine host")`, then a `caption` line with the one-sentence action.
3. A scan card: a child window with border (`ImGuiChildFlags_Borders`), square, sized by
   `card_side(avail_width, avail_height, below_px, min_side)` where `avail_height` is the height
   remaining after the notice, heading and caption, `below_px` is the height of the paste section
   (computed from style metrics: caption line height + one-line `field_frame` height +
   `primary_button` height + 3 * `ItemSpacing.y`) and the caller passes `8 * igGetTextLineHeight()`
   as `min_side`; the side is `min(avail_width, max(min_side, avail_height - below_px))`, floored at 0,
   so the field stays visible in landscape ("Use this" may need scrolling the window). Inside, centered:
   - camera live: a "Scanning" label with a pulsing dot (alpha from `igGetTime()`), and the line
     "Point the camera at the code in the terminal.";
   - not live (keyed on `!camera_ready`, the camera not running): when `camera_denied` is set,
     "Camera access is off" and the denied wording reworded to say "hold the field below and tap Paste" in place of the removed Paste button; otherwise (e.g. between the Allow tap
     and the permission result) a neutral "Camera is not running" line. Either way a
     `primary_button("Allow camera")` inside the card.
   No camera image is drawn (Decisions).
4. `caption("Or paste a pairing link or ticket")` as the section label.
5. `field_frame`: a full-width frame drawn with `igBeginChild` using `ImGuiChildFlags_FrameStyle`
   (applies `FrameBg`, `FrameRounding`, `FrameBorderSize` and `FramePadding`) and
   `ImGuiWindowFlags_NoScrollbar`. It holds the typed text, wrapped, or the dim hint "type here", and its height is `igCalcTextSize` of the text at the wrap width plus
   `2 * FramePadding.y`, so a pasted pairing link (~130-300 characters) or a multi-line session
   prompt is fully visible. A tap pushes `Tap::FocusField` on release (`igIsMouseReleased_Nil(0)` for a press that started on the
   field, with no long-press latched and no drag, the release-frame no-drag test being `!igIsMouseDragPastThreshold(0, -1.0)` since `igIsMouseDragging` is false once the button is up), so a long-press does not open the soft keyboard at press start.
   Holding it is detected by latching the press when `igIsItemClicked(0)` fires on the frame; the "press started on field" and
   "long-press latched" state is derived each frame from ImGui state (`io.MouseClickedPos[0]` inside the field rect; `igIsPopupOpen_Str`
   or `io.MouseDownDurationPrev[0] >= 0.5` on the release frame), with no field in `UiState`; on later frames,
   once `igIsMouseDown_Nil(0)`, `io.MouseDownDuration[0] >= 0.5` and `!igIsMouseDragging(0, -1)` all hold, it opens a
   popup at the press point with a single "Paste" entry drawn with `igButton` (so the release that ends the long press cannot trigger it; only a fresh tap inside the popup does), like other Android apps; choosing it pushes `Tap::PasteField`, and a tap
   anywhere else dismisses it. There is no separate Paste button on either screen. Session's prompt field uses `field_frame` too
   (`field_box` is removed).
6. `primary_button("Use this")`, shown only when the field is non-empty, full width.
7. When paired hosts exist: `caption("Paired hosts")` and one `secondary_button` per host.

The main window scrolls when the content overflows, through its scrollbar only: the app feeds ImGui just
the pointer position and left button (no wheel), and dragging content on a `NoMove` window does not
scroll, so there is no touch drag-to-scroll. `ScrollbarSize` is widened to 14 dp in `Imgui::new` so the bar is a usable touch target.

Other screens (Confirm, Pairing, Error, Session) get the safe rect and margin for free; Confirm and
Pairing use `heading` for their first line and `primary_button`/`secondary_button`. The session
transcript child height is `igGetContentRegionAvail().y` at the child's position minus the footer height measured on the
previous frame, clamped to a minimum of 3 text line heights (ImGui treats height 0 as fill-remaining
and negative as remaining minus |h|, which would push the footer off screen); the window scrolls
when the footer then overflows. The footer is everything `session` draws after the transcript child (separator,
`dialog_panel` with its wrapped text, diff lines and buttons, separator, prompt field and button
rows); it is measured as the cursor Y delta across drawing it and kept in a small `UiState { footer_px: f32 }`
that the `Activity` holds and passes to `ui::draw`. `footer_px == 0.0` marks the first frame
and triggers a one-line-per-row estimate. The dialog is not moved into a capped child.

## Mockups

- `tasks/completed/android_connect_screen_layout_mockups/scan-screen.html`: scan screen with the camera live and the long-press Paste popup, and with the camera denied and a wrapped pasted link.
- `tasks/completed/android_connect_screen_layout_mockups/session-screen.html`: session screen idle, with an approval showing a diff, and reconnecting with a wrapped message.

## Implementation Steps

1. Add `rust/fontaine-android/src/layout.rs` with `Insets`, `Insets::ZERO`, `Insets::clamped`, `Insets::from_content_rect`, `safe_rect`, `margin`, `card_side`
   (with its `min_side` parameter) and unit tests; declare `pub mod layout;` in `lib.rs`.
2. Add `system_insets` (raw values only, no content_rect conversion) to `jni_bridge.rs`, update the module doc at `jni_bridge.rs:1-2` to name the clipboard and the window insets, and reword the doc comment at `jni_bridge.rs:75-76` (clipboard served "when the user has just tapped the button that asks") to refer to the Paste entry of the field's long-press popup.
3. In `mod.rs`: add `insets: Option<RawInsets>` and `insets_read: Instant` (the 500 ms throttle) and a `UiState` to `Activity`, refresh the raw insets per the cadence above, and in `render` convert via `content_rect()`/`from_content_rect` with the frame's display size (pre-API-30 bottom max, no-read fallback) then `clamped`; pass the result and `UiState`, plus `self.scale` (or a precomputed margin, as `ui::draw` needs `margin(scale)`), to `render`
   and `ui::draw`; set the style fields and `io.MouseDragThreshold` (touch slop) in `Imgui::new`.
4. In `ui.rs`: apply the safe rect and window padding in `draw`; add the helpers; rewrite `scan`
   with the card and framed field; in `session`, replace `field_box` with `field_frame` (with the long-press Paste popup),
   remove the Paste button, and delete `field_box`; Send (`primary_button`) and Interrupt (`secondary_button`) split the
   session row in halves, with "Leave session" full width below; use `heading` and the button helpers on Confirm and Pairing, only the button helpers on Error;
   size the session transcript child as `igGetContentRegionAvail().y` at the child's position minus `UiState.footer_px` (clamped to at least 3 text line heights;
   `footer_px == 0.0` means first frame, use the one-line-per-row estimate), the
   footer height (everything drawn after the transcript) measured on the previous frame; update
   `footer_px` from the cursor Y before and after drawing the footer; drop `session`'s `display` parameter, which becomes unused.
5. In `docs/android.md`, add a scan-screen description to the Pair section, and change lines 40-41's
   "tap "Paste" on the scan screen" to "hold the field and tap Paste".
6. Add the Decisions lines as `- 2026-11-xx (#721):` lines at the end of the `### Build and release`
   subsection of `tasks/fontaine_harness_plan.md` Decisions (where the Android lines live), plus a
   `CHANGELOG.md` `[Unreleased]` "Fixed" entry.
7. Run the Android NDK clippy as CI does (`.github/workflows/android.yml`) plus the host checks.

## Files to Modify

- `rust/fontaine-android/src/layout.rs` (new), `src/lib.rs`
- `rust/fontaine-android/src/android/jni_bridge.rs`, `mod.rs`, `ui.rs`
- `docs/android.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- Insets via `AndroidManifest`/theme (`fitsSystemWindows`) vs. in-app: the activity has no Java code
  (`hasCode=false`), so a theme cannot be authored without a resource build step.

## Decisions

- The scan screen shows a scan card with the camera's state, not a live camera image. Reason: the
  camera pipeline deliberately shows and stores nothing, a viewfinder needs a YUV-to-GL texture path
  that cannot be verified without a device, and the issue accepts "a clear Scan button" as the
  alternative; the viewfinder stays a follow-up.
- Insets are re-read on resume, resize and every 500 ms rather than every frame.
- The keyboard inset is read (`ime()` on API 30+, `content_rect` bottom below) because
  `NativeActivity`'s surface and ImGui display size do not shrink under `adjustResize`.
- AFK: touch overflow relies on the (widened, `ScrollbarSize` 14 dp) scrollbar only; touch drag-to-scroll is not added (smaller design surface; the app sends no wheel events).
- AFK: `Insets::clamped` also caps opposing insets at 80 % of the axis total so `safe_rect` never collapses to 0 (auto-fit), with a unit test.
- AFK: the `content_rect` to inset conversion, the pre-API-30 bottom max and the no-read fallback run in `render` with that frame's display size; `Activity` and `system_insets` hold only raw JNI insets.
- JNI window insets are primary and `content_rect` the fallback; if a device shows `content_rect`
  alone is correct, drop the JNI path in a follow-up.

## Documentation

`docs/android.md`: describe the scan screen (scan card, paste field, paired hosts) in the Pair
section, and change the "tap Paste" instructions (Pair section, lines 40-41) to "hold the field and tap Paste". No new page, so `SUMMARY.md` and `index.md` do not change.

## Testing Strategy

- Unit tests in `layout.rs`: negative insets clamp to zero; an oversize inset is capped at 40 % of
  the axis (a keyboard-height bottom inset, portrait or landscape, survives up to the axis total); oversized opposing insets (e.g. 40 % top + 60 % bottom, likewise left/right) leave `safe_rect` a positive size of at least 20 % of the axis; `card_side` follows `min(avail_width, max(min_side, avail_height - below_px))` floored at 0, tested with an 8-line-height `min_side` and with width-limited, height-limited (landscape), min-side and tiny-display cases; `safe_rect` of zero insets is the full display; `safe_rect` with a 120 px top inset and
  a 48 px bottom inset on 1080x2400 starts at y = 120 with height 2232; `Insets::from_content_rect` of an all-zero rect is `Insets::ZERO`, and of a real rect gives
  left/top and display minus right/bottom.
- `ui.rs`/`jni_bridge.rs` are Android-only FFI and compile-checked by the NDK clippy in `android.yml`.

## Manual Verification

1. Install the PR's debug APK (`android.yml` artifact) on a phone with a status-bar cutout, open
   the app: the heading sits fully below the clock and icons, with equal side margins; expected
   because the safe rect is applied. Not automated: it needs a device and a real window.
2. With the camera permission off: the card shows "Camera access is off" and "Allow camera"; the paste
   field has an outline. Long-press it: a "Paste" popup appears at the finger and pastes the clipboard text; a short tap
   opens the keyboard. Rotate to landscape: the paste field is visible without scrolling.
3. With the soft keyboard up, type in the session prompt field: the window's bottom edge sits at the
   top of the keyboard, so the prompt field and footer stay visible above it.
4. Pair from a host QR: the live state reads "Scanning" and the flow is unchanged.

## Open Questions

None.
