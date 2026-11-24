# Android Transcript Block Selection Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/776

## Overview

In the Android app a person cannot copy anything out of the transcript. Add block selection: a
long press on the transcript selects the block under the finger, dragging extends the selection
across neighbouring blocks, drag handles at both ends adjust it, and a **Copy** menu puts the
selected blocks on the system clipboard. A block is a message paragraph, a fenced code block, a
tool call with its output, a shell command with its output, and so on. The block model, the
selection range, hit-testing and the copied text live in a new portable module so they are
unit-tested on every OS; the Android layer only records block rectangles, draws, reads touch and
writes the clipboard. Character-level selection is out of scope: the issue asks for blocks.

## Current State

- `rust/fontaine-android/src/android/ui.rs` `session()` draws the transcript as
  `igBeginChild_Str("transcript", ...)` and calls `entry_row` per `Entry` (`model.rs`). Rows are
  plain `colored`/`wrapped` text (`igTextUnformatted`); nothing records where a row is, and an
  assistant message is one text item holding any markdown fences as raw text.
- `drag_scroll()` (same file) drives `DragScroll` (`scroll.rs`) from ImGui's mouse state: `press`
  on a click inside the child (not on the scroll bar, no other active widget), `drag` with the
  `MouseDragThreshold` slop, `release`/fling after. `DragScroll::abandon_press` drops a held
  touch the transcript does not want, `cancel` stops a fling. `swallow_tap` stops a tap from
  toggling a `reasoning` row.
- `field_frame()` already implements long press and drag selection for the prompt field:
  `LONG_PRESS_SECS` (0.5 s), `MouseDownDuration[0]`, `igIsMouseDragging`, a popup menu with
  buttons (not selectables, so the releasing finger cannot trigger them), and `Tap::Copy`.
- Clipboard write: `jni_bridge::set_clipboard_text(&AndroidApp, &str)`; `Android::write_clipboard`
  (`android/mod.rs`) copies the *prompt field's* selection and sets a notice on failure.
  `handle_tap` maps `Tap` values to app/field actions.
- `docs/android.md` describes prompt-field selection and transcript drag scrolling (#775).
- Portable modules are declared in `lib.rs`; only `android/` is `cfg(target_os = "android")`.

## Design

### Portable: `rust/fontaine-android/src/select.rs`

No ImGui types; positions are `f32` pixels in the transcript child's content space.

```
/// One selectable unit of the transcript.
pub struct Block { pub entry: usize, pub text: String }

/// Splits entries into blocks, in order. Entries that draw nothing (`Other`) yield none.
pub fn blocks(entries: &[Entry], open_reasoning: impl Fn(usize) -> bool) -> Vec<Block>;

/// An inclusive range of block indices; `anchor` is where the press began, `cursor` the end that
/// moves. Order-independent accessors give `start()`/`end()`.
pub struct BlockSelection { anchor: usize, cursor: usize }
impl BlockSelection {
    pub fn new(at: usize) -> Self;
    pub fn extend_to(&mut self, at: usize);          // moves `cursor`
    pub fn move_handle(&mut self, start: bool, at: usize); // a handle drag; may swap ends
    pub fn range(&self) -> RangeInclusive<usize>;
    pub fn clamp(&mut self, len: usize) -> bool;     // false when nothing selectable is left
}

/// The block whose vertical span holds `y`, else the nearest one (a press in the gap between
/// blocks, or a drag past either end, still resolves). `None` for no blocks.
pub fn block_at(spans: &[(f32, f32)], y: f32) -> Option<usize>;

/// The text to copy: the selected blocks joined by a blank line.
pub fn copy_text(blocks: &[Block], selection: &BlockSelection) -> String;
```

Block rules (what the issue names: message, code block, tool output):

- `User`, `Error`, `Notice`, `Attachment`: one block each, the text as drawn without decoration
  (no `> `/`! ` prefix is added to what is copied).
- `Assistant`: split into paragraphs on blank lines, except that a fenced code block
  (```` ``` ````-delimited, unterminated while streaming) is one block whose copied text is the
  fence body *without* the fence lines, so pasting a code block gives code. A prose paragraph is
  copied as written.
- `Tool`: one block: the summary line, the failure/denial reason if any, and the output as shown
  (the first six lines, since that is what is on screen), so what is copied is what was selected.
- `Shell`: one block: `$ command` and the output.
- `Reasoning`: a block only while its text is open (`open_reasoning`); the toggle row is not
  selectable.

Splitting lives here, and `entry_row` is changed to draw from `blocks()` output for the entries
it covers, so the drawn items and the selectable blocks cannot drift apart (DRY).

### Android layer: `android/ui.rs`, `android/mod.rs`

State in `UiState`: `block_selection: Option<BlockSelection>`, `block_spans: Vec<(f32, f32)>`
(top/bottom of each block's item rect in screen coordinates, rebuilt each frame as the rows are
submitted with `igGetItemRectMin/Max`), `viewed_for_selection` (the agent the selection belongs
to), and `handle_drag: Option<bool>` (which end a handle drag moves).

1. **Recording.** `entry_row` takes `&mut Vec<(f32, f32)>` and pushes each block's span after
   drawing it. Selected blocks get a translucent `SELECTION` rect via the child's draw list, drawn
   behind nothing (added after the item; the colour is translucent so text stays readable).
2. **Long press.** After the rows, mirror `field_frame`: a press that began over the transcript
   (the same `free`/not-on-scrollbar conditions as `drag_scroll`), still down at `LONG_PRESS_SECS`,
   not past the drag threshold -> `BlockSelection::new(block_at(spans, press_y))`, then
   `transcript_scroll.abandon_press()` so the finger no longer scrolls. Because the scroll machine
   owns the slop, a drag before 0.5 s scrolls and never selects; a press held 0.5 s then dragged
   extends the selection (`extend_to(block_at(spans, mouse_y))`) and does not scroll. This is the
   coexistence with #775.
3. **Handles.** While a selection exists, a small circle is drawn at the top-left of the first
   block and bottom-right of the last (radius a few dp, hit area `HANDLE_DP` scaled by
   `frame.scale`). A press that begins on a handle sets `handle_drag` and is not given to
   `DragScroll` (checked before the scroll press); dragging calls `move_handle`.
4. **Edge auto-scroll.** While extending or moving a handle with the finger within one line of
   the child's top/bottom edge, scroll the child by a fixed per-frame step so a selection can
   reach off-screen blocks. Decision of the speed constant is in the plan's Decisions.
5. **Copy menu.** On release of the long press (as in the field) and whenever the selection is
   shown idle, a small bar anchored above the first block shows **Copy** and **Clear** as buttons
   (not selectables, same reasoning as the field's menu). A tap elsewhere in the transcript that
   is not a scroll drag clears the selection.
6. **Copy.** Copy pushes `Tap::CopyBlocks(String)` built from `copy_text`. `handle_tap` writes it
   with a new `write_clipboard_text(&str) -> bool` helper that `write_clipboard` also uses, so
   the failure notice is not duplicated. On success the selection clears.
7. **Staleness.** The selection is cleared when the viewed agent changes, and `clamp(blocks.len())`
   runs each frame so a `Truncate` event cannot leave an index past the end. Streaming appends
   only grow the tail, so indices before it stay valid; a message that is still streaming may
   gain blocks, which a selection ending on its last block does not auto-include.
8. **Rows that must not select.** The `reasoning` toggle keeps its tap behaviour; a long press on
   it selects nothing and does not toggle (the existing `swallow_tap`/drag-past-threshold guard is
   extended to a held press that began a selection).

## Implementation Steps

1. Add `select.rs` (`Block`, `blocks`, `BlockSelection`, `block_at`, `copy_text`) with unit
   tests; declare `pub mod select;` in `lib.rs`.
2. `android/ui.rs`: make `entry_row` draw from `blocks()` and record spans; add the `UiState`
   fields; draw selection highlight and handles.
3. `android/ui.rs`: long-press start, extension, handle drag, edge auto-scroll, Copy/Clear bar,
   clearing rules; integrate with `drag_scroll` (`abandon_press`, handle presses bypass it).
4. `Tap::CopyBlocks(String)`; `android/mod.rs` `handle_tap` arm and the shared
   `write_clipboard_text`.
5. `docs/android.md`: describe long-press block selection, handles, Copy.
6. `CHANGELOG.md` entry (done by the `pr` step).

## Files to Modify

- `rust/fontaine-android/src/select.rs` (new)
- `rust/fontaine-android/src/lib.rs`
- `rust/fontaine-android/src/android/ui.rs`
- `rust/fontaine-android/src/android/mod.rs`
- `docs/android.md`
- `CHANGELOG.md`

## Trade-offs

- **Block selection vs character selection.** The issue's expected behaviour is whole blocks, and
  ImGui text items give no glyph hit-testing outside the `Layout` the prompt field builds for its
  own editable text; character selection would need that layout per row. Block selection reuses
  the field's gesture vocabulary at a fraction of the cost and can be refined later.
- **Fence stripping on copy.** Copying the body of a fenced block (no fence lines) matches the
  stated use case ("copy a code block") and what a host terminal's selection of rendered code
  yields.
- **Spans from the current frame, not last frame.** Rows are submitted before the hit test in the
  same frame, so spans are never stale after a scroll or a streaming append.
- **Native Android text selection (a `TextView` overlay) rejected:** the UI is ImGui on a GL
  surface; an overlay view would need per-row JNI layout and would fight the ImGui input path.

## Decisions

- Copied text for a `Tool` block is what is on screen (summary plus the first six output lines),
  not the full output, so what is selected is what is copied.
- Edge auto-scroll uses a fixed rate of 12 dp per frame-second-normalised step (`dt`-scaled), a
  first value to be tuned on a device.
- A long press on a `reasoning` toggle selects nothing; opened reasoning text is selectable.

## Documentation

`docs/android.md`: under the transcript section, add the long-press selection gesture, the
handles, auto-scroll at the edges, the **Copy**/**Clear** bar, and that selection is by block.

## Testing Strategy

Unit tests in `select.rs` (host, every OS):

- `blocks`: an assistant message with two paragraphs and a fenced block yields three blocks in
  order; a blank line inside a fence does not split it; an unterminated fence (streaming) is one
  block; `Other` yields none; `Reasoning` yields a block only when open; `Tool` text holds the
  summary, reason and at most six output lines; `Shell` holds command and output.
- `copy_text`: fenced block copies without fence lines; multiple blocks join with a blank line;
  a reversed selection (cursor before anchor) copies in document order.
- `BlockSelection`: `extend_to` and `move_handle` keep order-independent `range()`, a handle
  dragged past the opposite end swaps ends; `clamp` shrinks to the new length and reports empty.
- `block_at`: inside a span; in the gap resolves to the nearest; above the first / below the last
  resolves to the ends; empty spans give `None`.

The ImGui gesture wiring, highlight, handles and clipboard write need a GLES context and a real
`ClipboardManager`, so they are not host-reachable; see Manual Verification.

## Manual Verification

On a device or emulator paired to a host, with a transcript holding prose, a fenced code block, a
tool call and a `+ reasoning` row. Not automated because the touch-to-ImGui and clipboard paths
only run on Android.

1. Long-press a paragraph: it highlights with handles; a Copy/Clear bar appears. Tap Copy, paste
   into the prompt field: the paragraph text arrives.
2. Long-press a code block and Copy: the pasted text is the code without fence lines.
3. Long-press, then drag down across several blocks: the selection grows; near the bottom edge
   the transcript scrolls and it keeps growing. Drag a handle to shrink it.
4. A plain drag (finger down and moving before half a second) still scrolls and fling still works;
   the scroll bar still works; no selection starts.
5. Switch agents with a selection active: it clears. Long-press the reasoning toggle: no toggle,
   no selection.

## Open Questions

- Whether a streaming message's new blocks should join an active selection that ends on its last
  block (currently they do not).
