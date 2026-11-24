//! Draws a `View` with Dear ImGui and reports what was tapped. The state lives in `App`; this
//! only renders it and forwards taps, so no session logic sits behind the unsafe FFI calls.

use std::collections::HashSet;
use std::ffi::{CString, c_char};

use dear_imgui_sys as sys;
use sys::{ImVec2_c, ImVec4_c};

use crate::app::{AgentChip, AgentHeader, DialogView, Screen, View};
use crate::commands::{DEVICE_COMMANDS, line_for};
use crate::field::Field;
use crate::layout::{self, Insets};
use crate::model::{DiffKind, Entry, TodoItem, ToolStatus};
use crate::scroll::DragScroll;

/// What the person did this frame.
pub enum Tap {
    /// Put the cursor at this byte offset of the field and open the keyboard.
    MoveCursor(usize),
    /// Select from the first offset to the second, which is the moving end.
    SelectRange(usize, usize),
    Cut,
    Copy,
    SelectAll,
    SubmitField,
    PasteField,
    GrantCamera,
    ConfirmPairing,
    DeclinePairing,
    ConnectHost(String),
    Interrupt,
    /// Show this agent's transcript; `None` is the main conversation.
    ViewAgent(Option<String>),
    /// Stop the agent on screen.
    StopAgent,
    /// Put this line in the prompt field.
    InsertCommand(String),
    Approval(String),
    Question(usize),
    Back,
}

const WINDOW_FLAGS: i32 = sys::ImGuiWindowFlags_NoTitleBar
    | sys::ImGuiWindowFlags_NoResize
    | sys::ImGuiWindowFlags_NoMove
    | sys::ImGuiWindowFlags_NoCollapse
    | sys::ImGuiWindowFlags_NoSavedSettings;

fn c(text: &str) -> CString {
    CString::new(text.replace('\0', "")).expect("NUL bytes were removed")
}

fn v2(x: f32, y: f32) -> ImVec2_c {
    ImVec2_c { x, y }
}

fn text(body: &str) {
    let body = c(body);
    // SAFETY: a frame is open and `body` is a NUL-terminated string that outlives the call.
    unsafe { sys::igTextUnformatted(body.as_ptr(), std::ptr::null()) };
}

fn wrapped(body: &str) {
    // SAFETY: a frame is open; wrap position 0 wraps at the window edge.
    unsafe { sys::igPushTextWrapPos(0.0) };
    text(body);
    // SAFETY: balances the push above.
    unsafe { sys::igPopTextWrapPos() };
}

fn colored(rgba: [f32; 4], body: &str) {
    let color = ImVec4_c {
        x: rgba[0],
        y: rgba[1],
        z: rgba[2],
        w: rgba[3],
    };
    // SAFETY: a frame is open; the pushed color is popped below.
    unsafe { sys::igPushStyleColor_Vec4(sys::ImGuiCol_Text, color) };
    wrapped(body);
    // SAFETY: balances the push above.
    unsafe { sys::igPopStyleColor(1) };
}

fn separator() {
    // SAFETY: a frame is open.
    unsafe { sys::igSeparator() };
}

fn avail_width() -> f32 {
    // SAFETY: a frame is open and a window is current.
    unsafe { sys::igGetContentRegionAvail() }.x
}

const ACCENT: [[f32; 4]; 3] = [
    [0.17, 0.42, 0.82, 1.0],
    [0.23, 0.50, 0.92, 1.0],
    [0.13, 0.34, 0.70, 1.0],
];
const NEUTRAL: [[f32; 4]; 3] = [
    [0.22, 0.24, 0.30, 1.0],
    [0.28, 0.31, 0.38, 1.0],
    [0.18, 0.20, 0.25, 1.0],
];
const BRIGHT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DIM: [f32; 4] = [0.62, 0.65, 0.70, 1.0];
/// Held this long without moving, a press on the field starts a selection.
const LONG_PRESS_SECS: f32 = 0.5;
const SELECTION: [f32; 4] = [0.23, 0.50, 0.92, 0.45];
const HEADING_SCALE: f32 = 1.4;

/// Per-frame state that outlives a frame and is not the app's.
#[derive(Default)]
pub struct UiState {
    /// Height of everything the session screen draws below the transcript, measured last frame;
    /// zero until a frame has been measured.
    footer_px: f32,
    /// A long press on the field that is still held, whether or not it has been dragged.
    selecting: Option<Selecting>,
    /// Reasoning rows the person opened, by the agent on screen (empty is main) and entry index.
    open_reasoning: HashSet<(String, usize)>,
    /// Touch-drag scrolling and fling of the transcript.
    transcript_scroll: DragScroll,
    /// The touch in progress stopped a fling, so it must not also toggle a reasoning row.
    swallow_tap: bool,
    todos_open: bool,
    commands_open: bool,
}

#[derive(Clone, Copy)]
struct Selecting {
    anchor: usize,
    moved: bool,
}

/// The camera image as a GL texture and the UV of each card corner (top-left, top-right,
/// bottom-right, bottom-left), already turned upright and cropped to a square.
#[derive(Clone, Copy, Debug)]
pub struct PreviewView {
    pub texture: u32,
    pub uv: [(f32, f32); 4],
}

/// Everything `draw` needs besides the view.
pub struct Frame<'a> {
    pub display: (f32, f32),
    pub insets: Insets,
    pub margin: f32,
    /// Display density: pixels per dp.
    pub scale: f32,
    /// The text typed so far, with its cursor and selection.
    pub field: &'a Field,
    /// Whether the scan screen can show the camera.
    pub camera_ready: bool,
    pub camera_denied: bool,
    /// The live image behind the scan card's label; `None` until a frame has arrived.
    pub preview: Option<PreviewView>,
    pub scroll_to_end: bool,
}

fn rgba(color: [f32; 4]) -> ImVec4_c {
    ImVec4_c {
        x: color[0],
        y: color[1],
        z: color[2],
        w: color[3],
    }
}

fn styled_button(label: &str, width: f32, colors: &[[f32; 4]; 3]) -> bool {
    let label = c(label);
    // SAFETY: a frame is open; the three pushed colors are popped below and `label` is
    // NUL-terminated.
    unsafe {
        sys::igPushStyleColor_Vec4(sys::ImGuiCol_Button, rgba(colors[0]));
        sys::igPushStyleColor_Vec4(sys::ImGuiCol_ButtonHovered, rgba(colors[1]));
        sys::igPushStyleColor_Vec4(sys::ImGuiCol_ButtonActive, rgba(colors[2]));
        let pressed = sys::igButton(label.as_ptr(), v2(width, 0.0));
        sys::igPopStyleColor(3);
        pressed
    }
}

fn primary_button(label: &str) -> bool {
    primary_button_sized(label, avail_width())
}

fn primary_button_sized(label: &str, width: f32) -> bool {
    styled_button(label, width, &ACCENT)
}

fn secondary_button(label: &str) -> bool {
    secondary_button_sized(label, avail_width())
}

fn secondary_button_sized(label: &str, width: f32) -> bool {
    styled_button(label, width, &NEUTRAL)
}

fn heading(body: &str) {
    // SAFETY: a frame is open; the font and color pushed here are popped below.
    unsafe {
        sys::igPushFont(
            std::ptr::null_mut(),
            (*sys::igGetStyle()).FontSizeBase * HEADING_SCALE,
        );
        sys::igPushStyleColor_Vec4(sys::ImGuiCol_Text, rgba(BRIGHT));
    }
    wrapped(body);
    // SAFETY: balances the pushes above.
    unsafe {
        sys::igPopStyleColor(1);
        sys::igPopFont();
    }
}

fn caption(body: &str) {
    colored(DIM, body);
}

fn gap(px: f32) {
    // SAFETY: a frame is open.
    unsafe { sys::igDummy(v2(0.0, px)) };
}

fn half_line() -> f32 {
    // SAFETY: a frame is open.
    let line = unsafe { sys::igGetTextLineHeight() };
    line * 0.5
}

fn style_pair(pick: impl FnOnce(&sys::ImGuiStyle) -> ImVec2_c) -> (f32, f32) {
    // SAFETY: the style lives as long as the context.
    let v = pick(unsafe { &*sys::igGetStyle() });
    (v.x, v.y)
}

fn avail() -> (f32, f32) {
    // SAFETY: a frame is open and a window is current.
    let v = unsafe { sys::igGetContentRegionAvail() };
    (v.x, v.y)
}

/// Draws the whole screen into a window covering the display minus the system's insets.
pub fn draw(view: &View<'_>, frame: &Frame<'_>, state: &mut UiState) -> Vec<Tap> {
    let mut taps = Vec::new();
    let title = c("fontaine");
    let (pos, size) = layout::safe_rect(frame.display, frame.insets);
    // SAFETY: a frame is open; the window is closed by the `igEnd` below, and the padding is
    // pushed only around `igBegin`, which is where the window reads it.
    unsafe {
        sys::igSetNextWindowPos(v2(pos.0, pos.1), sys::ImGuiCond_Always, v2(0.0, 0.0));
        sys::igSetNextWindowSize(v2(size.0, size.1), sys::ImGuiCond_Always);
        sys::igPushStyleVar_Vec2(
            sys::ImGuiStyleVar_WindowPadding,
            v2(frame.margin, frame.margin),
        );
        sys::igBegin(title.as_ptr(), std::ptr::null_mut(), WINDOW_FLAGS);
        sys::igPopStyleVar(1);
    }
    if let Some(notice) = view.notice {
        colored([1.0, 0.8, 0.3, 1.0], notice);
        gap(half_line());
    }
    if !matches!(view.screen, Screen::Session) {
        state.footer_px = 0.0;
    }
    match view.screen {
        Screen::Scan => scan(view, frame, state, &mut taps),
        Screen::Confirm { host } => {
            heading(&format!("Pair with {host}?"));
            caption(
                "The terminal on that machine will ask you to confirm before this phone is paired.",
            );
            gap(half_line());
            if primary_button("Pair") {
                taps.push(Tap::ConfirmPairing);
            }
            if secondary_button("Cancel") {
                taps.push(Tap::DeclinePairing);
            }
        }
        Screen::Pairing { host, first_time } => {
            if *first_time {
                heading(&format!("Waiting for {host} to confirm"));
                caption("Answer y in the host terminal.");
            } else {
                heading(&format!("Connecting to {host}..."));
            }
            gap(half_line());
            if secondary_button("Cancel") {
                taps.push(Tap::Back);
            }
        }
        Screen::Session => session(view, frame, state, &mut taps),
        Screen::Error(message) => {
            colored([1.0, 0.45, 0.4, 1.0], message);
            gap(half_line());
            if secondary_button("Back") {
                taps.push(Tap::Back);
            }
        }
    }
    // SAFETY: closes the window opened above.
    unsafe { sys::igEnd() };
    taps
}

/// Whether the press landed inside the rect, limited to the part of it the current window shows:
/// a field scrolled partly out of view is not tappable where it is cut off.
fn mouse_clicked_in(min: ImVec2_c, max: ImVec2_c) -> bool {
    // SAFETY: a frame is open; the context is current and the io struct lives as long as it.
    let (at, clip_min, clip_max) = unsafe {
        let list = sys::igGetWindowDrawList();
        (
            (*sys::igGetIO_Nil()).MouseClickedPos[0],
            sys::ImDrawList_GetClipRectMin(list),
            sys::ImDrawList_GetClipRectMax(list),
        )
    };
    let (min, max) = (
        ImVec2_c {
            x: min.x.max(clip_min.x),
            y: min.y.max(clip_min.y),
        },
        ImVec2_c {
            x: max.x.min(clip_max.x),
            y: max.y.min(clip_max.y),
        },
    );
    at.x >= min.x && at.x <= max.x && at.y >= min.y && at.y <= max.y
}

/// One visual line of wrapped text: the bytes drawn on it, without the blanks the wrap skipped or
/// the newline that ended it.
struct Line {
    start: usize,
    end: usize,
}

/// The wrapped layout ImGui gives `text` at one width, and the maps between byte offsets and
/// points. Drawing and hit-testing both go through it so they cannot disagree.
struct Layout {
    text: String,
    body: CString,
    font: *mut sys::ImFont,
    size: f32,
    wrap: f32,
    lines: Vec<Line>,
}

impl Layout {
    /// Follows the line loop of `ImFont::RenderText`, which `ImDrawList_AddText_FontPtr` runs, so
    /// the lines are the ones it draws.
    fn new(text: &str, wrap: f32) -> Self {
        let body = c(text);
        // SAFETY: a frame is open.
        let (font, size) = unsafe { (sys::igGetFont(), sys::igGetFontSize()) };
        let mut layout = Layout {
            text: text.replace('\0', ""),
            body,
            font,
            size,
            wrap,
            lines: Vec::new(),
        };
        layout.wrap_lines();
        layout
    }

    fn at(&self, offset: usize) -> *const c_char {
        // SAFETY: callers pass offsets within the text, so the pointer stays inside `body`.
        unsafe { self.body.as_ptr().add(offset) }
    }

    fn offset_of(&self, pointer: *const c_char) -> usize {
        pointer as usize - self.body.as_ptr() as usize
    }

    fn wrap_lines(&mut self) {
        let len = self.text.len();
        let mut line_start = 0;
        let mut s = 0;
        let mut wrap_end: Option<usize> = None;
        while s < len {
            let end = *wrap_end.get_or_insert_with(|| {
                // SAFETY: `s` is within the text and `len` is its end; the font belongs to the
                // current context.
                let found = unsafe {
                    sys::ImFont_CalcWordWrapPosition(
                        self.font,
                        self.size,
                        self.at(s),
                        self.at(len),
                        self.wrap,
                    )
                };
                self.offset_of(found)
            });
            if s >= end {
                self.lines.push(Line {
                    start: line_start,
                    end: s,
                });
                // SAFETY: both pointers lie within the text; the call only reads between them.
                let next =
                    unsafe { sys::igImTextCalcWordWrapNextLineStart(self.at(s), self.at(len), 0) };
                // A wrap width too small for one char would not advance: take the char.
                s = self.offset_of(next).max(s + 1).min(len);
                line_start = s;
                wrap_end = None;
                continue;
            }
            let c = self.text[s..].chars().next().expect("s is inside the text");
            s += c.len_utf8();
            if c == '\n' {
                self.lines.push(Line {
                    start: line_start,
                    end: s - 1,
                });
                line_start = s;
            }
        }
        if line_start < len || self.lines.is_empty() || self.text.ends_with('\n') {
            self.lines.push(Line {
                start: line_start,
                end: len,
            });
        }
    }

    fn height(&self) -> f32 {
        self.lines.len() as f32 * self.size
    }

    fn width(&self, from: usize, to: usize) -> f32 {
        // SAFETY: both offsets are within the text; unrounded widths, as the draw uses.
        unsafe {
            sys::ImFont_CalcTextSizeA(
                self.font,
                self.size,
                f32::MAX,
                0.0,
                self.at(from),
                self.at(to),
                std::ptr::null_mut(),
            )
        }
        .x
    }

    fn line_of(&self, idx: usize) -> usize {
        self.lines
            .iter()
            .rposition(|line| line.start <= idx)
            .unwrap_or(0)
    }

    /// Offset from the text origin of the top of the caret at byte `idx`.
    fn caret(&self, idx: usize) -> (f32, f32) {
        let row = self.line_of(idx);
        let line = &self.lines[row];
        (
            self.width(line.start, idx.min(line.end)),
            row as f32 * self.size,
        )
    }

    /// The boxes behind the selected range, as offsets from the text origin.
    fn selection_boxes(&self, range: std::ops::Range<usize>) -> Vec<(f32, f32, f32, f32)> {
        let mut boxes = Vec::new();
        for (row, line) in self.lines.iter().enumerate() {
            let limit = self
                .lines
                .get(row + 1)
                .map_or(self.text.len(), |next| next.start);
            if range.start >= limit.max(line.end) || range.end <= line.start {
                continue;
            }
            let from = range.start.max(line.start);
            let to = range.end.min(line.end).max(from);
            let left = self.width(line.start, from);
            let mut right = self.width(line.start, to);
            if range.end > line.end && line.end < limit {
                // Text between this line's end and the next line's start is a newline or skipped
                // blank; a soft wrap has none, so it gets no sliver.
                right += self.size * 0.25;
            }
            let top = row as f32 * self.size;
            boxes.push((left, top, right, top + self.size));
        }
        boxes
    }

    /// The byte offset nearest the point, given as an offset from the text origin.
    fn hit(&self, dx: f32, dy: f32) -> usize {
        let row = ((dy / self.size).floor().max(0.0) as usize).min(self.lines.len() - 1);
        let line = &self.lines[row];
        let mut previous = 0.0;
        for (at, c) in self.text[line.start..line.end].char_indices() {
            let next = self.width(line.start, line.start + at + c.len_utf8());
            if dx < (previous + next) * 0.5 {
                return line.start + at;
            }
            previous = next;
        }
        line.end
    }
}

/// A framed, wrapped text area standing in for a text input: the soft keyboard types into
/// `field` outside ImGui. A tap places the cursor and opens the keyboard; holding it selects a
/// word, or dragging after the hold selects a range, and either opens the edit menu.
fn field_frame(id: &str, field: &Field, hint: &str, state: &mut UiState, taps: &mut Vec<Tap>) {
    let (pad_x, pad_y) = style_pair(|s| s.FramePadding);
    // SAFETY: the style lives as long as the context.
    let border = unsafe { (*sys::igGetStyle()).FrameBorderSize };
    let width = avail_width();
    // Whole pixels, so the wrap is the same at every use; valid while the padding is at least the
    // frame border.
    let wrap = (width.floor() - 2.0 * pad_x).max(1.0);
    let layout = Layout::new(field.as_str(), wrap);
    let hint_layout = field.is_empty().then(|| Layout::new(hint, wrap));
    let shown = hint_layout.as_ref().unwrap_or(&layout);
    // SAFETY: a frame is open.
    let line_h = unsafe { sys::igGetTextLineHeight() };
    let height = shown.height().max(line_h) + 2.0 * pad_y + 2.0 * border;
    // SAFETY: a frame is open.
    let min = unsafe { sys::igGetCursorScreenPos() };
    let max = ImVec2_c {
        x: min.x + width,
        y: min.y + height,
    };
    let mut origin = v2(min.x + pad_x, min.y + pad_y);
    let child = c(id);
    // SAFETY: a frame is open; the child is ended below whether or not it is visible.
    let open = unsafe {
        sys::igBeginChild_Str(
            child.as_ptr(),
            v2(0.0, height),
            sys::ImGuiChildFlags_FrameStyle,
            sys::ImGuiWindowFlags_NoScrollbar | sys::ImGuiWindowFlags_NoScrollWithMouse,
        )
    };
    if open {
        // SAFETY: a frame is open and this child is its current window; `shown.body` is
        // NUL-terminated and outlives the draw calls. The wrap is the layout's, so the text breaks
        // where the layout says.
        unsafe {
            let at = sys::igGetCursorScreenPos();
            origin = v2(at.x.trunc(), at.y.trunc());
            let list = sys::igGetWindowDrawList();
            if let Some(range) = field.selection() {
                let color = sys::igGetColorU32_Vec4(rgba(SELECTION));
                for (left, top, right, bottom) in layout.selection_boxes(range) {
                    sys::ImDrawList_AddRectFilled(
                        list,
                        v2(origin.x + left, origin.y + top),
                        v2(origin.x + right, origin.y + bottom),
                        color,
                        0.0,
                        0,
                    );
                }
            }
            if let Some(range) = field.composing()
                && !field.is_empty()
            {
                let color = sys::igGetColorU32_Vec4(rgba(BRIGHT));
                for (left, _, right, bottom) in layout.selection_boxes(range) {
                    let y = origin.y + bottom - 1.0;
                    sys::ImDrawList_AddLine(
                        list,
                        v2(origin.x + left, y),
                        v2(origin.x + right, y),
                        color,
                        1.5,
                        0,
                    );
                }
            }
            let tint = if field.is_empty() {
                [0.55, 0.55, 0.55, 1.0]
            } else {
                BRIGHT
            };
            sys::ImDrawList_AddText_FontPtr(
                list,
                shown.font,
                shown.size,
                origin,
                sys::igGetColorU32_Vec4(rgba(tint)),
                shown.body.as_ptr(),
                std::ptr::null(),
                shown.wrap,
                std::ptr::null(),
            );
            let (x, y) = layout.caret(field.cursor());
            sys::ImDrawList_AddLine(
                list,
                v2(origin.x + x, origin.y + y),
                v2(origin.x + x, origin.y + y + layout.size),
                sys::igGetColorU32_Vec4(rgba(BRIGHT)),
                1.5,
                0,
            );
            sys::igDummy(v2(wrap, shown.height().max(line_h)));
        }
    }
    // SAFETY: balances `igBeginChild_Str`, which requires an end call even when it returned false.
    unsafe { sys::igEndChild() };

    let menu = c(&format!("{id}-menu"));
    let index_at = |at: ImVec2_c| layout.hit(at.x - origin.x, at.y - origin.y);
    // SAFETY: a frame is open; these only read input state and open or draw a popup of this window.
    unsafe {
        let io = sys::igGetIO_Nil();
        let popup_open = sys::igIsPopupOpen_Str(menu.as_ptr(), 0);
        let press_here = mouse_clicked_in(min, max);
        let down = sys::igIsMouseDown_Nil(0);
        let released = sys::igIsMouseReleased_Nil(0);
        let held = (*io).MouseDownDuration[0];
        if !down && !released {
            state.selecting = None;
        }
        if press_here
            && !popup_open
            && state.selecting.is_none()
            && down
            && held >= LONG_PRESS_SECS
            && !sys::igIsMouseDragging(0, -1.0)
        {
            state.selecting = Some(Selecting {
                anchor: index_at((*io).MouseClickedPos[0]),
                moved: false,
            });
        }
        if let Some(selecting) = state.selecting.as_mut()
            && down
            && sys::igIsMouseDragging(0, -1.0)
        {
            selecting.moved = true;
            taps.push(Tap::SelectRange(
                selecting.anchor,
                index_at(sys::igGetMousePos()),
            ));
        }
        if released && let Some(selecting) = state.selecting.take() {
            if !selecting.moved {
                let anchor = selecting.anchor;
                let inside = field
                    .selection()
                    .is_some_and(|range| range.start <= anchor && anchor <= range.end);
                if !inside {
                    let word = field.word_range_at(anchor);
                    taps.push(Tap::SelectRange(word.start, word.end));
                }
            }
            sys::igOpenPopup_Str(menu.as_ptr(), 0);
        }
        // `igIsMouseDragging` is false once the button is up, so the release frame asks whether
        // the finger ever moved past the threshold instead.
        if press_here
            && !popup_open
            && released
            && (*io).MouseDownDurationPrev[0] < LONG_PRESS_SECS
            && !sys::igIsMouseDragPastThreshold(0, -1.0)
        {
            taps.push(Tap::MoveCursor(index_at(sys::igGetMousePos())));
        }
        // The popup is drawn after the release check above, so a tap on its entry, which sits over
        // the field, still sees it open and does not also count as a tap on the field.
        if sys::igBeginPopup(menu.as_ptr(), 0) {
            // Buttons, not selectables: the release that ends the long press cannot trigger
            // them, only a fresh tap can.
            if field.selection().is_some() {
                if primary_button_sized("Cut", 0.0) {
                    taps.push(Tap::Cut);
                    sys::igCloseCurrentPopup();
                }
                if primary_button_sized("Copy", 0.0) {
                    taps.push(Tap::Copy);
                    sys::igCloseCurrentPopup();
                }
            }
            if primary_button_sized("Paste", 0.0) {
                taps.push(Tap::PasteField);
                sys::igCloseCurrentPopup();
            }
            if primary_button_sized("Select all", 0.0) {
                taps.push(Tap::SelectAll);
                sys::igCloseCurrentPopup();
            }
            sys::igEndPopup();
        }
    }
}

fn scan(view: &View<'_>, frame: &Frame<'_>, state: &mut UiState, taps: &mut Vec<Tap>) {
    heading("Connect to a Fontaine host");
    caption("Scan the QR code the host shows, or paste a pairing link.");
    gap(half_line());

    let (_, pad_y) = style_pair(|s| s.FramePadding);
    let (_, spacing_y) = style_pair(|s| s.ItemSpacing);
    // SAFETY: a frame is open.
    let line = unsafe { sys::igGetTextLineHeight() };
    let below = line + (line + 2.0 * pad_y) + (line + 2.0 * pad_y) + 3.0 * spacing_y;
    let (avail_w, avail_h) = avail();
    let side = layout::card_side(avail_w, avail_h, below, 8.0 * line);
    scan_card(frame, side, avail_w, taps);

    caption("Or paste a pairing link or ticket");
    field_frame("field", frame.field, "type here", state, taps);
    if !frame.field.is_empty() && primary_button("Use this") {
        taps.push(Tap::SubmitField);
    }
    if !view.hosts.is_empty() {
        caption("Paired hosts");
        for host in view.hosts {
            let label = format!(
                "{} ({})",
                host.label,
                fontaine_remote::short_node(&host.node)
            );
            if secondary_button(&label) {
                taps.push(Tap::ConnectHost(host.node.clone()));
            }
        }
    }
}

fn text_height(body: &str, width: f32) -> f32 {
    let body = c(body);
    // SAFETY: a frame is open and `body` is NUL-terminated.
    unsafe { sys::igCalcTextSize(body.as_ptr(), std::ptr::null(), false, width.max(1.0)) }.y
}

fn centered_wrapped(body: &str, tint: [f32; 4]) {
    let width = avail_width();
    let c_body = c(body);
    // SAFETY: a frame is open and `c_body` is NUL-terminated.
    let size =
        unsafe { sys::igCalcTextSize(c_body.as_ptr(), std::ptr::null(), false, width.max(1.0)) };
    if size.x < width {
        // SAFETY: a frame is open.
        unsafe { sys::igSetCursorPosX(sys::igGetCursorPosX() + (width - size.x) * 0.5) };
    }
    colored(tint, body);
}

fn scan_card(frame: &Frame<'_>, side: f32, avail_w: f32, taps: &mut Vec<Tap>) {
    let id = c("scan-card");
    // SAFETY: a frame is open.
    unsafe {
        if side < avail_w {
            sys::igSetCursorPosX(sys::igGetCursorPosX() + (avail_w - side) * 0.5);
        }
    }
    // SAFETY: a frame is open; the child is ended below whether or not it is visible.
    let open = unsafe {
        sys::igBeginChild_Str(
            id.as_ptr(),
            v2(side, side),
            sys::ImGuiChildFlags_Borders,
            sys::ImGuiWindowFlags_NoScrollbar | sys::ImGuiWindowFlags_NoScrollWithMouse,
        )
    };
    if open {
        let (inner_w, inner_h) = avail();
        let (_, spacing_y) = style_pair(|s| s.ItemSpacing);
        let (_, pad_y) = style_pair(|s| s.FramePadding);
        // SAFETY: a frame is open.
        let line = unsafe { sys::igGetTextLineHeight() };
        let button_h = line + 2.0 * pad_y;
        if frame.camera_ready {
            let hint = "Point the camera at the code in the terminal.";
            let total = line + spacing_y + text_height(hint, inner_w);
            if let Some(preview) = frame.preview {
                draw_preview(&preview, inner_w, inner_h, total + 2.0 * spacing_y);
                // Sit the label in the strip at the bottom of the image.
                // SAFETY: a frame is open and inside a child window.
                unsafe {
                    sys::igSetCursorPosY(
                        sys::igGetCursorPosY() + (inner_h - total - spacing_y).max(0.0),
                    );
                }
            } else {
                center_vertically(inner_h, total);
            }
            scanning_label(line);
            centered_wrapped(hint, DIM);
        } else {
            let (title, detail) = if frame.camera_denied {
                (
                    "Camera access is off",
                    "Allow it, or hold the field below and tap Paste to use a copied pairing link.",
                )
            } else {
                ("Camera is not running", "")
            };
            let detail_h = if detail.is_empty() {
                0.0
            } else {
                text_height(detail, inner_w) + spacing_y
            };
            let total = line + spacing_y + detail_h + button_h;
            center_vertically(inner_h, total);
            centered_wrapped(title, BRIGHT);
            if !detail.is_empty() {
                centered_wrapped(detail, DIM);
            }
            if primary_button("Allow camera") {
                taps.push(Tap::GrantCamera);
            }
        }
    }
    // SAFETY: balances `igBeginChild_Str`, which requires an end call even when it returned false.
    unsafe { sys::igEndChild() };
}

/// Fills the card's content area with the camera image and darkens a strip of `strip_h` at the
/// bottom so the label stays legible over a bright scene.
fn draw_preview(preview: &PreviewView, width: f32, height: f32, strip_h: f32) {
    // SAFETY: a frame is open inside the card; the draw list is the current window's and the
    // texture name is a live GL texture of the current context for this frame's render.
    unsafe {
        let origin = sys::igGetCursorScreenPos();
        let list = sys::igGetWindowDrawList();
        let at = |x: f32, y: f32| v2(origin.x + x, origin.y + y);
        let uv = |i: usize| v2(preview.uv[i].0, preview.uv[i].1);
        let texture = sys::ImTextureRef_c {
            _TexID: u64::from(preview.texture),
            ..Default::default()
        };
        sys::ImDrawList_AddImageQuad(
            list,
            texture,
            at(0.0, 0.0),
            at(width, 0.0),
            at(width, height),
            at(0.0, height),
            uv(0),
            uv(1),
            uv(2),
            uv(3),
            0xFFFF_FFFF,
        );
        let strip = sys::igGetColorU32_Vec4(ImVec4_c {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 0.6,
        });
        sys::ImDrawList_AddRectFilled(
            list,
            at(0.0, (height - strip_h).max(0.0)),
            at(width, height),
            strip,
            0.0,
            0,
        );
    }
}

fn center_vertically(inner_h: f32, content_h: f32) {
    let offset = ((inner_h - content_h) * 0.5).max(0.0);
    if offset > 0.0 {
        // SAFETY: a frame is open and inside a child window.
        unsafe { sys::igSetCursorPosY(sys::igGetCursorPosY() + offset) };
    }
}

/// "Scanning" with a dot that pulses, so a stalled frame loop is visible.
fn scanning_label(line: f32) {
    let label = "Scanning";
    let dot = line * 0.6;
    let spacing = line * 0.4;
    let text_w = {
        let c_label = c(label);
        // SAFETY: a frame is open and `c_label` is NUL-terminated.
        unsafe { sys::igCalcTextSize(c_label.as_ptr(), std::ptr::null(), false, -1.0) }.x
    };
    let width = avail_width();
    let total = dot + spacing + text_w;
    // SAFETY: a frame is open inside the card; the draw list is the current window's.
    unsafe {
        if total < width {
            sys::igSetCursorPosX(sys::igGetCursorPosX() + (width - total) * 0.5);
        }
        sys::igDummy(v2(dot, line));
        let min = sys::igGetItemRectMin();
        let pulse = 0.55 + 0.45 * (sys::igGetTime() as f32 * 5.0).sin();
        let color = sys::igGetColorU32_Vec4(ImVec4_c {
            x: 0.3,
            y: 0.85,
            z: 0.5,
            w: pulse,
        });
        sys::ImDrawList_AddCircleFilled(
            sys::igGetWindowDrawList(),
            v2(min.x + dot * 0.5, min.y + line * 0.5),
            dot * 0.5,
            color,
            0,
        );
        sys::igSameLine(0.0, spacing);
    }
    text(label);
}

fn session(view: &View<'_>, frame: &Frame<'_>, state: &mut UiState, taps: &mut Vec<Tap>) {
    if !view.status_line.is_empty() {
        colored([0.6, 0.7, 0.8, 1.0], &view.status_line);
    }
    if !view.chips.is_empty() {
        caption(&view.chips.join(" · "));
    }
    if let Some(ms) = view.reconnecting_in_ms {
        colored(
            [1.0, 0.8, 0.3, 1.0],
            &format!("Reconnecting in {:.1}s", ms as f32 / 1000.0),
        );
    }
    agents_bar(&view.agents, taps);
    if let Some(header) = &view.header {
        agent_header(header);
    }
    let dialog_rows = if view.dialog.is_some() { 7.0 } else { 0.0 };
    // SAFETY: a frame is open.
    let (line, row) = unsafe {
        (
            sys::igGetTextLineHeightWithSpacing(),
            sys::igGetFrameHeightWithSpacing(),
        )
    };
    let (_, spacing_y) = style_pair(|s| s.ItemSpacing);
    let footer = if state.footer_px > 0.0 {
        state.footer_px
    } else {
        row * 3.0 + line * dialog_rows
    };
    // The window scrolls rather than the transcript shrinking to nothing: ImGui reads a zero
    // height as "fill the rest" and a negative one as "rest minus this".
    let height = (avail().1 - footer - spacing_y).max(3.0 * line);
    let id = c("transcript");
    // SAFETY: a frame is open.
    let active_before = unsafe { sys::igGetActiveID() };
    // SAFETY: a frame is open; the child is ended below whether or not it is visible.
    let open = unsafe { sys::igBeginChild_Str(id.as_ptr(), v2(0.0, height), 0, 0) };
    if open {
        let owner = view.viewed.unwrap_or_default();
        for (at, entry) in view.entries.iter().enumerate() {
            entry_row(
                entry,
                owner,
                at,
                &mut state.open_reasoning,
                state.swallow_tap,
            );
        }
        drag_scroll(state, frame.scale, active_before == 0);
        // SAFETY: inside the child window.
        unsafe {
            if frame.scroll_to_end
                && !state.transcript_scroll.is_active()
                && sys::igGetScrollY() >= sys::igGetScrollMaxY() - line * 2.0
            {
                sys::igSetScrollHereY(1.0);
            }
        }
    }
    // SAFETY: balances `igBeginChild_Str`, which requires an end call even when it returned false.
    unsafe { sys::igEndChild() };
    // SAFETY: a frame is open.
    let footer_top = unsafe { sys::igGetCursorPosY() };
    separator();
    if let Some(dialog) = &view.dialog {
        dialog_panel(dialog, taps);
        separator();
    }
    todo_panel(view.todos, &mut state.todos_open);
    if !view.queued.is_empty() {
        caption("queued");
        for line in view.queued {
            wrapped(&format!("- {line}"));
        }
    }
    if view.commands_allowed {
        command_picker(&mut state.commands_open, taps);
    } else {
        state.commands_open = false;
    }
    let hint = if view.header.is_some() {
        "message this agent"
    } else {
        "message the session"
    };
    field_frame("prompt", frame.field, hint, state, taps);
    let (spacing_x, _) = style_pair(|s| s.ItemSpacing);
    let half = (avail_width() - spacing_x) * 0.5;
    if primary_button_sized("Send", half) {
        taps.push(Tap::SubmitField);
    }
    // SAFETY: a frame is open.
    unsafe { sys::igSameLine(0.0, -1.0) };
    if view.header.is_some() {
        if secondary_button_sized("Stop agent", avail_width()) {
            taps.push(Tap::StopAgent);
        }
    } else if secondary_button_sized("Interrupt", avail_width()) {
        taps.push(Tap::Interrupt);
    }
    if secondary_button("Leave session") {
        taps.push(Tap::Back);
    }
    // SAFETY: a frame is open.
    state.footer_px = unsafe { sys::igGetCursorPosY() } - footer_top;
}

/// Scrolls the transcript child, which must be the current window with its rows submitted, by a
/// finger drag and the fling after it. `free` is whether no other widget owned the active ID when
/// the child began, so a press on one (the reasoning button excepted, which is submitted inside)
/// is not taken for a scroll.
fn drag_scroll(state: &mut UiState, scale: f32, free: bool) {
    // SAFETY: a frame is open and the transcript child is the current window; this only reads
    // input and window state, and sets the child's scroll for the next frame.
    unsafe {
        let io = sys::igGetIO_Nil();
        let now = sys::igGetTime() as f32;
        let down = sys::igIsMouseDown_Nil(0);
        let released = sys::igIsMouseReleased_Nil(0);
        let y = sys::igGetMousePos().y;
        let scroll = &mut state.transcript_scroll;
        let mut delta = 0.0;

        if free && sys::igIsMouseClicked_Bool(0, false) {
            let max = sys::igGetScrollMaxY();
            // The scroll bar is submitted by `igEndChild`, so it is tested by its rect.
            let bar_left = sys::igGetWindowPos().x + sys::igGetWindowWidth()
                - (*sys::igGetStyle()).ScrollbarSize;
            let on_bar = max > 0.0 && sys::igGetMousePos().x >= bar_left;
            if !on_bar
                && sys::igIsWindowHovered(sys::ImGuiHoveredFlags_AllowWhenBlockedByActiveItem)
            {
                state.swallow_tap = scroll.press(y, now);
            }
        }
        if down {
            delta += scroll.drag(y, now, (*io).MouseDragThreshold);
        } else {
            if released {
                scroll.release(now, scale);
            }
            if scroll.is_flinging() {
                let at = sys::igGetScrollY();
                let towards_top = scroll.velocity() < 0.0;
                // Compared with the bounds rather than last frame's offset: a sub-pixel step
                // leaves the offset unchanged without being at an edge.
                if (towards_top && at <= 0.0) || (!towards_top && at >= sys::igGetScrollMaxY()) {
                    scroll.hit_edge();
                } else {
                    delta += scroll.step((*io).DeltaTime, scale);
                }
            }
            if !released {
                state.swallow_tap = false;
            }
        }
        if delta != 0.0 {
            sys::igSetScrollY_Float(sys::igGetScrollY() + delta);
        }
    }
}

fn entry_row(
    entry: &Entry,
    owner: &str,
    at: usize,
    open: &mut HashSet<(String, usize)>,
    swallow_tap: bool,
) {
    match entry {
        Entry::User(body) => colored([0.5, 0.8, 1.0, 1.0], &format!("> {body}")),
        Entry::Assistant { text: body, .. } => wrapped(body),
        Entry::Tool {
            tool,
            summary,
            status,
            output,
        } => {
            let (mark, color) = match status {
                ToolStatus::Pending => ("...", [0.7, 0.7, 0.7, 1.0]),
                ToolStatus::Done => ("ok", [0.5, 0.9, 0.5, 1.0]),
                ToolStatus::Failed(_) => ("failed", [1.0, 0.45, 0.4, 1.0]),
                ToolStatus::Denied(_) => ("denied", [1.0, 0.7, 0.3, 1.0]),
                ToolStatus::Interrupted => ("interrupted", [1.0, 0.7, 0.3, 1.0]),
                ToolStatus::Unknown => ("?", [0.7, 0.7, 0.7, 1.0]),
            };
            colored(color, &format!("[{tool}] {summary} - {mark}"));
            if let ToolStatus::Failed(why) | ToolStatus::Denied(why) = status {
                wrapped(why);
            }
            if let Some(body) = output {
                let shown: String = body.lines().take(6).collect::<Vec<_>>().join("\n");
                colored([0.6, 0.6, 0.6, 1.0], &shown);
            }
        }
        Entry::Notice {
            warning, headline, ..
        } => colored(
            if *warning {
                [1.0, 0.8, 0.3, 1.0]
            } else {
                [0.6, 0.6, 0.6, 1.0]
            },
            headline,
        ),
        Entry::Error(body) => colored([1.0, 0.45, 0.4, 1.0], &format!("! {body}")),
        Entry::Reasoning {
            text: body,
            streaming,
        } => {
            let key = (owner.to_string(), at);
            let opened = open.contains(&key);
            let mark = if opened { "-" } else { "+" };
            let label = format!(
                "{mark} reasoning{}##reasoning-{at}",
                if *streaming { " ..." } else { "" }
            );
            let clicked = styled_button(&label, avail_width(), &NEUTRAL);
            // The release frame of a drag is still past the threshold; `igIsMouseDragging` is
            // false by then.
            // SAFETY: a frame is open.
            let dragged = unsafe { sys::igIsMouseDragPastThreshold(0, -1.0) };
            if clicked && !dragged && !swallow_tap {
                if opened {
                    open.remove(&key);
                } else {
                    open.insert(key);
                }
            }
            if opened {
                colored(DIM, body);
            }
        }
        Entry::Shell { command, output } => {
            colored([0.5, 0.8, 1.0, 1.0], &format!("$ {command}"));
            colored([0.6, 0.6, 0.6, 1.0], output);
        }
        Entry::Attachment(name) => colored(DIM, &format!("attached: {name}")),
        Entry::Other(_) => {}
    }
}

/// The row of agents under the status line: a chip each, wrapped to the width. A chip with a
/// waiting dialog is marked with `!`.
fn agents_bar(agents: &[AgentChip<'_>], taps: &mut Vec<Tap>) {
    if agents.is_empty() {
        return;
    }
    let (spacing_x, _) = style_pair(|s| s.ItemSpacing);
    let (padding_x, _) = style_pair(|s| s.FramePadding);
    let labels: Vec<String> = agents
        .iter()
        .enumerate()
        .map(|(i, chip)| {
            format!(
                "{}{}{} {}##agent-{i}",
                if chip.selected { "> " } else { "" },
                if chip.waiting { "! " } else { "" },
                chip.name,
                chip.status
            )
        })
        .collect();
    let widths: Vec<f32> = labels
        .iter()
        .map(|label| {
            let shown = label.split("##").next().unwrap_or_default();
            text_height_width(shown) + padding_x * 2.0
        })
        .collect();
    for row in layout::chip_rows(&widths, avail_width(), spacing_x) {
        for i in row.clone() {
            if i != row.start {
                // SAFETY: a frame is open.
                unsafe { sys::igSameLine(0.0, -1.0) };
            }
            let chip = &agents[i];
            let colors = if chip.selected { &ACCENT } else { &NEUTRAL };
            if styled_button(&labels[i], widths[i], colors) {
                taps.push(Tap::ViewAgent(chip.id.map(str::to_string)));
            }
        }
    }
    gap(half_line());
}

fn text_height_width(body: &str) -> f32 {
    let body = c(body);
    // SAFETY: a frame is open and `body` is NUL-terminated.
    unsafe { sys::igCalcTextSize(body.as_ptr(), std::ptr::null(), false, -1.0) }.x
}

fn agent_header(header: &AgentHeader<'_>) {
    let mut line = format!("{} - {}", header.name, header.status);
    if !header.model.is_empty() {
        line.push_str(&format!(" - {}", header.model));
    }
    if !header.spend.is_empty() {
        line.push_str(&format!(" - {}", header.spend));
    }
    colored([0.85, 0.9, 1.0, 1.0], &line);
}

/// The host's todo list, read-only, collapsed to its progress until opened.
fn todo_panel(todos: &[TodoItem], open: &mut bool) {
    if todos.is_empty() {
        return;
    }
    let done = todos.iter().filter(|t| t.status == "completed").count();
    let label = format!(
        "{} todos {done}/{}##todos",
        if *open { "-" } else { "+" },
        todos.len()
    );
    if styled_button(&label, avail_width(), &NEUTRAL) {
        *open = !*open;
    }
    if *open {
        for todo in todos {
            let (mark, color) = match todo.status.as_str() {
                "completed" => ("[x]", [0.5, 0.9, 0.5, 1.0]),
                "in_progress" => ("[~]", [1.0, 0.85, 0.4, 1.0]),
                _ => ("[ ]", [0.7, 0.7, 0.7, 1.0]),
            };
            colored(color, &format!("{mark} {}", todo.content));
        }
    }
}

/// The commands the host runs for a device; a tap puts one in the prompt field.
fn command_picker(open: &mut bool, taps: &mut Vec<Tap>) {
    let label = format!("{} commands##commands", if *open { "-" } else { "+" });
    if styled_button(&label, avail_width(), &NEUTRAL) {
        *open = !*open;
    }
    if !*open {
        return;
    }
    for command in DEVICE_COMMANDS {
        let label = if command.hint.is_empty() {
            format!("/{}##cmd-{}", command.name, command.name)
        } else {
            format!("/{} {}##cmd-{}", command.name, command.hint, command.name)
        };
        if styled_button(&label, avail_width(), &NEUTRAL) {
            taps.push(Tap::InsertCommand(line_for(command)));
            *open = false;
        }
    }
}

fn dialog_panel(dialog: &DialogView<'_>, taps: &mut Vec<Tap>) {
    match dialog {
        DialogView::Approval {
            title,
            summary,
            reason,
            preview,
            choices,
            answerable,
        } => {
            colored([1.0, 0.85, 0.4, 1.0], title);
            wrapped(summary);
            if !reason.is_empty() {
                wrapped(reason);
            }
            if let Some(preview) = preview {
                wrapped(&format!(
                    "{} (+{} -{})",
                    preview.path, preview.added, preview.removed
                ));
                for line in preview.lines.iter().take(12) {
                    let (sign, color) = match line.kind {
                        DiffKind::Added => ("+ ", [0.5, 0.9, 0.5, 1.0]),
                        DiffKind::Removed => ("- ", [1.0, 0.5, 0.5, 1.0]),
                        _ => ("  ", [0.7, 0.7, 0.7, 1.0]),
                    };
                    colored(color, &format!("{sign}{}", line.text));
                }
            }
            // SAFETY: a frame is open; balanced below.
            unsafe { sys::igBeginDisabled(!answerable) };
            for choice in choices {
                if secondary_button(choice) {
                    taps.push(Tap::Approval((*choice).to_string()));
                }
            }
            // SAFETY: balances `igBeginDisabled`.
            unsafe { sys::igEndDisabled() };
        }
        DialogView::Question {
            question,
            answerable,
            host_only_note,
        } => {
            colored([1.0, 0.85, 0.4, 1.0], &question.question);
            if let Some(note) = host_only_note {
                wrapped(note);
            }
            // SAFETY: a frame is open; balanced below.
            unsafe { sys::igBeginDisabled(!answerable) };
            for (i, option) in question.options.iter().enumerate() {
                if secondary_button(&option.label) {
                    taps.push(Tap::Question(i));
                }
                if !option.description.is_empty() {
                    colored([0.6, 0.6, 0.6, 1.0], &option.description);
                }
            }
            // SAFETY: balances `igBeginDisabled`.
            unsafe { sys::igEndDisabled() };
        }
        DialogView::HostWaiting(what) => {
            colored(
                [1.0, 0.85, 0.4, 1.0],
                &format!("the host is waiting on {what}"),
            );
        }
    }
}
