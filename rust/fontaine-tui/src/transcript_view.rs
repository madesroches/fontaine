//! The transcript view: every entry of the shown conversation in its full form, scrolled in the
//! live area. Settled lines are in the terminal's scrollback and cannot be redrawn, so the view
//! renders from the in-memory entries and leaves scrollback alone.

use std::cell::{Cell, RefCell};

use fontaine_core::keybindings::TuiAction;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::theme::Theme;
use crate::transcript::Transcript;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptKey {
    None,
    Close,
}

/// What the cached lines were rendered from. A change in any part makes them stale.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheKey {
    width: usize,
    viewing: Option<String>,
    changes: u64,
    entries: usize,
    /// Streaming text grows without an event of its own on the counter's other paths.
    last_len: usize,
    show_reasoning: bool,
    theme: Theme,
}

#[derive(Default)]
pub struct TranscriptView {
    /// First visible row while not following.
    scroll: usize,
    /// How far the lines can scroll, as last drawn.
    max_scroll: Cell<usize>,
    /// Rows the body had at the last draw, for the page keys.
    page: Cell<usize>,
    /// Pinned to the last row: content that grows keeps the tail in view. Drawing cannot write
    /// `scroll`, so the pin is a flag that the window computation resolves.
    follow: Cell<bool>,
    cache: RefCell<Option<(CacheKey, Vec<Line<'static>>)>>,
    #[cfg(test)]
    renders: Cell<usize>,
}

impl TranscriptView {
    /// Opens pinned to the tail, where the conversation currently is.
    pub fn new() -> TranscriptView {
        TranscriptView {
            follow: Cell::new(true),
            ..TranscriptView::default()
        }
    }

    pub fn following(&self) -> bool {
        self.follow.get()
    }

    /// The row the window starts at, as last drawn.
    fn top(&self) -> usize {
        if self.follow.get() {
            self.max_scroll.get()
        } else {
            self.scroll.min(self.max_scroll.get())
        }
    }

    /// `mapped` is the action the keymap gives the key, so a rebound `transcript` key closes
    /// the view. Ctrl-C is routed by the app before it gets here.
    pub fn handle_key(&mut self, key: KeyEvent, mapped: Option<TuiAction>) -> TranscriptKey {
        if mapped == Some(TuiAction::Transcript) {
            return TranscriptKey::Close;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let page = self.page.get().saturating_sub(1).max(1);
        match key.code {
            KeyCode::Esc => return TranscriptKey::Close,
            KeyCode::Char('q') if plain => return TranscriptKey::Close,
            KeyCode::Up => self.scroll_to(self.top().saturating_sub(1)),
            KeyCode::Down => self.scroll_to(self.top() + 1),
            KeyCode::PageUp => self.scroll_to(self.top().saturating_sub(page)),
            KeyCode::PageDown => self.scroll_to(self.top() + page),
            KeyCode::Home => self.scroll_to(0),
            KeyCode::End => self.scroll_to(usize::MAX),
            _ => {}
        }
        TranscriptKey::None
    }

    /// Moves the window to row `top`, clamped; reaching the bottom resumes following, and
    /// anything above it stops.
    fn scroll_to(&mut self, top: usize) {
        let max = self.max_scroll.get();
        self.scroll = top.min(max);
        self.follow.set(self.scroll >= max);
    }

    /// The rows to draw in a body of `rows`: the cached full rendering, rebuilt when what it
    /// was rendered from changed, sliced to the window.
    pub fn window(
        &self,
        transcript: &Transcript,
        viewing: Option<&str>,
        width: usize,
        theme: &Theme,
        rows: usize,
    ) -> Vec<Line<'static>> {
        let key = CacheKey {
            width,
            viewing: viewing.map(str::to_string),
            changes: transcript.changes(),
            entries: transcript.entries.len(),
            last_len: transcript.last_len(),
            show_reasoning: transcript.show_reasoning,
            theme: *theme,
        };
        let mut cache = self.cache.borrow_mut();
        if cache.as_ref().is_none_or(|(cached, _)| *cached != key) {
            #[cfg(test)]
            self.renders.set(self.renders.get() + 1);
            let lines = transcript.verbose_lines(width, theme);
            *cache = Some((key, lines));
        }
        let lines = &cache.as_ref().expect("filled above").1;
        let max = lines.len().saturating_sub(rows);
        self.max_scroll.set(max);
        self.page.set(rows);
        let top = self.top();
        lines[top..lines.len().min(top + rows)].to_vec()
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        vec![Line::styled(
            "Up/Down PgUp/PgDn scroll · Home/End · Esc or q close",
            Style::default().add_modifier(Modifier::DIM),
        )]
    }

    #[cfg(test)]
    fn renders(&self) -> usize {
        self.renders.get()
    }
}

#[cfg(test)]
mod tests {
    use fontaine_core::turn::SessionEvent;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn filled(n: usize) -> Transcript {
        let mut t = Transcript::default();
        for i in 0..n {
            t.user(format!("message {i}"));
        }
        t
    }

    fn first_text(lines: &[Line<'static>]) -> String {
        lines[0].spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_cache_is_reused_until_something_changes() {
        let theme = Theme::default();
        let mut t = filled(3);
        let view = TranscriptView::new();
        view.window(&t, None, 40, &theme, 5);
        view.window(&t, None, 40, &theme, 5);
        assert_eq!(view.renders(), 1);
        view.window(&t, None, 30, &theme, 5);
        assert_eq!(view.renders(), 2, "a new width");
        view.window(&t, Some("agent-1"), 30, &theme, 5);
        assert_eq!(view.renders(), 3, "another agent");
        t.user("more".into());
        view.window(&t, Some("agent-1"), 30, &theme, 5);
        assert_eq!(view.renders(), 4, "a new entry");
        t.on_event(SessionEvent::AssistantDelta("hel".into()));
        view.window(&t, Some("agent-1"), 30, &theme, 5);
        let streamed = view.renders();
        t.on_event(SessionEvent::AssistantDelta("lo".into()));
        view.window(&t, Some("agent-1"), 30, &theme, 5);
        assert_eq!(view.renders(), streamed + 1, "streaming growth");
    }

    #[test]
    fn it_opens_pinned_to_the_tail() {
        let theme = Theme::default();
        let t = filled(10);
        let view = TranscriptView::new();
        assert!(view.following());
        let window = view.window(&t, None, 40, &theme, 4);
        // Each message is a line and a blank row: 20 rows, the last 4 shown.
        assert_eq!(first_text(&window), "> message 8");
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let theme = Theme::default();
        let t = filled(10);
        let mut view = TranscriptView::new();
        view.window(&t, None, 40, &theme, 4);
        view.handle_key(key(KeyCode::Home), None);
        assert_eq!(
            first_text(&view.window(&t, None, 40, &theme, 4)),
            "> message 0"
        );
        view.handle_key(key(KeyCode::Up), None);
        assert_eq!(
            first_text(&view.window(&t, None, 40, &theme, 4)),
            "> message 0"
        );
        view.handle_key(key(KeyCode::PageDown), None);
        assert_eq!(first_text(&view.window(&t, None, 40, &theme, 4)), "");
        view.handle_key(key(KeyCode::End), None);
        view.handle_key(key(KeyCode::Down), None);
        assert_eq!(
            first_text(&view.window(&t, None, 40, &theme, 4)),
            "> message 8"
        );
    }

    #[test]
    fn following_keeps_the_tail_in_view_as_entries_grow() {
        let theme = Theme::default();
        let mut t = filled(10);
        let view = TranscriptView::new();
        view.window(&t, None, 40, &theme, 4);
        t.user("last".into());
        let window = view.window(&t, None, 40, &theme, 4);
        assert_eq!(first_text(&window), "> message 9");
        assert_eq!(first_text(&window[2..]), "> last");
    }

    #[test]
    fn scrolling_up_stops_following_until_the_bottom_is_reached_again() {
        let theme = Theme::default();
        let mut t = filled(10);
        let mut view = TranscriptView::new();
        view.window(&t, None, 40, &theme, 4);
        view.handle_key(key(KeyCode::Up), None);
        assert!(!view.following());
        let held = first_text(&view.window(&t, None, 40, &theme, 4));
        t.user("appended".into());
        t.user("appended again".into());
        assert_eq!(first_text(&view.window(&t, None, 40, &theme, 4)), held);
        view.handle_key(key(KeyCode::End), None);
        assert!(view.following());
        let at_end = view.window(&t, None, 40, &theme, 4);
        t.user("and one more".into());
        let after = view.window(&t, None, 40, &theme, 4);
        assert_ne!(first_text(&at_end), first_text(&after));
    }

    #[test]
    fn the_transcript_key_esc_and_q_close_and_other_keys_do_not() {
        let mut view = TranscriptView::new();
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), None),
            TranscriptKey::None
        );
        for closing in [
            (key(KeyCode::Esc), None),
            (key(KeyCode::Char('q')), None),
            (
                KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
                Some(TuiAction::Transcript),
            ),
        ] {
            assert_eq!(view.handle_key(closing.0, closing.1), TranscriptKey::Close);
        }
    }
}
