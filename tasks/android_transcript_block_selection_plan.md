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

No ImGui types; positions are `f32` screen-space pixels of the current frame (spans come from
`igGetItemRectMin/Max`, hit-tested with `igGetMousePos`).

```
/// One selectable unit of the transcript. `parts` are the drawn items (one per item, styled
/// segment with a colour tag); `copy` is the separate text that goes on the clipboard.
pub struct Block { pub entry: usize, pub parts: Vec<Part>, pub copy: String }
pub struct Part { pub text: String, pub color: ColorTag }

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
pub fn copy_text(blocks: &[Block], selection: &BlockSelection) -> String; // joins `Block::copy`
```

Block rules (what the issue names: message, code block, tool output):

- `User`, `Error`, `Notice`, `Attachment`: one block each, the copy text as drawn without decoration
  (no `> `/`! ` prefix is added to what is copied).
- A block whose display is several drawn items (`Tool`, `Shell`) gets one span covering all of
  them: min of the first item's rect min, max of the last item's rect max.
- `Assistant`: split into paragraphs on blank lines, except that a fenced code block
  (```` ``` ````-delimited, unterminated while streaming) is one block whose copied text is the
  fence body *without* the fence lines, so pasting a code block gives code. A prose paragraph is
  copied as written.
- `Tool`: one block: the summary line, the failure/denial reason if any, and the output as shown
  (the first six lines, since that is what is on screen), so what is copied is what was selected.
- `Shell`: one block: `$ command` and the output.
- `Reasoning`: a block only while its text is open (`open_reasoning`); the toggle row is not
  selectable.

Splitting lives here. The text drawn cannot be the same string as the text copied (decorations,
fence lines, truncated output), so `blocks()` returns both per block: the display `parts` and the
`copy` text. `entry_row` draws from the `parts` of `blocks()` output for the entries it covers, so
the drawn items and the selectable blocks cannot drift apart (DRY). Assistant paragraphs are drawn
as separate items with an explicit blank-line gap (a dummy of one line height minus two `ItemSpacing.y`, since ImGui adds
the spacing on both sides), so the original spacing is preserved.

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
   the child's top/bottom edge, scroll the child at 12 dp per second, scaled by `frame.scale` and
   the frame's `DeltaTime`, so a selection can reach off-screen blocks.
5. **Copy bar.** Whenever a selection exists, a bar in the footer area (above the prompt field,
   like the other footer items) shows **Copy** and **Clear** as buttons (not selectables, same
   reasoning as the field's menu), so Copy is reachable regardless of scroll. A tap elsewhere in
   the transcript that is not a scroll drag also clears the selection.
6. **Copy.** Copy pushes `Tap::CopyBlocks(String)` built from `copy_text`. `handle_tap` writes it
   with a new `write_clipboard_text(&str) -> bool` helper that `write_clipboard` also uses, so
   the failure notice is not duplicated. On success the selection clears.
7. **Staleness.** The selection is cleared when the viewed agent changes, and `clamp(blocks.len())`
   runs each frame so a `Truncate` event cannot leave an index past the end. Streaming appends
   only grow the tail, so indices before it stay valid; a message that is still streaming may
   gain blocks, which a selection ending on its last block does not auto-include (the selection
   is stable). Toggling a `reasoning` row (a tap that flips its open state) shifts global block
   indices, so the selection is cleared on any such toggle; a streaming reasoning entry that is
   the open one is not special-cased beyond the per-frame clamp.
8. **Rows that must not select.** The `reasoning` toggle keeps its tap behaviour; a long press on
   it selects nothing and does not toggle (the existing `swallow_tap`/drag-past-threshold guard is
   extended to a held press that began a selection).

## Implementation Steps

1. Add `select.rs` (`Block`, `blocks`, `BlockSelection`, `block_at`, `copy_text`) with unit
   tests; declare `pub mod select;` in `lib.rs`.
2. `android/ui.rs`: make `entry_row` draw from `blocks()` parts and record one span per block; add the `UiState`
   fields; draw selection highlight and handles.
3. `android/ui.rs`: long-press start, extension, handle drag, edge auto-scroll, footer Copy/Clear bar,
   clearing rules (including clear on reasoning toggle); integrate with `drag_scroll` (`abandon_press`, handle presses bypass it).
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

## Decisions

- Copied text for a `Tool` block is what is on screen (summary plus the first six output lines),
  not the full output, so what is selected is what is copied.
- Edge auto-scroll runs at 12 dp per second, scaled by `frame.scale` and the frame's `DeltaTime`.
- A long press on a `reasoning` toggle selects nothing; opened reasoning text is selectable.
- Native Android text-selection overlay rejected (UI is ImGui on a GL surface).
- AFK: `blocks()` returns per block both the display parts (styled segments, one per drawn item, with colour tag) and the separate copy text; a multi-item block (Tool, Shell) gets one span from the first item's rect min to the last item's rect max; assistant paragraphs are separate items with an explicit `ItemSpacing` blank-line gap.
- AFK: the selection is cleared on any reasoning toggle (a tap that flips open state); a streaming open reasoning entry is not special-cased beyond the per-frame clamp.
- AFK: the Copy/Clear bar is drawn in the footer area above the prompt field, visible whenever a selection exists; a non-scroll tap elsewhere in the transcript also clears.
- AFK: blocks added by a streaming message do not join an existing selection (the selection is stable).

## Documentation

`docs/android.md`: under the transcript section, add the long-press selection gesture, the
handles, auto-scroll at the edges, the **Copy**/**Clear** bar, and that selection is by block.

## Testing Strategy

Unit tests in `select.rs` (host, every OS):

- `blocks`: an assistant message with two paragraphs and a fenced block yields three blocks in
  order; a blank line inside a fence does not split it; an unterminated fence (streaming) is one
  block; `Other` yields none; `Reasoning` yields a block only when open (the `blocks()` count changes with `open_reasoning`); `Tool` text holds the
  summary, reason and at most six output lines; `Shell` holds command and output; display `parts` and `copy` text differ where expected (fence lines, decorations).
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

1. Long-press a paragraph: it highlights with handles; the Copy/Clear bar appears in the footer above the prompt field and stays reachable after scrolling. Tap Copy, paste
   into the prompt field: the paragraph text arrives.
2. Long-press a code block and Copy: the pasted text is the code without fence lines.
3. Long-press, then drag down across several blocks: the selection grows; near the bottom edge
   the transcript scrolls and it keeps growing. Drag a handle to shrink it.
4. A plain drag (finger down and moving before half a second) still scrolls and fling still works;
   the scroll bar still works; no selection starts.
5. Switch agents with a selection active: it clears. Long-press the reasoning toggle: no toggle,
   no selection.
6. With a selection active, tap a `+ reasoning` row to toggle it: the selection clears. Tap
   elsewhere in the transcript (no drag): the selection clears.
