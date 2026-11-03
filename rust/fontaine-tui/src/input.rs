//! The multi-line text buffer behind the message box and the deny-reason field.

use std::ops::Range;
use std::path::PathBuf;

use fontaine_core::config::EditorMode;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rig_core::message::ImageMediaType;

/// The most undo snapshots kept; edits before the oldest are no longer undoable.
const MAX_UNDO: usize = 100;

/// A vim operator waiting for its motion or target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingOp {
    Delete,
    Change,
    /// The first `g` of `gg`.
    G,
    /// `r`, waiting for the replacement character.
    Replace,
}

/// The vim input state. Insert is the start state; Normal may carry a pending operator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VimState {
    #[default]
    Insert,
    Normal {
        pending: Option<PendingOp>,
    },
}

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
    /// The last non-empty kill, for Ctrl-Y and vim's `p`; it outlives `take` and `set_text`.
    killed: String,
    /// Files attached to the message, shown as chips above the typed text.
    attachments: Vec<PendingAttachment>,
    /// How key events edit the buffer; overlay inputs keep the default (Emacs).
    pub mode: EditorMode,
    /// The vim modal state while `mode` is Vim.
    pub vim: VimState,
    /// Snapshots taken before each Normal-mode edit, for `u`; capped at `MAX_UNDO`.
    undo: Vec<(Vec<char>, usize)>,
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

    /// Whether the buffer is in vim Insert state and consumes an Esc to switch to Normal.
    /// The App asks before its own Esc handling, so Esc never interrupts from Insert.
    pub fn esc_leaves_insert(&mut self) -> bool {
        if self.mode == EditorMode::Vim && matches!(self.vim, VimState::Insert) {
            self.vim = VimState::Normal { pending: None };
            true
        } else {
            false
        }
    }

    /// `vim:insert` or `vim:normal` while vim mode is on, for the status bar.
    pub fn vim_label(&self) -> Option<&'static str> {
        if self.mode != EditorMode::Vim {
            return None;
        }
        Some(match self.vim {
            VimState::Insert => "vim:insert",
            VimState::Normal { .. } => "vim:normal",
        })
    }

    /// Applies an editing key; false when the key is not one (or, in vim Normal, when the key
    /// is Esc or a Ctrl/Alt shortcut, which belong to the App-level keymap).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            EditorMode::Emacs => self.emacs_key(key),
            EditorMode::Vim => match self.vim {
                VimState::Insert => self.vim_insert_key(key),
                VimState::Normal { .. } => self.vim_normal_key(key),
            },
        }
    }

    /// The readline/emacs key set: unchanged from before vim mode existed.
    fn emacs_key(&mut self, key: KeyEvent) -> bool {
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

    /// Vim Insert: printable text edits; Esc switches to Normal; the Ctrl+D/H/W editing keys
    /// stay the buffer's. Every other Ctrl+key returns false so the App-level keymap sees it.
    fn vim_insert_key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.vim = VimState::Normal { pending: None },
            KeyCode::Char(c) if !control && !alt => self.insert(c),
            KeyCode::Char('d') if control => self.delete(),
            KeyCode::Char('h') if control => self.backspace(),
            KeyCode::Char('w') if control => self.delete_word_whitespace(),
            // Backspace on an empty input removes the last attachment chip.
            KeyCode::Backspace if self.chars.is_empty() && self.pop_attachment().is_some() => {}
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }

    /// Vim Normal: motions, operators and undo. Esc and Ctrl/Alt shortcuts return false for
    /// the App-level keymap (interrupt, history search); every other key is consumed, and an
    /// unrecognized one just cancels a pending operator.
    fn vim_normal_key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Char(c) if !control && !alt => return self.vim_normal_char(c),
            KeyCode::Char(_) => return false,
            KeyCode::Left => {
                self.vim_motion(|b| b.left());
            }
            KeyCode::Right => {
                self.vim_motion(|b| b.right());
            }
            KeyCode::Up => {
                self.vim_motion(|b| b.up());
            }
            KeyCode::Down => {
                self.vim_motion(|b| b.down());
            }
            KeyCode::Home => {
                self.vim_motion(|b| b.home());
            }
            KeyCode::End => {
                self.vim_motion(|b| b.end());
            }
            KeyCode::Backspace => {
                self.vim_motion(|b| b.left());
            }
            _ => return true,
        }
        true
    }

    /// A movement key in Normal mode moves the cursor and cancels any pending operator.
    fn vim_motion(&mut self, motion: impl FnOnce(&mut Self)) -> bool {
        self.vim = VimState::Normal { pending: None };
        motion(self);
        true
    }

    /// Enter Insert at `pos`, the shared tail of `i` and `a`.
    fn vim_insert_at(&mut self, pos: usize) {
        self.cursor = pos.min(self.chars.len());
        self.vim = VimState::Insert;
    }

    /// A Normal-mode character; a pending operator takes it as its motion or target.
    fn vim_normal_char(&mut self, c: char) -> bool {
        if let VimState::Normal { pending: Some(op) } = self.vim {
            self.vim = VimState::Normal { pending: None };
            return self.vim_operator(op, c);
        }
        match c {
            'h' => self.left(),
            'l' => self.right(),
            'j' => self.down(),
            'k' => self.up(),
            '0' => self.home(),
            '$' => self.end(),
            'w' => self.word_right(),
            'b' => self.word_left(),
            'e' => self.cursor = self.vim_word_end(),
            'd' => self.vim_pending(PendingOp::Delete),
            'c' => self.vim_pending(PendingOp::Change),
            'g' => self.vim_pending(PendingOp::G),
            'G' => self.buffer_end(),
            'r' => self.vim_pending(PendingOp::Replace),
            'x' => {
                self.snapshot();
                let end = (self.cursor + 1).min(self.chars.len());
                self.kill(self.cursor..end);
            }
            'D' => {
                self.snapshot();
                self.kill(self.cursor..self.line_end(self.cursor));
            }
            'C' => {
                self.snapshot();
                self.kill(self.cursor..self.line_end(self.cursor));
                self.vim = VimState::Insert;
            }
            'u' => self.undo(),
            'p' => {
                if !self.killed.is_empty() {
                    self.snapshot();
                    let killed = self.killed.clone();
                    let at = (self.cursor + 1).min(self.chars.len());
                    self.replace_range(at, at, &killed);
                }
            }
            'i' => self.vim_insert_at(self.cursor),
            'a' => self.vim_insert_at(self.cursor + 1),
            'A' => {
                self.end();
                self.vim = VimState::Insert;
            }
            'I' => {
                self.home();
                self.vim = VimState::Insert;
            }
            'o' => {
                self.end();
                self.insert('\n');
                self.vim = VimState::Insert;
            }
            'O' => {
                self.home();
                self.insert('\n');
                self.up();
                self.end();
                self.vim = VimState::Insert;
            }
            // Any other key is consumed and ignored.
            _ => {}
        }
        true
    }

    /// The follow-up of a pending operator: its motion or target. An unrecognized key cancels
    /// the operator and is otherwise ignored.
    fn vim_operator(&mut self, op: PendingOp, c: char) -> bool {
        match op {
            PendingOp::G => {
                if c == 'g' {
                    self.buffer_start();
                }
            }
            PendingOp::Replace => {
                self.snapshot();
                if let Some(slot) = self.chars.get_mut(self.cursor) {
                    *slot = c;
                }
            }
            PendingOp::Delete | PendingOp::Change => {
                let change = op == PendingOp::Change;
                let range = match c {
                    'd' if !change => Some(self.line_range()),
                    'c' if change => Some(self.line_range()),
                    'w' => Some(self.cursor..self.vim_word_delete_end()),
                    'b' => Some(
                        self.word_start_before(self.cursor, char::is_alphanumeric)..self.cursor,
                    ),
                    'e' => Some(self.cursor..(self.vim_word_end() + 1).min(self.chars.len())),
                    '$' => Some(self.cursor..self.line_end(self.cursor)),
                    '0' => Some(self.line_start(self.cursor)..self.cursor),
                    _ => None,
                };
                if let Some(range) = range {
                    self.snapshot();
                    self.kill(range);
                    if change {
                        self.vim = VimState::Insert;
                    }
                }
            }
        }
        true
    }

    fn vim_pending(&mut self, op: PendingOp) {
        self.vim = VimState::Normal { pending: Some(op) };
    }

    /// The current line's whole span, what `dd`/`cc` delete.
    fn line_range(&self) -> Range<usize> {
        self.line_start(self.cursor)..self.line_end(self.cursor)
    }

    /// Where `dw` ends: the next word's end plus the separators after it, like vim's
    /// exclusive word motion.
    fn vim_word_delete_end(&self) -> usize {
        let mut end = self.word_end_after(self.cursor, char::is_alphanumeric);
        while end < self.chars.len()
            && self.chars[end] != '\n'
            && !self.chars[end].is_alphanumeric()
        {
            end += 1;
        }
        end
    }

    /// Where `e` lands: the last character of the word under the cursor, or of the next word.
    fn vim_word_end(&self) -> usize {
        let len = self.chars.len();
        let mut pos = self.cursor.min(len);
        if pos >= len {
            return self.cursor;
        }
        let is_word = |c: char| c.is_alphanumeric();
        if is_word(self.chars[pos]) {
            let mut end = pos;
            while end + 1 < len && is_word(self.chars[end + 1]) {
                end += 1;
            }
            if end > pos {
                return end;
            }
            pos += 1;
        }
        while pos < len && !is_word(self.chars[pos]) {
            pos += 1;
        }
        let mut end = pos;
        while end + 1 < len && is_word(self.chars[end + 1]) {
            end += 1;
        }
        end
    }

    /// Pushes the current text and cursor for `u`.
    fn snapshot(&mut self) {
        self.undo.push((self.chars.clone(), self.cursor));
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
    }

    /// `u`: restores the buffer as it was before the last Normal-mode edit.
    fn undo(&mut self) {
        if let Some((chars, cursor)) = self.undo.pop() {
            self.chars = chars;
            self.cursor = cursor.min(self.chars.len());
        }
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

    fn vim(text: &str) -> InputBuffer {
        let mut b = buffer(text);
        b.mode = EditorMode::Vim;
        b
    }

    fn vim_at(text: &str, cursor: usize) -> InputBuffer {
        let mut b = vim(text);
        b.cursor = cursor;
        b
    }

    fn normal(b: &mut InputBuffer) {
        b.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(b.vim, VimState::Normal { .. }));
    }

    #[test]
    fn vim_starts_in_insert_and_esc_switches_to_normal_without_leaving_the_buffer() {
        let mut b = vim("");
        assert!(matches!(b.vim, VimState::Insert));
        b.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(b.text(), "a");
        assert!(b.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(matches!(b.vim, VimState::Normal { .. }));
        // Esc in Normal is not consumed: it belongs to the App-level keymap.
        assert!(!b.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn esc_leaves_insert_reports_the_switch_once() {
        let mut b = vim("x");
        assert!(b.esc_leaves_insert());
        assert!(matches!(b.vim, VimState::Normal { .. }));
        assert!(!b.esc_leaves_insert());
        // Emacs mode never claims Esc.
        let mut e = buffer("x");
        assert!(!e.esc_leaves_insert());
    }

    #[test]
    fn vim_normal_motions_move_by_char_word_and_line() {
        let mut b = vim_at("one two three\nlast", 0);
        normal(&mut b);
        // 0 is the line's start; the cursor is already there.
        b.handle_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 0);
        for _ in 0..2 {
            b.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        }
        assert_eq!(b.cursor(), 7);
        b.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 4);
        b.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 6);
        b.handle_key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 13);
        b.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 12);
        b.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 13);
        // j and k walk the lines; k on the first line stays put, j clamps the column.
        b.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(b.cursor_position(), (0, 13));
        b.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(b.cursor_position(), (1, 4));
        // gg and G jump across the buffer.
        b.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), 0);
        b.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(b.cursor(), b.text().chars().count());
    }

    #[test]
    fn vim_operators_delete_and_change_and_fill_the_kill_slot() {
        let mut b = vim_at("one two three", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(b.text(), "two three");
        b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(b.text().is_empty());

        let mut b = vim_at("one two three", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert!(matches!(b.vim, VimState::Insert));
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        // cw takes the word and the separators after it, as dw does.
        assert_eq!(b.text(), "xtwo three");

        let mut b = vim_at("one two three", 8);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
        assert_eq!(b.text(), "one two ");

        // x deletes into the kill slot, p pastes it back after the cursor.
        let mut b = vim_at("abc", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(b.text(), "bc");
        b.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert_eq!(b.text(), "bac");

        // r replaces one character and stays in Normal.
        let mut b = vim_at("abc", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(b.text(), "zbc");
        assert!(matches!(b.vim, VimState::Normal { .. }));

        // An unrecognized operator follow-up cancels the operator and is ignored.
        let mut b = vim("abc");
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(b.text(), "abc");
        assert!(matches!(b.vim, VimState::Normal { pending: None }));
    }

    #[test]
    fn vim_undo_restores_the_last_normal_edit() {
        let mut b = vim_at("one two", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(b.text(), "two");
        b.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(b.text(), "one two");
        b.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(b.text(), "one two");
    }

    #[test]
    fn vim_entry_keys_enter_insert_at_the_right_spot() {
        // i, a, A and I on a single line.
        let mut b = vim_at("ac", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(matches!(b.vim, VimState::Insert));
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(b.text(), "xac");

        let mut b = vim_at("ac", 0);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(b.text(), "axc");

        let mut b = vim("ac bd");
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(b.text(), "ac bdx");

        let mut b = vim_at("ac bd", 4);
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(b.text(), "xac bd");

        // o opens a line below the current one, O above it.
        let mut b = vim("one\ntwo");
        normal(&mut b);
        b.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        assert_eq!(b.cursor_position(), (2, 0));
        b.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE));
        assert_eq!(b.cursor_position(), (2, 0));
        b.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(b.text(), "one\ntwo\ny\nx");
    }

    #[test]
    fn vim_insert_keeps_ctrl_d_h_w_and_passes_other_ctrl_through() {
        let mut b = vim_at("ac", 1);
        assert!(b.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)));
        assert_eq!(b.text(), "c");
        assert!(b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)));
        assert_eq!(b.text(), "");
        b.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        b.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(b.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert_eq!(b.text(), "a ");
        // Ctrl+T is the App-level agents panel: the buffer refuses it.
        assert!(!b.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)));
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
