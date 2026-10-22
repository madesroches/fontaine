//! The multi-line text buffer behind the message box and the deny-reason field.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, Default)]
pub struct InputBuffer {
    chars: Vec<char>,
    cursor: usize,
}

impl InputBuffer {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.chars).into_iter().collect()
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

    pub fn kill_to_end(&mut self) {
        let end = self.line_end(self.cursor);
        // At a line's end the kill joins the next line, as in a shell.
        let end = if end == self.cursor {
            (end + 1).min(self.chars.len())
        } else {
            end
        };
        self.chars.drain(self.cursor..end);
    }

    pub fn kill_to_start(&mut self) {
        let start = self.line_start(self.cursor);
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn delete_word(&mut self) {
        let mut start = self.cursor;
        while start > 0 && self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.chars.drain(start..self.cursor);
        self.cursor = start;
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
                'w' => self.delete_word(),
                'b' => self.left(),
                'f' => self.right(),
                _ => return false,
            },
            KeyCode::Char(_) if alt => return false,
            KeyCode::Char(c) => self.insert(c),
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
    fn wrapping_splits_by_width() {
        assert_eq!(wrap("abcde", 2), vec!["ab", "cd", "e"]);
        assert_eq!(wrap("", 4), vec![""]);
    }
}
