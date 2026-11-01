//! The multi-line text buffer behind the message box and the deny-reason field.

use std::ops::Range;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rig_core::message::ImageMediaType;

/// What an attached file is.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingMedia {
    /// A clipboard image, sniffed when it was read.
    Image(ImageMediaType),
    /// A dropped PDF file.
    Pdf,
}

/// The most bytes one attached file may be: providers reject larger base64 parts
/// (Anthropic takes 5 MB per part), so an oversized attachment must not reach the wire.
pub const MAX_ATTACHMENT_BYTES: u64 = 5 * 1024 * 1024;

/// One file attached to the message box, waiting to be sent: the clipboard image saved in
/// the session scratch directory, or a dropped PDF. The bytes stay on disk until the message
/// is sent.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingAttachment {
    pub label: String,
    pub path: PathBuf,
    pub media: PendingMedia,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct InputBuffer {
    chars: Vec<char>,
    cursor: usize,
    /// The last non-empty kill, for Ctrl-Y; it outlives `take` and `set_text`.
    killed: String,
    /// Files attached to the message, shown as chips above the typed text.
    attachments: Vec<PendingAttachment>,
}

impl InputBuffer {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn first_char(&self) -> Option<char> {
        self.chars.first().copied()
    }

    /// The cursor as a char offset into the text.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the text, leaving the cursor at its end.
    pub fn set_text(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    /// Replaces the chars in `start..end` with `text`, leaving the cursor after it.
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        let end = end.min(self.chars.len());
        let start = start.min(end);
        let inserted: Vec<char> = text.chars().collect();
        self.cursor = start + inserted.len();
        self.chars.splice(start..end, inserted);
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.chars).into_iter().collect()
    }

    pub fn attachments(&self) -> &[PendingAttachment] {
        &self.attachments
    }

    pub fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    /// Adds a chip to what the message will carry.
    pub fn attach(&mut self, pending: PendingAttachment) {
        self.attachments.push(pending);
    }

    /// The attached files, for the message being sent.
    pub fn take_attachments(&mut self) -> Vec<PendingAttachment> {
        std::mem::take(&mut self.attachments)
    }

    /// Drops the last attachment; what Backspace does on an empty input.
    fn pop_attachment(&mut self) -> Option<PendingAttachment> {
        self.attachments.pop()
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Pasted text arrives with whatever line endings the source used.
    pub fn insert_str(&mut self, text: &str) {
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    fn line_start(&self, pos: usize) -> usize {
        self.chars[..pos]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |i| i + 1)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.chars[pos..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(self.chars.len(), |i| pos + i)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    pub fn up(&mut self) {
        let start = self.line_start(self.cursor);
        if start == 0 {
            return;
        }
        let column = self.cursor - start;
        let previous_end = start - 1;
        let previous_start = self.line_start(previous_end);
        self.cursor = (previous_start + column).min(previous_end);
    }

    pub fn down(&mut self) {
        let end = self.line_end(self.cursor);
        if end >= self.chars.len() {
            return;
        }
        let column = self.cursor - self.line_start(self.cursor);
        let next_start = end + 1;
        self.cursor = (next_start + column).min(self.line_end(next_start));
    }

    /// Removes `range`, leaves the cursor at its start, and keeps the text for Ctrl-Y.
    fn kill(&mut self, range: Range<usize>) {
        if !range.is_empty() {
            self.killed = self.chars[range.clone()].iter().collect();
        }
        self.cursor = range.start;
        self.chars.drain(range);
    }

    /// Start of the word before `pos`: skip separators back, then word chars back.
    fn word_start_before(&self, pos: usize, is_word: fn(char) -> bool) -> usize {
        let mut start = pos;
        while start > 0 && !is_word(self.chars[start - 1]) {
            start -= 1;
        }
        while start > 0 && is_word(self.chars[start - 1]) {
            start -= 1;
        }
        start
    }

    /// End of the word after `pos`: skip separators forward, then word chars forward.
    fn word_end_after(&self, pos: usize, is_word: fn(char) -> bool) -> usize {
        let mut end = pos;
        while end < self.chars.len() && !is_word(self.chars[end]) {
            end += 1;
        }
        while end < self.chars.len() && is_word(self.chars[end]) {
            end += 1;
        }
        end
    }

    pub fn kill_to_end(&mut self) {
        let end = self.line_end(self.cursor);
        // At a line's end the kill joins the next line, as in a shell.
        let end = if end == self.cursor {
            (end + 1).min(self.chars.len())
        } else {
            end
        };
        self.kill(self.cursor..end);
    }

    pub fn kill_to_start(&mut self) {
        self.kill(self.line_start(self.cursor)..self.cursor);
    }

    pub fn delete_word(&mut self) {
        self.kill(self.word_start_before(self.cursor, char::is_alphanumeric)..self.cursor);
    }

    /// Ctrl-W: a word is a run of non-whitespace, so a path goes in one piece.
    pub fn delete_word_whitespace(&mut self) {
        self.kill(self.word_start_before(self.cursor, |c| !c.is_whitespace())..self.cursor);
    }

    pub fn delete_word_forward(&mut self) {
        self.kill(self.cursor..self.word_end_after(self.cursor, char::is_alphanumeric));
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start_before(self.cursor, char::is_alphanumeric);
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end_after(self.cursor, char::is_alphanumeric);
    }

    pub fn buffer_start(&mut self) {
        self.cursor = 0;
    }

    pub fn buffer_end(&mut self) {
        self.cursor = self.chars.len();
    }

    pub fn yank(&mut self) {
        let killed = self.killed.clone();
        self.replace_range(self.cursor, self.cursor, &killed);
    }

    pub fn lines(&self) -> Vec<String> {
        self.text().split('\n').map(str::to_string).collect()
    }

    /// The cursor as a line and a column in characters.
    pub fn cursor_position(&self) -> (usize, usize) {
        let start = self.line_start(self.cursor);
        let line = self.chars[..start].iter().filter(|c| **c == '\n').count();
        (line, self.cursor - start)
    }

    /// Applies an editing key; false when the key is not one.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if control => match c.to_ascii_lowercase() {
                'a' => self.home(),
                'e' => self.end(),
                'u' => self.kill_to_start(),
                'k' => self.kill_to_end(),
                'w' => self.delete_word_whitespace(),
                'b' => self.left(),
                'f' => self.right(),
                'h' => self.backspace(),
                'd' => self.delete(),
                'y' => self.yank(),
                _ => return false,
            },
            KeyCode::Char(c) if alt => match c.to_ascii_lowercase() {
                'b' => self.word_left(),
                'f' => self.word_right(),
                'd' => self.delete_word_forward(),
                _ => return false,
            },
            KeyCode::Char(c) => self.insert(c),
            KeyCode::Backspace if control || alt => self.delete_word(),
            // Backspace on an empty input removes the last attachment chip.
            KeyCode::Backspace if self.chars.is_empty() && self.pop_attachment().is_some() => {}
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete if control => self.delete_word_forward(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if control || alt => self.word_left(),
            KeyCode::Left => self.left(),
            KeyCode::Right if control || alt => self.word_right(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::Home if control => self.buffer_start(),
            KeyCode::Home => self.home(),
            KeyCode::End if control => self.buffer_end(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }
}

/// Splits a line into pieces of at most `width` characters.
pub fn wrap(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> InputBuffer {
        let mut b = InputBuffer::default();
        b.insert_str(text);
        b
    }

    #[test]
    fn vertical_movement_keeps_the_column() {
        let mut b = buffer("abcd\nxy\nmnopq");
        b.up();
        assert_eq!(b.cursor_position(), (1, 2));
        b.up();
        assert_eq!(b.cursor_position(), (0, 2));
        b.down();
        b.down();
        assert_eq!(b.cursor_position(), (2, 2));
    }

    #[test]
    fn kills_and_word_delete() {
        let mut b = buffer("one two");
        b.delete_word();
        assert_eq!(b.text(), "one ");
        b.kill_to_start();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn set_text_and_replace_range_move_the_cursor() {
        let mut b = buffer("see @sr here");
        b.replace_range(4, 7, "@src/main.rs ");
        assert_eq!(b.text(), "see @src/main.rs  here");
        assert_eq!(b.cursor(), 17);
        b.set_text("new");
        assert_eq!((b.text().as_str(), b.cursor()), ("new", 3));
        assert_eq!(b.first_char(), Some('n'));
    }

    #[test]
    fn wrapping_splits_by_width() {
        assert_eq!(wrap("abcde", 2), vec!["ab", "cd", "e"]);
        assert_eq!(wrap("", 4), vec![""]);
    }

    fn press(b: &mut InputBuffer, code: KeyCode, modifiers: KeyModifiers) -> bool {
        b.handle_key(KeyEvent::new(code, modifiers))
    }

    fn at(text: &str, cursor: usize) -> InputBuffer {
        let mut b = buffer(text);
        b.cursor = cursor;
        b
    }

    #[test]
    fn word_movement_crosses_spaces_newlines_and_stops_at_the_ends() {
        let mut b = buffer("one   two\nthree");
        b.word_left();
        assert_eq!(b.cursor(), 10);
        b.word_left();
        assert_eq!(b.cursor(), 6);
        b.word_left();
        assert_eq!(b.cursor(), 0);
        b.word_left();
        assert_eq!(b.cursor(), 0);
        b.word_right();
        assert_eq!(b.cursor(), 3);
        b.word_right();
        assert_eq!(b.cursor(), 9);
        b.word_right();
        assert_eq!(b.cursor(), 15);
        b.word_right();
        assert_eq!(b.cursor(), 15);
    }

    #[test]
    fn forward_and_backward_word_deletes_share_boundaries() {
        let mut b = at("one  two three", 3);
        b.delete_word_forward();
        assert_eq!(b.text(), "one three");
        let mut b = at("one  two three", 8);
        b.delete_word();
        assert_eq!(b.text(), "one   three");
        let mut b = at("one  two three", 5);
        b.delete_word();
        assert_eq!(b.text(), "two three");
    }

    #[test]
    fn words_go_by_segment_but_ctrl_w_takes_the_whole_path() {
        let mut b = buffer("src/main.rs");
        b.word_left();
        assert_eq!(b.cursor(), 9);
        b.word_left();
        assert_eq!(b.cursor(), 4);
        b.delete_word();
        assert_eq!(b.text(), "main.rs");
        let mut b = buffer("cat src/main.rs");
        b.delete_word();
        assert_eq!(b.text(), "cat src/main.");
        let mut b = buffer("cat src/main.rs");
        b.delete_word_whitespace();
        assert_eq!(b.text(), "cat ");
    }

    #[test]
    fn a_kill_can_be_yanked_for_every_kind() {
        type Kill = (&'static str, usize, fn(&mut InputBuffer), &'static str);
        let kills: [Kill; 5] = [
            ("one two", 7, InputBuffer::kill_to_start, "one two"),
            ("one two", 0, InputBuffer::kill_to_end, "one two"),
            ("one two", 7, InputBuffer::delete_word, "two"),
            ("one two", 7, InputBuffer::delete_word_whitespace, "two"),
            ("one two", 0, InputBuffer::delete_word_forward, "one"),
        ];
        for (text, cursor, kill, killed) in kills {
            let mut b = at(text, cursor);
            kill(&mut b);
            b.set_text("x");
            b.yank();
            assert_eq!(b.text(), format!("x{killed}"));
            assert_eq!(b.cursor(), 1 + killed.len());
        }
    }

    #[test]
    fn an_empty_kill_keeps_the_entry_and_take_keeps_the_buffer() {
        let mut b = buffer("one two");
        b.delete_word();
        b.kill_to_end();
        b.take();
        b.yank();
        assert_eq!(b.text(), "two");
        let mut empty = InputBuffer::default();
        empty.yank();
        assert_eq!(empty.text(), "");
    }

    fn chip(name: &str) -> PendingAttachment {
        PendingAttachment {
            label: name.into(),
            path: PathBuf::from("/scratch").join(name),
            media: PendingMedia::Image(rig_core::message::ImageMediaType::PNG),
            size: 231_000,
        }
    }

    #[test]
    fn attachments_chip_take_and_pop_in_order() {
        let mut b = InputBuffer::default();
        assert!(!b.has_attachments());
        b.attach(chip("one.png"));
        b.attach(chip("two.png"));
        assert_eq!(b.attachments().len(), 2);
        // Backspace on an empty input pops the last chip.
        assert!(b.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        assert_eq!(b.attachments().len(), 1);
        assert_eq!(b.attachments()[0].label, "one.png");
        // With text, Backspace edits the text and leaves the chips alone.
        b.insert_str("x");
        b.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(b.text().is_empty());
        assert_eq!(b.attachments().len(), 1);
        // take() leaves the chips for the message.
        let taken = b.take_attachments();
        assert_eq!(taken.len(), 1);
        assert!(!b.has_attachments());
    }

    #[test]
    fn backspace_pops_only_the_last_chip_until_none_are_left() {
        let mut b = InputBuffer::default();
        b.attach(chip("a.png"));
        for _ in 0..2 {
            b.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        assert!(!b.has_attachments());
        // Nothing left: Backspace is a no-op, not a crash.
        b.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(b.text().is_empty());
    }

    #[test]
    fn buffer_jumps_span_lines_while_home_and_end_stay_on_one() {
        let mut b = at("ab\ncd\nef", 4);
        b.home();
        assert_eq!(b.cursor(), 3);
        b.end();
        assert_eq!(b.cursor(), 5);
        b.buffer_start();
        assert_eq!(b.cursor(), 0);
        b.buffer_end();
        assert_eq!(b.cursor(), 8);
    }

    #[test]
    fn the_editing_keys_map_to_their_actions() {
        use KeyCode::{Backspace, Char, Delete, End, Home, Left, Right};
        const C: KeyModifiers = KeyModifiers::CONTROL;
        const A: KeyModifiers = KeyModifiers::ALT;
        // (key, modifiers, text, cursor before, text after, cursor after)
        let rows = [
            (Left, C, "one two", 7, "one two", 4),
            (Left, A, "one two", 7, "one two", 4),
            (Char('b'), A, "one two", 7, "one two", 4),
            (Char('B'), A, "one two", 7, "one two", 4),
            (Right, C, "one two", 0, "one two", 3),
            (Right, A, "one two", 0, "one two", 3),
            (Char('f'), A, "one two", 0, "one two", 3),
            (Char('d'), A, "one two", 0, " two", 0),
            (Delete, C, "one two", 0, " two", 0),
            (Backspace, A, "one two", 7, "one ", 4),
            (Backspace, C, "one two", 7, "one ", 4),
            (Char('w'), C, "a src/x.rs", 10, "a ", 2),
            (Home, C, "ab\ncd", 4, "ab\ncd", 0),
            (End, C, "ab\ncd", 0, "ab\ncd", 5),
            (Char('h'), C, "abc", 3, "ab", 2),
            (Char('d'), C, "abc", 1, "ac", 1),
            (Left, KeyModifiers::NONE, "abc", 3, "abc", 2),
            (Home, KeyModifiers::NONE, "ab\ncd", 4, "ab\ncd", 3),
            (Backspace, KeyModifiers::NONE, "abc", 3, "ab", 2),
            (Delete, KeyModifiers::NONE, "abc", 0, "bc", 0),
        ];
        for (code, modifiers, text, cursor, after, cursor_after) in rows {
            let mut b = at(text, cursor);
            assert!(press(&mut b, code, modifiers), "{code:?} {modifiers:?}");
            assert_eq!(
                (b.text().as_str(), b.cursor()),
                (after, cursor_after),
                "{code:?} {modifiers:?}"
            );
        }
        let mut b = at("one two", 7);
        press(&mut b, Char('w'), C);
        press(&mut b, Char('y'), C);
        assert_eq!(b.text(), "one two");
        assert!(!press(&mut at("abc", 1), Char('z'), A));
    }
}
