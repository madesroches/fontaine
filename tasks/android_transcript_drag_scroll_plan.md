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
  two lines of the bottom and `frame.scroll_to_end` is set. A slow drag or fling that starts near
  the bottom would be snapped back every frame, so the follow must be skipped while `DragScroll`
  is active (see Design).
- Transcript rows are plain text (`colored`/`wrapped`), so there is no text selection to conflict
  with. The only interactive row is the `reasoning` toggle button (`entry_row`, `styled_button`),
  which fires on release while hovered, so a drag that starts or ends on it would toggle it.
  Selection and long press exist only in `field_frame` (the prompt), outside the transcript.
- Portable modules (`layout.rs`, `field.rs`, `keys.rs`, ...) are declared in `lib.rs`; only
  `android/` is `cfg(target_os = "android")`.

## Design

### Portable: `rust/fontaine-android/src/scroll.rs`

A small state machine with no ImGui types, time passed in as `f32` seconds (`now` from
`igGetTime()`, `dt` from the io `DeltaTime`):

```
pub struct DragScroll { /* phase, last_y, samples ring, velocity */ }
pub enum Phase { Idle, Dragging, Flinging }

impl DragScroll {
    /// A press began over the transcript (caller decides that). Clears any running fling.
    pub fn press(&mut self, y: f32, now: f32);
    /// Pointer moved while held; returns pixels to scroll by (content moves with finger,
    /// so scroll offset change = -(y - last_y)). Returns 0 until `slop` is passed, then the
    /// whole accumulated movement so the content does not jump.
    pub fn drag(&mut self, y: f32, now: f32, slop: f32) -> f32;
    /// Finger lifted. Starts a fling if the velocity over the last ~100 ms exceeds `MIN_FLING`.
    pub fn release(&mut self, now: f32);
    /// Advances a fling by `dt`; returns the scroll delta, stops below `STOP_SPEED`.
    pub fn step(&mut self, dt: f32) -> f32;
    /// Stops a fling (content edge reached).
    pub fn cancel(&mut self);
    pub fn is_dragging(&self) -> bool;
    /// True while the phase is `Dragging` or `Flinging`.
    pub fn is_active(&self) -> bool;
}
```

- Velocity: least-effort estimate from the samples within the last 100 ms (distance / time), so a
  pause before lift-off yields no fling.
- Fling: exponential decay, `v *= exp(-FRICTION * dt)` with `FRICTION` about 4.0/s; the step
  delta is the closed form `v/FRICTION*(1-exp(-FRICTION*dt))`, so travel over a given total
  duration does not depend on step size; stops under a
  few px/s; velocity clamped to a max (a few thousand px/s scaled by `scale`).
- Constants that are physical pixels (min fling, stop speed, max speed) take a `scale` factor from
  the display density passed per call, as `TOUCH_SLOP_DP` is scaled.

### Android layer: `android/ui.rs`

In `session()`, around the transcript child, with `UiState` gaining a `transcript_scroll:
DragScroll` and the frame time from `igGetIO().DeltaTime`:

1. Before `igBeginChild_Str`, record `igGetActiveID()`; nonzero means another widget (the prompt
   field, say) owns any press this frame. Inside the child (after the rows), if the mouse was
   pressed this frame (`igIsMouseClicked_Bool(0, false)`), that recorded ID was zero, and
   `igIsWindowHovered` with `ImGuiHoveredFlags_AllowWhenBlockedByActiveItem` (a reasoning button is
   already the active item on the press frame) and the press is not on the scrollbar -> `press`.
2. While down and the press began in the transcript: call `drag` with `igGetMousePos().y` and
   the slop (the machine owns the slop; no `igIsMouseDragging` gate) and apply the result with
   `igSetScrollY(igGetScrollY() + delta)` (ImGui clamps to `[0, ScrollMaxY]`).
3. On `igIsMouseReleased_Nil(0)` call `release`; every frame without the button down call `step`
   and apply it; `cancel` the fling when `igGetScrollY()` equals last frame's value while
   flinging (`igSetScrollY` is deferred, so this is the clamped result of the previous apply).
4. The tail-follow `igSetScrollHereY` runs after this, still only when near the bottom, and is
   skipped while `is_active()`; a fling that ends at the bottom resumes following.
5. The scroll bar keeps working. The child's scrollbar is submitted in `igEndChild`, so a grab is
   not visible after the rows; test the press position against the scrollbar rect (a strip of
   `ScrollbarSize` at the child's right edge when `ScrollMaxY > 0`), captured after `igEndChild`
   or from the previous frame, and do not `press` inside it.

### Tap and selection conflicts

- Reasoning toggle: suppress the toggle when the press became a drag. In `entry_row` the toggle
  condition becomes `clicked && !igIsMouseDragPastThreshold(0, -1.0)` (the release frame is still
  past-threshold; `igIsMouseDragging` is false once the button is up, as the field code already
  notes). A press that starts on the button and drags still scrolls, since the press is detected
  by `igIsWindowHovered` with `AllowWhenBlockedByActiveItem`, not by item hover.
- A press that starts in the transcript and is released in the footer (or vice versa) never
  reaches the field or its long-press logic: the state machine is only entered from a press whose
  start position was inside the transcript child, and `field_frame` only begins a selection from a
  press that began over the field.
- No text selection exists in the transcript today; this plan does not add one.

## Implementation Steps

1. Add `scroll.rs` with `DragScroll` and its unit tests; declare `pub mod scroll;` in `lib.rs`.
2. Add `transcript_scroll` to `UiState` (`android/ui.rs`). `UiState` derives `Default`, so make
   `DragScroll: Default` and pass the display scale to it as an argument per call. `Frame` has no
   scale (only `margin`): add a `scale: f32` field to `Frame` and fill it from `params.scale` in
   `render()` in `android/mod.rs`.
3. Wire press/drag/release/step in `session()` as above; suppress the reasoning toggle on drag.
4. `docs/android.md`: add a line that the transcript scrolls by dragging, with fling, and the
   scroll bar remains.
5. `CHANGELOG.md` entry (done by the `pr` step).

## Files to Modify

- `rust/fontaine-android/src/scroll.rs` (new)
- `rust/fontaine-android/src/lib.rs`
- `rust/fontaine-android/src/android/ui.rs`
- `rust/fontaine-android/src/android/mod.rs` (fill `Frame.scale` in `render()`)
- `docs/android.md`
- `CHANGELOG.md`

## Decisions

- ImGui-only config rejected: Dear ImGui has no built-in touch-drag scrolling for child windows, hence the small state machine.
- Platform `VelocityTracker` over JNI rejected: cannot be unit-tested and adds a bridge call per event.
- Linear scroll without fling rejected: the issue asks for momentum.

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
- `is_active()` is true while dragging and flinging, false when idle.
- frame-rate independence: with the closed-form step delta, stepping 60 x 16 ms and 30 x 32 ms
  (equal total duration) gives the same travel within a small tolerance.

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
