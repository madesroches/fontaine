//! Draws a `View` with Dear ImGui and reports what was tapped. The state lives in `App`; this
//! only renders it and forwards taps, so no session logic sits behind the unsafe FFI calls.

use std::ffi::CString;

use dear_imgui_sys as sys;
use sys::{ImVec2_c, ImVec4_c};

use crate::app::{DialogView, Screen, View};
use crate::layout::{self, Insets};
use crate::model::{DiffKind, Entry, ToolStatus};

/// What the person did this frame.
pub enum Tap {
    /// Open the keyboard for the prompt or paste field.
    FocusField,
    SubmitField,
    PasteField,
    GrantCamera,
    ConfirmPairing,
    DeclinePairing,
    ConnectHost(String),
    Interrupt,
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
/// Held this long without moving, a press on the field opens its Paste popup.
const LONG_PRESS_SECS: f32 = 0.5;
const HEADING_SCALE: f32 = 1.4;

/// Per-frame state that outlives a frame and is not the app's.
#[derive(Default)]
pub struct UiState {
    /// Height of everything the session screen draws below the transcript, measured last frame;
    /// zero until a frame has been measured.
    footer_px: f32,
}

/// Everything `draw` needs besides the view.
pub struct Frame<'a> {
    pub display: (f32, f32),
    pub insets: Insets,
    pub margin: f32,
    /// The text the soft keyboard has typed so far.
    pub field: &'a str,
    /// Whether the scan screen can show the camera.
    pub camera_ready: bool,
    pub camera_denied: bool,
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
        Screen::Scan => scan(view, frame, &mut taps),
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

fn mouse_clicked_in(min: ImVec2_c, max: ImVec2_c) -> bool {
    // SAFETY: the context is current and the io struct lives as long as it.
    let at = unsafe { (*sys::igGetIO_Nil()).MouseClickedPos[0] };
    at.x >= min.x && at.x <= max.x && at.y >= min.y && at.y <= max.y
}

/// A framed, wrapped text area standing in for a text input: the soft keyboard types into
/// `field` outside ImGui. A tap opens the keyboard; holding it offers Paste.
fn field_frame(id: &str, field: &str, hint: &str, taps: &mut Vec<Tap>) {
    let (shown, tint) = if field.is_empty() {
        (hint, [0.55, 0.55, 0.55, 1.0])
    } else {
        (field, BRIGHT)
    };
    let (pad_x, pad_y) = style_pair(|s| s.FramePadding);
    // SAFETY: the style lives as long as the context.
    let border = unsafe { (*sys::igGetStyle()).FrameBorderSize };
    let width = avail_width();
    let body = c(shown);
    // SAFETY: a frame is open and `body` is NUL-terminated.
    let (text_h, line_h) = unsafe {
        (
            sys::igCalcTextSize(
                body.as_ptr(),
                std::ptr::null(),
                false,
                (width - 2.0 * pad_x).max(1.0),
            )
            .y,
            sys::igGetTextLineHeight(),
        )
    };
    let height = text_h.max(line_h) + 2.0 * pad_y + 2.0 * border;
    // SAFETY: a frame is open.
    let min = unsafe { sys::igGetCursorScreenPos() };
    let max = ImVec2_c {
        x: min.x + width,
        y: min.y + height,
    };
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
        colored(tint, shown);
    }
    // SAFETY: balances `igBeginChild_Str`, which requires an end call even when it returned false.
    unsafe { sys::igEndChild() };

    let menu = c(&format!("{id}-menu"));
    // SAFETY: a frame is open; these only read input state and open or draw a popup of this window.
    unsafe {
        let io = sys::igGetIO_Nil();
        let popup_open = sys::igIsPopupOpen_Str(menu.as_ptr(), 0);
        let press_here = mouse_clicked_in(min, max);
        let held = (*io).MouseDownDuration[0];
        if press_here
            && !popup_open
            && sys::igIsMouseDown_Nil(0)
            && held >= LONG_PRESS_SECS
            && !sys::igIsMouseDragging(0, -1.0)
        {
            sys::igOpenPopup_Str(menu.as_ptr(), 0);
        }
        // `igIsMouseDragging` is false once the button is up, so the release frame asks whether
        // the finger ever moved past the threshold instead.
        if press_here
            && !popup_open
            && sys::igIsMouseReleased_Nil(0)
            && (*io).MouseDownDurationPrev[0] < LONG_PRESS_SECS
            && !sys::igIsMouseDragPastThreshold(0, -1.0)
        {
            taps.push(Tap::FocusField);
        }
        // The popup is drawn after the release check above, so a tap on its entry, which sits over
        // the field, still sees it open and does not also count as a tap on the field.
        if sys::igBeginPopup(menu.as_ptr(), 0) {
            // A button, not a selectable: the release that ends the long press cannot trigger it,
            // only a fresh tap can.
            if primary_button_sized("Paste", 0.0) {
                taps.push(Tap::PasteField);
                sys::igCloseCurrentPopup();
            }
            sys::igEndPopup();
        }
    }
}

fn scan(view: &View<'_>, frame: &Frame<'_>, taps: &mut Vec<Tap>) {
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
    field_frame("field", frame.field, "type here", taps);
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
            center_vertically(inner_h, total);
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
    if let Some(ms) = view.reconnecting_in_ms {
        colored(
            [1.0, 0.8, 0.3, 1.0],
            &format!("Reconnecting in {:.1}s", ms as f32 / 1000.0),
        );
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
    // SAFETY: a frame is open; the child is ended below whether or not it is visible.
    let open = unsafe { sys::igBeginChild_Str(id.as_ptr(), v2(0.0, height), 0, 0) };
    if open {
        for entry in view.entries {
            entry_row(entry);
        }
        // SAFETY: inside the child window.
        unsafe {
            if frame.scroll_to_end && sys::igGetScrollY() >= sys::igGetScrollMaxY() - line * 2.0 {
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
    field_frame("prompt", frame.field, "message the session", taps);
    let (spacing_x, _) = style_pair(|s| s.ItemSpacing);
    let half = (avail_width() - spacing_x) * 0.5;
    if primary_button_sized("Send", half) {
        taps.push(Tap::SubmitField);
    }
    // SAFETY: a frame is open.
    unsafe { sys::igSameLine(0.0, -1.0) };
    if secondary_button_sized("Interrupt", avail_width()) {
        taps.push(Tap::Interrupt);
    }
    if secondary_button("Leave session") {
        taps.push(Tap::Back);
    }
    // SAFETY: a frame is open.
    state.footer_px = unsafe { sys::igGetCursorPosY() } - footer_top;
}

fn entry_row(entry: &Entry) {
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
        Entry::Other(_) => {}
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
