# Android Transcript Drag Scroll Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/775

## Overview

In the Android app the transcript scrolls only through ImGui's scroll bar. Make a touch drag on
the transcript scroll it, with fling momentum after release, like any mobile list. The scroll bar
stays. The gesture math (velocity estimate, fling decay) lives in the portable part of the crate so
it is unit-tested on every OS; the Android layer only feeds it ImGui's pointer state.

## Current State

- `rust/fontaine-android/src/android/ui.rs` `session()` draws the transcript as
  `igBeginChild_Str("transcript", ..., 0, 0)`. Dear ImGui scrolls a child by wheel or scroll bar
  only; a finger drag does nothing.
- Touch reaches ImGui as a mouse: `android/mod.rs` `input()` maps `MotionEvent` pointer 0 to
  `Imgui::mouse` (pos + left button down/up). `Imgui::new` sets `MouseDragThreshold` to
  `TOUCH_SLOP_DP * scale`, so `igIsMouseDragging(0, -1.0)` is the touch-slop test.
- Tail-following: after the rows, `session()` calls `igSetScrollHereY(1.0)` when the view is within
  two lines of the bottom and `frame.scroll_to_end` is set. A user who drags up must not be pulled
  back down; this already holds because the check is "was near the bottom".
- Transcript rows are plain text (`colored`/`wrapped`), so there is no text selection to conflict
  with. The only interactive row is the `reasoning` toggle button (`entry_row`, `styled_button`),
  which fires on release while hovered, so a drag that starts or ends on it would toggle it.
  Selection and long press exist only in `field_frame` (the prompt), outside the transcript.
- Portable modules (`layout.rs`, `field.rs`, `keys.rs`, ...) are declared in `lib.rs`; only
  `android/` is `cfg(target_os = "android")`.

## Design

### Portable: `rust/fontaine-android/src/scroll.rs`

A small state machine with no ImGui types, time passed in as `f32` seconds:

```
pub struct DragScroll { /* phase, last_y, samples ring, velocity */ }
pub enum Phase { Idle, Dragging, Flinging }

impl DragScroll {
    /// A press began over the transcript (caller decides that).
    pub fn press(&mut self, y: f32, now: f32);
    /// Pointer moved while held; returns pixels to scroll by (content moves with finger,
    /// so scroll offset change = -(y - last_y)). Returns 0 until `slop` is passed, then the
    /// whole accumulated movement so the content does not jump.
    pub fn drag(&mut self, y: f32, now: f32, slop: f32) -> f32;
    /// Finger lifted. Starts a fling if the velocity over the last ~100 ms exceeds `MIN_FLING`.
    pub fn release(&mut self, now: f32);
    /// Advances a fling by `dt`; returns the scroll delta, stops below `STOP_SPEED`.
    pub fn step(&mut self, dt: f32) -> f32;
    /// Pressing during a fling stops it; edge of the content stops it (`hit_edge`).
    pub fn cancel(&mut self);
    pub fn is_dragging(&self) -> bool;
}
```

- Velocity: least-effort estimate from the samples within the last 100 ms (distance / time), so a
  pause before lift-off yields no fling.
- Fling: exponential decay, `v *= exp(-FRICTION * dt)` with `FRICTION` about 4.0/s; stops under a
  few px/s; velocity clamped to a max (a few thousand px/s scaled by `scale`).
- Constants that are physical pixels (min fling, stop speed, max speed) take a `scale` factor from
  the display density passed at construction, as `TOUCH_SLOP_DP` does.

### Android layer: `android/ui.rs`

In `session()`, around the transcript child, with `UiState` gaining a `transcript_scroll:
DragScroll` and the frame time from `igGetIO().DeltaTime`:

1. Inside the child (after the rows), if the mouse was pressed this frame
   (`igIsMouseClicked_Bool(0, false)`) and `igIsWindowHovered` (child, no popup/blocked) -> `press`,
   else-if a fling is running and the press lands -> `cancel` and treat as press.
2. While down and the press began in the transcript: on `igIsMouseDragging(0, -1.0)` call `drag`
   with `igGetMousePos().y` and apply the result with `igSetScrollY(igGetScrollY() + delta)`
   (ImGui clamps to `[0, ScrollMaxY]`).
3. On `igIsMouseReleased_Nil(0)` call `release`; every frame without the button down call `step`
   and apply it; stop the fling when the clamped scroll did not move.
4. The tail-follow `igSetScrollHereY` runs after this and still only when near the bottom, so a
   fling that ends at the bottom resumes following.
5. The scroll bar keeps working; dragging the bar sets the child's active id, so the drag state
   machine ignores presses where `igIsAnyItemActive()` / the scrollbar is hovered (check
   `igGetActiveID()` is zero at press, which is false while the bar is grabbed).

### Tap and selection conflicts

- Reasoning toggle: suppress the toggle when the press became a drag. In `entry_row` the toggle
  condition becomes `clicked && !igIsMouseDragPastThreshold(0, -1.0)` (the release frame is still
  past-threshold; `igIsMouseDragging` is false once the button is up, as the field code already
  notes). A press that starts on the button and drags still scrolls, since the press is detected
  by window hover, not by item.
- A press that starts in the transcript and is released in the footer (or vice versa) never
  reaches the field or its long-press logic: the state machine is only entered from a press whose
  start position was inside the transcript child, and `field_frame` only begins a selection from a
  press that began over the field.
- No text selection exists in the transcript today; this plan does not add one.

## Implementation Steps

1. Add `scroll.rs` with `DragScroll` and its unit tests; declare `pub mod scroll;` in `lib.rs`.
2. Add `transcript_scroll` to `UiState` (`android/ui.rs`), constructed with the display scale.
   `UiState` derives `Default`, so make `DragScroll: Default` with a `with_scale` setter called
   when the surface is created (`android/mod.rs`), or hold the scale in `Frame`.
3. Wire press/drag/release/step in `session()` as above; suppress the reasoning toggle on drag.
4. `docs/android.md`: add a line that the transcript scrolls by dragging, with fling, and the
   scroll bar remains.
5. `CHANGELOG.md` entry (done by the `pr` step).

## Files to Modify

- `rust/fontaine-android/src/scroll.rs` (new)
- `rust/fontaine-android/src/lib.rs`
- `rust/fontaine-android/src/android/ui.rs`
- `rust/fontaine-android/src/android/mod.rs` (scale plumbing only, if needed)
- `docs/android.md`
- `CHANGELOG.md`

## Trade-offs

- **ImGui-only (use `ImGuiWindowFlags` / io config)**: Dear ImGui has no built-in touch-drag
  scrolling for child windows; its docs point at doing it by hand. Hence the small state machine.
- **Handling raw `MotionEvent` with a platform `VelocityTracker` over JNI**: gives platform-native
  fling feel but puts the logic behind JNI where it cannot be unit-tested and adds a bridge call
  per event. Rejected for the testability requirement in the issue.
- **Linear scroll without fling**: simpler, but the issue asks for momentum ideally.

## Decisions

## Documentation

- `docs/android.md`: transcript scrolls by drag and fling; scroll bar still available.

## Testing Strategy

Unit tests in `scroll.rs` (host, every OS), written against constructed samples:

- drag below the slop returns zero and does not start dragging; crossing it returns the whole
  accumulated movement once, then per-move deltas with the right sign (finger up scrolls down).
- release after a fast flick starts a fling whose total travel is monotonic and decaying; `step`
  eventually returns zero and the phase goes `Idle`.
- release after the finger paused (>100 ms since the last move) starts no fling.
- velocity is clamped to the maximum.
- `cancel` (press during a fling) stops it immediately.
- scale factor scales the thresholds.
- frame-rate independence: stepping 60 x 16 ms and 30 x 33 ms travel within a small tolerance.

The ImGui wiring in `android/ui.rs` is Android-only (it needs a device GLES context) and is not
reachable from host tests; see Manual Verification.

## Manual Verification

On a device (or emulator) paired to a host with a long transcript, since the touch-to-ImGui path
only runs on Android:

1. Drag the transcript up/down: it follows the finger; flick: it coasts and stops.
2. Press on a `+ reasoning` row and drag: it scrolls and the row does not toggle; a plain tap
   still toggles it.
3. Scroll up while the agent streams: the view is not pulled to the bottom; scroll back to the
   bottom: it follows again.
4. The scroll bar still drags the transcript; drags in the prompt field still select text.

## Open Questions

- Fling friction and maximum velocity constants need on-device tuning; initial values above are a
  starting point.
