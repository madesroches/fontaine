//! Draws a `View` with Dear ImGui and reports what was tapped. The state lives in `App`; this
//! only renders it and forwards taps, so no session logic sits behind the unsafe FFI calls.

use std::ffi::CString;

use dear_imgui_sys as sys;
use sys::{ImVec2_c, ImVec4_c};

use crate::app::{DialogView, Screen, View};
use crate::model::{DiffKind, Entry, ToolStatus};

/// What the person did this frame.
pub enum Tap {
    /// Open the keyboard for the prompt or paste field.
    FocusField,
    SubmitField,
    GrantCamera,
    ConfirmPairing,
    DeclinePairing,
    ConnectHost(String),
    Interrupt,
    Approval(String),
    Question(usize),
    Back,
}

const WINDOW_FLAGS: i32 = (sys::ImGuiWindowFlags_NoTitleBar
    | sys::ImGuiWindowFlags_NoResize
    | sys::ImGuiWindowFlags_NoMove
    | sys::ImGuiWindowFlags_NoCollapse
    | sys::ImGuiWindowFlags_NoSavedSettings) as i32;

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
    unsafe { sys::igPushStyleColor_Vec4(sys::ImGuiCol_Text as i32, color) };
    wrapped(body);
    // SAFETY: balances the push above.
    unsafe { sys::igPopStyleColor(1) };
}

fn button(label: &str, width: f32) -> bool {
    let label = c(label);
    // SAFETY: a frame is open and `label` is NUL-terminated.
    unsafe { sys::igButton(label.as_ptr(), v2(width, 0.0)) }
}

fn separator() {
    // SAFETY: a frame is open.
    unsafe { sys::igSeparator() };
}

fn avail_width() -> f32 {
    // SAFETY: a frame is open and a window is current.
    unsafe { sys::igGetContentRegionAvail() }.x
}

/// Draws the whole screen into a full-display window. `field` is the text the soft keyboard has
/// typed so far, `camera_ready` says whether the scan screen can show the camera.
pub fn draw(
    view: &View<'_>,
    display: (f32, f32),
    field: &str,
    camera_ready: bool,
    camera_denied: bool,
    scroll_to_end: bool,
) -> Vec<Tap> {
    let mut taps = Vec::new();
    let title = c("fontaine");
    // SAFETY: a frame is open; the window is closed by the `igEnd` below.
    unsafe {
        sys::igSetNextWindowPos(v2(0.0, 0.0), sys::ImGuiCond_Always as i32, v2(0.0, 0.0));
        sys::igSetNextWindowSize(v2(display.0, display.1), sys::ImGuiCond_Always as i32);
        sys::igBegin(title.as_ptr(), std::ptr::null_mut(), WINDOW_FLAGS);
    }
    if let Some(notice) = view.notice {
        colored([1.0, 0.8, 0.3, 1.0], notice);
        separator();
    }
    match view.screen {
        Screen::Scan => scan(view, field, camera_ready, camera_denied, &mut taps),
        Screen::Confirm { host } => {
            wrapped(&format!("Pair with {host}?"));
            wrapped(
                "The terminal on that machine will ask you to confirm before this phone is paired.",
            );
            if button("Pair", avail_width() * 0.5) {
                taps.push(Tap::ConfirmPairing);
            }
            if button("Cancel", avail_width()) {
                taps.push(Tap::DeclinePairing);
            }
        }
        Screen::Pairing { host, first_time } => {
            if *first_time {
                wrapped(&format!(
                    "Waiting for {host} to confirm. Answer y in the host terminal."
                ));
            } else {
                wrapped(&format!("Connecting to {host}..."));
            }
            if button("Cancel", avail_width()) {
                taps.push(Tap::Back);
            }
        }
        Screen::Session => session(view, display, field, scroll_to_end, &mut taps),
        Screen::Error(message) => {
            colored([1.0, 0.45, 0.4, 1.0], message);
            if button("Back", avail_width()) {
                taps.push(Tap::Back);
            }
        }
    }
    // SAFETY: closes the window opened above.
    unsafe { sys::igEnd() };
    taps
}

fn field_box(field: &str, hint: &str, taps: &mut Vec<Tap>) {
    let shown = if field.is_empty() { hint } else { field };
    let tint = if field.is_empty() {
        [0.55, 0.55, 0.55, 1.0]
    } else {
        [1.0, 1.0, 1.0, 1.0]
    };
    colored(tint, shown);
    // SAFETY: a frame is open; the item just drawn is the one asked about.
    let tapped = unsafe { sys::igIsItemClicked(0) };
    if tapped {
        taps.push(Tap::FocusField);
    }
}

fn scan(
    view: &View<'_>,
    field: &str,
    camera_ready: bool,
    camera_denied: bool,
    taps: &mut Vec<Tap>,
) {
    text("Scan the QR code a Fontaine host shows");
    if camera_ready {
        wrapped("Point the camera at the code in the terminal.");
    } else if camera_denied {
        wrapped("Camera access is off. Paste the pairing link instead, or allow the camera.");
    }
    if !camera_ready && button("Allow camera", avail_width()) {
        taps.push(Tap::GrantCamera);
    }
    separator();
    text("Or paste a pairing link or ticket");
    field_box(field, "tap to paste", taps);
    if !field.is_empty() && button("Use this", avail_width()) {
        taps.push(Tap::SubmitField);
    }
    if !view.hosts.is_empty() {
        separator();
        text("Paired hosts");
        for host in view.hosts {
            let label = format!(
                "{} ({})",
                host.label,
                fontaine_remote::short_node(&host.node)
            );
            if button(&label, avail_width()) {
                taps.push(Tap::ConnectHost(host.node.clone()));
            }
        }
    }
}

fn session(
    view: &View<'_>,
    display: (f32, f32),
    field: &str,
    scroll_to_end: bool,
    taps: &mut Vec<Tap>,
) {
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
    let line = unsafe { sys::igGetTextLineHeightWithSpacing() };
    let footer = line * (3.0 + dialog_rows);
    let id = c("transcript");
    // SAFETY: a frame is open; the child is ended below whether or not it is visible.
    let open = unsafe {
        sys::igBeginChild_Str(id.as_ptr(), v2(0.0, display.1 - footer - line * 3.0), 0, 0)
    };
    if open {
        for entry in view.entries {
            entry_row(entry);
        }
        // SAFETY: inside the child window.
        unsafe {
            if scroll_to_end && sys::igGetScrollY() >= sys::igGetScrollMaxY() - line * 2.0 {
                sys::igSetScrollHereY(1.0);
            }
        }
    }
    // SAFETY: balances `igBeginChild_Str`, which requires an end call even when it returned false.
    unsafe { sys::igEndChild() };
    separator();
    if let Some(dialog) = &view.dialog {
        dialog_panel(dialog, taps);
        separator();
    }
    field_box(field, "message the session", taps);
    let half = avail_width() * 0.5;
    if button("Send", half) {
        taps.push(Tap::SubmitField);
    }
    // SAFETY: a frame is open.
    unsafe { sys::igSameLine(0.0, -1.0) };
    if button("Interrupt", avail_width()) {
        taps.push(Tap::Interrupt);
    }
    if button("Leave session", avail_width()) {
        taps.push(Tap::Back);
    }
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
                if button(choice, avail_width()) {
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
                if button(&option.label, avail_width()) {
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
