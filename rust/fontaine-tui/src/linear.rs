//! The renderer for screen readers: no live area and no box drawing, just labelled lines
//! appended in order. Settled transcript entries print once, dialogs print when they open and
//! then only what changed, and what is typed is echoed. Raw mode clears output processing, so
//! every line ends in `\r\n`.

use std::collections::HashSet;
use std::io::{self, Write};

use fontaine_core::config::PermissionMode;
use fontaine_tools::todo::TodoItem;
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::app::App;
use crate::todo_panel;
use crate::transcript::{Cut, Entry, Settled, ToolStatus};
use crate::view;

const SELECTED: &str = "selected: ";
const PROMPT: &str = "> ";

/// Wide and tall enough that a dialog is neither wrapped nor cut: the terminal wraps for itself.
const WIDTH: usize = 1000;

/// Dialogs that wait for an answer, which ring the bell when they open.
const NEEDS_ANSWER: &[&str] = &["approval", "question", "MCP server", "guardian unavailable"];

/// One line as plain text. A selected row, which the dialogs mark by reversing it (and the `@`
/// list by a bold `> ` row), becomes `selected: <text>`.
pub(crate) fn flatten(line: &Line<'_>) -> String {
    let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let text = joined.trim_end();
    let has = |modifier: Modifier| {
        line.style.add_modifier.contains(modifier)
            || line
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(modifier))
    };
    let marked = text.starts_with(PROMPT) && has(Modifier::BOLD);
    if has(Modifier::REVERSED) || marked {
        let row = text.strip_prefix(PROMPT).unwrap_or(text).trim_start();
        return format!("{SELECTED}{row}");
    }
    text.to_string()
}

pub(crate) fn flatten_all(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| {
            flatten(line)
                .split('\n')
                .map(terminal_text)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// One line made safe to write raw: of a line redrawn with `\r` (a progress meter) only the
/// last drawing is kept, as a terminal would show it, and other control characters, which could
/// move the cursor or ring the bell, are dropped.
pub(crate) fn terminal_text(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    let line = line.rsplit('\r').next().unwrap_or(line);
    line.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}

/// Typed, pasted or recalled text as it is echoed: control characters other than newline and
/// tab would otherwise reach the terminal as escape sequences.
pub(crate) fn echo_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// A line without its `selected: ` prefix, marker column or indentation, so a row that only
/// gained or lost the selection compares equal to itself.
fn normalise(line: &str) -> &str {
    let line = line.strip_prefix(SELECTED).unwrap_or(line);
    let line = line
        .strip_prefix(PROMPT)
        .or_else(|| line.strip_prefix("  "))
        .unwrap_or(line);
    line.trim_start()
}

fn is_selected(line: &str) -> bool {
    line.starts_with(SELECTED)
}

fn header(title: &str) -> String {
    if title == "approval" {
        "Permission Required:".to_string()
    } else {
        format!("{title}:")
    }
}

struct Snapshot {
    title: String,
    instance: Option<u64>,
    lines: Vec<String>,
}

/// What printing a dialog came to.
pub(crate) struct Echoed {
    pub lines: Vec<String>,
    /// The dialog was not showing before: its header is in `lines`.
    pub opened: bool,
}

/// Prints a dialog when it opens and afterwards only the lines that are new or changed, so a
/// screen reader is not made to read it all again on every key.
#[derive(Default)]
pub(crate) struct OverlayEcho {
    last: Option<Snapshot>,
}

impl OverlayEcho {
    /// `instance` tells two dialogs with the same title apart.
    pub fn update(&mut self, title: &str, instance: Option<u64>, lines: Vec<String>) -> Echoed {
        let lines: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
        let printed = match &self.last {
            Some(last) if last.title == title && last.instance == instance => {
                let seen: HashSet<&str> = last.lines.iter().map(|l| normalise(l)).collect();
                let was_selected: HashSet<&str> = last
                    .lines
                    .iter()
                    .filter(|l| is_selected(l))
                    .map(|l| normalise(l))
                    .collect();
                let changed = lines
                    .iter()
                    .filter(|l| {
                        let n = normalise(l);
                        !seen.contains(n) || (is_selected(l) && !was_selected.contains(n))
                    })
                    .cloned()
                    .collect();
                Echoed {
                    lines: changed,
                    opened: false,
                }
            }
            _ => {
                let mut all = vec![header(title)];
                all.extend(lines.iter().cloned());
                Echoed {
                    lines: all,
                    opened: true,
                }
            }
        };
        self.last = Some(Snapshot {
            title: title.to_string(),
            instance,
            lines,
        });
        printed
    }

    /// The dialog went away: showing one again counts as opening it.
    pub fn close(&mut self) {
        self.last = None;
    }
}

/// What to write to turn the echoed input `old` into `new`, in a terminal that cannot move the
/// cursor within the line.
pub(crate) fn echo_diff(old: &str, new: &str) -> String {
    echo_diff_after(PROMPT, old, new)
}

/// A line that only shows the open text field: the dialogs draw it inline, ending in a cursor.
pub(crate) fn is_field_line(line: &str, bare_prefix: &str) -> bool {
    line.starts_with(bare_prefix) || line.ends_with('▏')
}

pub(crate) fn echo_diff_after(prompt: &str, old: &str, new: &str) -> String {
    let (old, new) = (&echo_text(old), &echo_text(new));
    if old == new {
        return String::new();
    }
    if let Some(added) = new.strip_prefix(old) {
        return crlf(added);
    }
    if let Some(removed) = old.strip_prefix(new)
        && !removed.contains('\n')
    {
        // One backspace per display column: wide characters occupy two, combining marks none.
        return "\x08 \x08".repeat(ratatui::text::Span::raw(removed).width());
    }
    format!("\r\n{prompt}{}", crlf(new))
}

pub(crate) fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

fn mode_announcement(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "[manual mode on]",
        PermissionMode::AcceptEdits => "[accept edits on]",
        PermissionMode::Plan => "[plan mode on]",
        PermissionMode::Auto => "[auto mode on]",
        PermissionMode::DontAsk => "[dontAsk mode on]",
    }
}

fn labelled(label: &str, text: &str, out: &mut Vec<String>) {
    for (i, line) in text.split('\n').enumerate() {
        out.push(if i == 0 {
            format!("{label} {line}")
        } else {
            line.to_string()
        });
    }
}

/// A settled piece of the transcript as labelled lines.
fn label(settled: &Settled<'_>, out: &mut Vec<String>) {
    match settled.entry {
        Entry::User(text) => labelled("you:", text, out),
        Entry::Assistant { text, .. } => {
            let from = settled.skip;
            let piece: String = match settled.end {
                Some(end) => text.chars().skip(from).take(end - from).collect(),
                None => text.chars().skip(from).collect(),
            };
            if piece.is_empty() {
                return;
            }
            let piece = piece.strip_suffix('\n').unwrap_or(&piece);
            if from == 0 {
                labelled("assistant:", piece, out);
            } else {
                out.extend(piece.split('\n').map(str::to_string));
            }
        }
        Entry::Tool {
            tool,
            summary,
            status,
            ..
        } => {
            let call = format!("{tool} {summary}");
            let line = match status {
                ToolStatus::Pending => format!("tool: {call} — pending"),
                ToolStatus::Done => format!("tool: {call} — done"),
                ToolStatus::Interrupted => format!("tool: {call} — interrupted"),
                ToolStatus::Failed(why) => format!("tool error: {call} — failed: {why}"),
                ToolStatus::Denied(why) => format!("tool error: {call} — denied: {why}"),
            };
            out.push(line);
        }
        Entry::Notice(text) => labelled("notice:", text, out),
        Entry::Error(text) => labelled("error:", text, out),
        Entry::Shell {
            command, output, ..
        } => {
            out.push(format!("shell: {command}"));
            out.extend(output.trim_end().split('\n').map(terminal_text));
        }
    }
}

/// Appends to a stream instead of drawing a screen. The callback `drive` takes is
/// [`Linear::render`].
pub struct Linear<W: Write> {
    out: W,
    echo: OverlayEcho,
    mode: Option<PermissionMode>,
    was_running: bool,
    /// The input text currently shown after a prompt, when a prompt is the open line.
    echoed: String,
    prompt_open: bool,
    /// The deny-reason field, not the message box, is what is being typed into.
    on_field: bool,
    /// The task list as last printed.
    todos_printed: Vec<TodoItem>,
}

impl<W: Write> Linear<W> {
    pub fn new(out: W) -> Linear<W> {
        Linear {
            out,
            echo: OverlayEcho::default(),
            mode: None,
            was_running: false,
            echoed: String::new(),
            prompt_open: false,
            on_field: false,
            todos_printed: Vec::new(),
        }
    }

    pub fn render(&mut self, app: &mut App) -> io::Result<()> {
        let mut lines = Vec::new();
        let mut bell = false;

        for settled in app.transcript.commit_entries(Cut::Lines) {
            let mut labelled = Vec::new();
            label(&settled, &mut labelled);
            // Not `terminal_text`: its redraw handling would drop the label before a `\r`.
            lines.extend(labelled.iter().map(|line| {
                line.chars()
                    .filter(|c| !c.is_control() || *c == '\t')
                    .collect::<String>()
            }));
        }

        // The list prints in full each time it changes, since there is no panel to look at.
        if app.todos.items() != self.todos_printed.as_slice() {
            self.todos_printed = app.todos.items().to_vec();
            lines.extend(
                todo_panel::plain_lines(&self.todos_printed)
                    .iter()
                    .map(|line| terminal_text(line)),
            );
        }

        let overlay = view::overlay(app, WIDTH, u16::MAX, true);
        if overlay.body.is_empty() {
            self.echo.close();
        } else {
            let mut shown = flatten_all(&overlay.body);
            shown.extend(flatten_all(&overlay.footer));
            if let Some((prefix, _)) = &overlay.field {
                // What is typed is echoed as it is typed, so the lines that show it (the
                // reason's footer, a dialog input with its cursor glyph) are not printed.
                let bare = prefix.trim_end();
                shown.retain(|line| !is_field_line(line, bare));
            }
            let echoed = self.echo.update(overlay.title, overlay.instance, shown);
            bell |= echoed.opened && NEEDS_ANSWER.contains(&overlay.title);
            lines.extend(echoed.lines);
        }

        if self
            .mode
            .replace(app.mode)
            .is_some_and(|before| before != app.mode)
        {
            lines.push(mode_announcement(app.mode).to_string());
        }

        bell |= self.was_running && !app.running;
        self.was_running = app.running;

        let (prompt, text) = match &overlay.field {
            Some((prefix, typed)) => (*prefix, typed.clone()),
            None => (PROMPT, app.input.text()),
        };
        let text = echo_text(&text);
        let on_field = overlay.field.is_some();
        let switched = on_field != self.on_field;
        self.on_field = on_field;

        let mut out = String::new();
        if !lines.is_empty() {
            if self.prompt_open {
                // A bare prompt is overwritten; typed text is left and the output starts below.
                out.push_str(if self.echoed.is_empty() {
                    "\r\x1b[2K"
                } else {
                    "\r\n"
                });
            }
            for line in &lines {
                out.push_str(line);
                out.push_str("\r\n");
            }
            self.prompt_open = false;
            self.echoed.clear();
        } else if switched && self.prompt_open {
            out.push_str("\r\n");
            self.prompt_open = false;
            self.echoed.clear();
        }
        if bell {
            out.push('\x07');
        }
        if self.prompt_open {
            out.push_str(&if on_field {
                echo_diff_after(prompt, &self.echoed, &text)
            } else {
                echo_diff(&self.echoed, &text)
            });
            self.echoed = text;
        } else if !app.running || on_field || !text.is_empty() {
            out.push_str(prompt);
            out.push_str(&crlf(&text));
            self.prompt_open = true;
            self.echoed = text;
        }
        if out.is_empty() {
            return Ok(());
        }
        self.out.write_all(out.as_bytes())?;
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Input;
    use crate::model_picker::{Group, ModelPicker, PickerPurpose};
    use fontaine_core::models::ModelRef;
    use fontaine_core::permissions::{ApprovalRequest, Offers};
    use fontaine_core::turn::SessionEvent;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Style;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tokio::sync::oneshot;

    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Rig {
        linear: Linear<Shared>,
        bytes: Rc<RefCell<Vec<u8>>>,
        app: App,
    }

    impl Rig {
        fn new() -> Rig {
            let bytes = Rc::new(RefCell::new(Vec::new()));
            Rig {
                linear: Linear::new(Shared(bytes.clone())),
                bytes,
                app: App::new(PermissionMode::Manual, "m".into()),
            }
        }

        /// Renders and returns what that render wrote.
        fn render(&mut self) -> String {
            let before = self.bytes.borrow().len();
            self.linear.render(&mut self.app).unwrap();
            String::from_utf8(self.bytes.borrow()[before..].to_vec()).unwrap()
        }

        fn key(&mut self, code: KeyCode) {
            self.app
                .handle_input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }

        fn type_text(&mut self, text: &str) {
            for c in text.chars() {
                self.key(KeyCode::Char(c));
            }
        }
    }

    fn request(command: &str) -> ApprovalRequest {
        ApprovalRequest {
            tool: "shell".into(),
            agent: "main".into(),
            tool_use_id: None,
            args: serde_json::json!({ "command": command }),
            summary: command.into(),
            tier: fontaine_core::tier::Tier::CriticalPath,
            reason: "needs approval".into(),
            offers: Offers::default(),
            proposed_rule: None,
        }
    }

    fn strings(lines: &[&str]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    fn tool_call(t: &mut crate::transcript::Transcript, id: &str, args: serde_json::Value) {
        t.on_event(SessionEvent::ToolCall {
            id: id.into(),
            tool: "shell".into(),
            args,
        });
    }

    // --- echo_diff ---

    #[test]
    fn terminal_text_keeps_the_last_redraw_and_drops_controls() {
        assert_eq!(terminal_text("10%\r50%\r100%"), "100%");
        assert_eq!(terminal_text("line\r"), "line");
        assert_eq!(terminal_text("a\x07b\x1b[2Jc\td"), "ab[2Jc\td");
        let lines = [Line::from(ratatui::text::Span::raw("first\nsecond"))];
        assert_eq!(flatten_all(&lines), ["first", "second"]);
    }

    #[test]
    fn moving_between_same_named_models_announces_the_account() {
        let mut rig = Rig::new();
        let group = |account: &str| {
            Group::new(
                account.into(),
                fontaine_core::config::AccountKind::named("ollama"),
                Ok(vec!["llama3".to_string()]),
            )
        };
        rig.app.models = Some(ModelPicker::new(
            PickerPurpose::Main,
            "pick".into(),
            None,
            Vec::new(),
            vec![group("alpha"), group("beta")],
        ));
        rig.render();
        rig.key(KeyCode::Down);
        let out = rig.render();
        assert!(out.contains("selected: beta/llama3"), "{out:?}");
    }

    #[test]
    fn echo_diff_writes_what_was_appended() {
        assert_eq!(echo_diff("", "a"), "a");
        assert_eq!(echo_diff("ab", "abc"), "c");
        assert_eq!(echo_diff("ab", "ab"), "");
    }

    #[test]
    fn echo_diff_ends_lines_with_a_carriage_return() {
        assert_eq!(echo_diff("ab", "ab\ncd"), "\r\ncd");
    }

    #[test]
    fn echo_diff_erases_trailing_characters_with_backspaces() {
        assert_eq!(echo_diff("abc", "ab"), "\x08 \x08");
        assert_eq!(echo_diff("abc", ""), "\x08 \x08".repeat(3));
    }

    #[test]
    fn echo_diff_starts_a_fresh_line_for_anything_else() {
        assert_eq!(echo_diff("abc", "aXc"), "\r\n> aXc");
        assert_eq!(echo_diff("abc", "history"), "\r\n> history");
        assert_eq!(echo_diff("ab\ncd", "ab"), "\r\n> ab");
        assert_eq!(echo_diff("ab\ncd", ""), "\r\n> ");
        assert_eq!(echo_diff("a", "x\ny"), "\r\n> x\r\ny");
    }

    // --- flatten ---

    fn reversed() -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    #[test]
    fn flatten_joins_spans_and_trims_the_end() {
        let line = Line::from(vec![
            ratatui::text::Span::raw("a "),
            ratatui::text::Span::raw("b   "),
        ]);
        assert_eq!(flatten(&line), "a b");
    }

    #[test]
    fn flatten_marks_a_reversed_row_selected_and_drops_its_marker() {
        assert_eq!(
            flatten(&Line::styled("> [y] allow once", reversed())),
            "selected: [y] allow once"
        );
        assert_eq!(
            flatten(&Line::styled("  ollama", reversed())),
            "selected: ollama"
        );
        let in_span = Line::from(ratatui::text::Span::styled("> row", reversed()));
        assert_eq!(flatten(&in_span), "selected: row");
        assert_eq!(
            flatten(&Line::raw("  [s] allow session")),
            "  [s] allow session"
        );
    }

    #[test]
    fn flatten_recognises_the_file_lists_bold_marker_row() {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        assert_eq!(
            flatten(&Line::styled("> src/main.rs", bold)),
            "selected: src/main.rs"
        );
        assert_eq!(flatten(&Line::raw("  src/lib.rs")), "  src/lib.rs");
        assert_eq!(flatten(&Line::raw("> quoted")), "> quoted");
    }

    #[test]
    fn flatten_finds_the_selection_in_each_dialog() {
        let theme = crate::theme::Theme::default();
        let selected = |lines: &[Line<'_>]| -> Vec<String> {
            flatten_all(lines)
                .into_iter()
                .filter(|l| is_selected(l))
                .collect()
        };

        let (tx, _rx) = oneshot::channel();
        let approval = crate::approval::Approval::new(request("ls"), tx);
        assert_eq!(selected(&approval.footer(80)), ["selected: [y] allow once"]);

        let prompt = crate::trust_prompt::TrustPrompt::new(
            std::path::Path::new("/w"),
            &crate::trust_prompt::Summary::default(),
            theme,
        );
        assert_eq!(
            selected(&prompt.lines()),
            ["selected: [n] do not trust it (asked again next time)"]
        );

        let picker = crate::model_picker::ModelPicker::new(
            crate::model_picker::PickerPurpose::Main,
            "h".to_string(),
            None,
            Vec::<ModelRef>::new(),
            vec![crate::model_picker::Group::new(
                "local".into(),
                fontaine_core::config::AccountKind::named("ollama"),
                Ok(vec!["llama3".to_string()]),
            )],
        );
        let rows = selected(&picker.body(80, 14, &theme));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("llama3"), "{rows:?}");

        let mut app = App::new(PermissionMode::Manual, "m".into());
        app.completion = Some(crate::app::CompletionList {
            mention: crate::complete::Mention {
                start: 0,
                end: 1,
                query: String::new(),
            },
            matches: vec!["a.rs".into(), "b.rs".into()],
            selected: 1,
        });
        let overlay = view::overlay(&app, 80, 24, true);
        assert_eq!(selected(&overlay.body), ["selected: b.rs"]);
    }

    // --- OverlayEcho ---

    #[test]
    fn a_dialog_prints_its_header_and_every_line_when_it_opens() {
        let mut echo = OverlayEcho::default();
        let out = echo.update(
            "model",
            None,
            strings(&["pick one", "selected: a", "  b", ""]),
        );
        assert!(out.opened);
        assert_eq!(out.lines, ["model:", "pick one", "selected: a", "  b"]);
    }

    #[test]
    fn the_approval_header_is_permission_required() {
        let mut echo = OverlayEcho::default();
        let out = echo.update("approval", Some(1), strings(&["Allow shell?"]));
        assert_eq!(out.lines[0], "Permission Required:");
    }

    #[test]
    fn an_unchanged_dialog_prints_nothing() {
        let mut echo = OverlayEcho::default();
        let lines = strings(&["q", "selected: a", "  b"]);
        echo.update("model", None, lines.clone());
        let again = echo.update("model", None, lines);
        assert!(!again.opened);
        assert!(again.lines.is_empty());
    }

    #[test]
    fn moving_the_selection_announces_only_the_new_row_and_a_changed_hint() {
        let mut echo = OverlayEcho::default();
        let (marked, plain) = (
            flatten(&Line::styled("> [y] allow", reversed())),
            flatten(&Line::raw("  [y] allow")),
        );
        let (marked_n, plain_n) = (
            flatten(&Line::styled("> [n] deny", reversed())),
            flatten(&Line::raw("  [n] deny")),
        );
        echo.update(
            "approval",
            Some(1),
            vec!["Allow?".into(), marked, plain_n, "hint for allow".into()],
        );
        let out = echo.update(
            "approval",
            Some(1),
            vec!["Allow?".into(), plain, marked_n, "hint for deny".into()],
        );
        assert_eq!(out.lines, ["selected: [n] deny", "hint for deny"]);
    }

    #[test]
    fn a_changed_body_line_prints_and_the_rest_does_not() {
        let mut echo = OverlayEcho::default();
        echo.update("providers", None, strings(&["one", "two"]));
        let out = echo.update("providers", None, strings(&["one", "two (signed in)"]));
        assert_eq!(out.lines, ["two (signed in)"]);
    }

    #[test]
    fn a_scroll_step_prints_the_row_that_came_in_and_the_new_selection() {
        let mut echo = OverlayEcho::default();
        echo.update("model", None, strings(&["  m1", "  m2", "selected: m3"]));
        let out = echo.update("model", None, strings(&["  m2", "  m3", "selected: m4"]));
        assert_eq!(out.lines, ["selected: m4"]);
        let out = echo.update("model", None, strings(&["  m3", "selected: m4", "  m5"]));
        assert_eq!(out.lines, ["  m5"]);
    }

    #[test]
    fn reopening_after_a_close_prints_the_header_again() {
        let mut echo = OverlayEcho::default();
        echo.update("denied", None, strings(&["x"]));
        echo.close();
        let out = echo.update("denied", None, strings(&["x"]));
        assert!(out.opened);
        assert_eq!(out.lines, ["denied:", "x"]);
    }

    #[test]
    fn a_second_approval_with_identical_text_counts_as_opened() {
        let mut echo = OverlayEcho::default();
        let lines = strings(&["Allow shell?", "ls"]);
        echo.update("approval", Some(1), lines.clone());
        let out = echo.update("approval", Some(2), lines);
        assert!(out.opened);
        assert_eq!(out.lines[0], "Permission Required:");
    }

    // --- the renderer ---

    #[test]
    fn the_first_render_shows_a_bare_prompt() {
        let mut rig = Rig::new();
        assert_eq!(rig.render(), "> ");
        assert_eq!(rig.render(), "");
    }

    #[test]
    fn typing_is_echoed_and_backspace_erases() {
        let mut rig = Rig::new();
        rig.render();
        rig.type_text("hi");
        assert_eq!(rig.render(), "hi");
        rig.key(KeyCode::Backspace);
        assert_eq!(rig.render(), "\x08 \x08");
    }

    #[test]
    fn output_while_text_is_typed_moves_off_the_line_and_re_echoes_it() {
        let mut rig = Rig::new();
        rig.render();
        rig.type_text("draft");
        rig.render();
        rig.app.transcript.notice("heads up");
        assert_eq!(rig.render(), "\r\nnotice: heads up\r\n> draft");
    }

    #[test]
    fn output_over_a_bare_prompt_overwrites_it() {
        let mut rig = Rig::new();
        rig.render();
        rig.app.transcript.notice("heads up");
        assert_eq!(rig.render(), "\r\x1b[2Knotice: heads up\r\n> ");
    }

    #[test]
    fn control_sequences_in_transcript_text_never_reach_the_terminal() {
        let mut rig = Rig::new();
        rig.render();
        let evil = "x\x1b]52;c;AAAA\x07y\rz";
        let t = &mut rig.app.transcript;
        t.on_event(SessionEvent::Assistant(evil.into()));
        t.notice(evil);
        t.on_event(SessionEvent::ToolCall {
            id: "a".into(),
            tool: "shell".into(),
            args: serde_json::json!({ "command": evil }),
        });
        t.error(evil);
        let out = rig.render();
        // The renderer's own prompt erase legitimately starts with ESC, so look for the payload.
        assert!(
            !out.contains("\x1b]52;") && !out.contains('\x07'),
            "{out:?}"
        );
        assert!(out.contains("assistant: x]52;c;AAAAyz"), "{out:?}");
    }

    #[test]
    fn echoed_input_drops_control_characters() {
        assert_eq!(echo_diff("", "a\x1b[2Jb\x07"), "a[2Jb");
        assert_eq!(echo_diff("a\x1bb", "a"), "\x08 \x08");
    }

    #[test]
    fn echo_diff_erases_one_backspace_per_display_column() {
        assert_eq!(echo_diff("a\u{4e2d}", "a"), "\x08 \x08".repeat(2));
        assert_eq!(echo_diff("ae\u{301}", "a"), "\x08 \x08");
    }

    #[test]
    fn each_entry_kind_has_its_label() {
        let mut rig = Rig::new();
        rig.render();
        let t = &mut rig.app.transcript;
        t.user("hi\nthere".into());
        t.on_event(SessionEvent::Assistant("hello".into()));
        tool_call(t, "a", serde_json::json!({ "command": "ls" }));
        t.on_event(SessionEvent::ToolResult {
            id: "a".into(),
            tool: "shell".into(),
            success: true,
            text: String::new(),
        });
        tool_call(t, "b", serde_json::json!({ "command": "rm x" }));
        t.on_event(SessionEvent::ToolResult {
            id: "b".into(),
            tool: "shell".into(),
            success: false,
            text: "no such file".into(),
        });
        tool_call(t, "c", serde_json::json!({ "command": "sleep" }));
        t.interrupt_pending();
        t.guardian_denied("d", "shell", "r", "why");
        tool_call(t, "e", serde_json::json!({ "command": "curl" }));
        t.guardian_denied("e", "shell", "rule", "too risky");
        t.error("it broke");
        t.notice("fyi");
        t.shell("echo hi".into(), "hi\n[exit 0]".into(), Some(0));
        let out = rig.render();
        let out = out
            .strip_prefix("\r\x1b[2K")
            .expect("the bare prompt is overwritten");
        let lines: Vec<&str> = out.split("\r\n").collect();
        assert_eq!(
            lines,
            [
                "you: hi",
                "there",
                "assistant: hello",
                "tool: shell ls — done",
                "tool error: shell rm x — failed: no such file",
                "tool: shell sleep — interrupted",
                "tool error: shell curl — denied: [rule] too risky",
                "error: it broke",
                "notice: fyi",
                "shell: echo hi",
                "hi",
                "[exit 0]",
                "> ",
            ]
        );
    }

    #[test]
    fn a_streaming_reply_is_committed_per_line_and_labelled_once() {
        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        let delta = |rig: &mut Rig, s: &str| {
            rig.app
                .transcript
                .on_event(SessionEvent::AssistantDelta(s.into()));
        };
        delta(&mut rig, "first li");
        assert_eq!(rig.render(), "");
        delta(&mut rig, "ne\nsecond");
        assert_eq!(rig.render(), "assistant: first line\r\n");
        delta(&mut rig, " line\nthird");
        assert_eq!(rig.render(), "second line\r\n");
        rig.app.transcript.on_event(SessionEvent::Assistant(
            "first line\nsecond line\nthird".into(),
        ));
        assert_eq!(rig.render(), "third\r\n");
    }

    #[test]
    fn a_mode_change_is_announced_once_and_the_first_render_is_silent() {
        let mut rig = Rig::new();
        rig.render();
        rig.app.mode = PermissionMode::Plan;
        assert_eq!(rig.render(), "\r\x1b[2K[plan mode on]\r\n> ");
        assert_eq!(rig.render(), "");
        rig.app.mode = PermissionMode::AcceptEdits;
        assert!(rig.render().contains("[accept edits on]"));
        for (mode, said) in [
            (PermissionMode::Auto, "[auto mode on]"),
            (PermissionMode::DontAsk, "[dontAsk mode on]"),
            (PermissionMode::Manual, "[manual mode on]"),
        ] {
            rig.app.mode = mode;
            assert!(rig.render().contains(said), "{said}");
        }
    }

    #[test]
    fn the_bell_rings_when_a_turn_ends() {
        let mut rig = Rig::new();
        rig.render();
        rig.app.running = true;
        assert!(!rig.render().contains('\x07'));
        rig.app.running = false;
        assert!(rig.render().contains('\x07'));
        assert!(!rig.render().contains('\x07'));
    }

    #[test]
    fn the_bell_rings_for_prompts_that_need_an_answer_and_not_for_others() {
        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        let (tx, _rx) = oneshot::channel();
        rig.app.begin_approval(request("ls"), tx);
        let out = rig.render();
        assert!(
            out.contains('\x07') && out.contains("Permission Required:"),
            "{out:?}"
        );

        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        rig.app
            .begin_server(crate::server_prompt::ServerPrompt::new(
                "github".into(),
                "d".into(),
            ));
        let out = rig.render();
        assert!(
            out.contains('\x07') && out.contains("MCP server:"),
            "{out:?}"
        );

        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        rig.type_text("/he");
        let out = rig.render();
        assert!(
            out.contains("commands:") && !out.contains('\x07'),
            "{out:?}"
        );
    }

    #[test]
    fn consecutive_server_prompts_each_announce_and_ring() {
        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        for name in ["github", "linear"] {
            rig.app
                .begin_server(crate::server_prompt::ServerPrompt::new(
                    name.into(),
                    "d".into(),
                ));
            let out = rig.render();
            assert!(
                out.contains('\x07') && out.contains("MCP server:"),
                "{out:?}"
            );
        }
    }

    #[test]
    fn a_queued_approval_replacing_an_answered_one_opens_again() {
        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        let (tx, _rx) = oneshot::channel();
        rig.app.begin_approval(request("ls"), tx);
        let (tx2, _rx2) = oneshot::channel();
        rig.app.begin_approval(request("ls"), tx2);
        rig.render();
        rig.app.approval = None;
        rig.app.prune_closed();
        let out = rig.render();
        assert!(
            out.contains('\x07') && out.contains("Permission Required:"),
            "{out:?}"
        );
    }

    #[test]
    fn the_deny_reason_is_echoed_as_it_is_typed_without_reprinting_the_footer() {
        let mut rig = Rig::new();
        rig.app.running = true;
        rig.render();
        let (tx, _rx) = oneshot::channel();
        rig.app.begin_approval(request("ls"), tx);
        rig.render();
        // Answer keys wait out a grace period after the prompt opens.
        rig.key(KeyCode::Down);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        rig.app
            .approval
            .as_mut()
            .expect("the approval is open")
            .handle_key_at(
                enter,
                std::time::Instant::now() + std::time::Duration::from_millis(400),
            );
        rig.key(KeyCode::Char('x'));
        rig.key(KeyCode::Backspace);
        let out = rig.render();
        assert_eq!(out.matches("Reason (optional").count(), 1, "{out:?}");
        assert!(out.ends_with(crate::approval::FEEDBACK_PREFIX), "{out:?}");
        rig.key(KeyCode::Char('o'));
        rig.key(KeyCode::Char('k'));
        assert_eq!(rig.render(), "ok");
        rig.key(KeyCode::Backspace);
        assert_eq!(rig.render(), "\x08 \x08");
    }

    #[test]
    fn typing_in_a_provider_field_echoes_only_the_new_characters() {
        use fontaine_core::accounts::{AccountRow, SignInState};
        let rows = vec![AccountRow {
            name: "work".into(),
            kind: fontaine_core::config::AccountKind::named("anthropic"),
            endpoint: "default".into(),
            state: SignInState::NotSignedIn,
        }];
        let mut rig = Rig::new();
        rig.app.providers = Some(crate::provider_dialog::ProviderDialog::new(rows, Some("work")).0);
        let opened = rig.render();
        assert!(opened.contains("API key for `work`"), "{opened:?}");
        assert!(
            !opened.contains('▏') && opened.ends_with("API key: "),
            "{opened:?}"
        );
        rig.key(KeyCode::Char('a'));
        assert_eq!(rig.render(), "•");
        rig.type_text("bc");
        assert_eq!(rig.render(), "••");
        rig.key(KeyCode::Backspace);
        assert_eq!(rig.render(), "\x08 \x08");
    }

    #[test]
    fn a_failed_write_is_an_error() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut linear = Linear::new(Broken);
        let mut app = App::new(PermissionMode::Manual, "m".into());
        assert!(linear.render(&mut app).is_err());
    }
}
