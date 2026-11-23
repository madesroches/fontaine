//! The text of the paste or prompt field with a cursor, a selection and the input method's
//! composing region. The input method edits a Java-side mirror and the app edits taps and keys, so
//! the editing lives here where it can be tested.

use std::ops::Range;

/// Text with an anchor and a moving cursor, both byte offsets on char boundaries. The selection is
/// the range between them and is empty when they are equal. `composing` is the span the input
/// method is still revising (a swiped word); any edit that does not come from it ends the span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Field {
    text: String,
    anchor: usize,
    cursor: usize,
    composing: Option<Range<usize>>,
}

#[derive(PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

/// A NUL byte would shift every offset: the UI strips it before drawing.
fn stripped(text: &str) -> String {
    text.replace('\0', "")
}

impl Field {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        (self.anchor != self.cursor)
            .then(|| self.anchor.min(self.cursor)..self.anchor.max(self.cursor))
    }

    /// The span the input method is composing, when there is one.
    pub fn composing(&self) -> Option<Range<usize>> {
        self.composing.clone()
    }

    pub fn selected(&self) -> &str {
        self.selection().map_or("", |range| &self.text[range])
    }

    /// The nearest char boundary at or before `idx`, never past the end.
    fn clamp(&self, idx: usize) -> usize {
        let mut idx = idx.min(self.text.len());
        while !self.text.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    fn previous(&self, idx: usize) -> usize {
        self.text[..idx]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    fn next(&self, idx: usize) -> usize {
        self.text[idx..]
            .chars()
            .next()
            .map_or(idx, |c| idx + c.len_utf8())
    }

    fn collapse_to(&mut self, idx: usize) {
        self.composing = None;
        self.anchor = idx;
        self.cursor = idx;
    }

    fn delete(&mut self, range: Range<usize>) {
        self.text.replace_range(range.clone(), "");
        self.collapse_to(range.start);
    }

    /// Replaces the selection, if any, with `text`.
    pub fn insert(&mut self, text: &str) {
        let text = stripped(text);
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.text.replace_range(range.clone(), &text);
        self.collapse_to(range.start + text.len());
    }

    pub fn insert_char(&mut self, c: char) {
        self.insert(c.encode_utf8(&mut [0; 4]));
    }

    pub fn backspace(&mut self) {
        if let Some(range) = self.selection() {
            self.delete(range);
        } else if self.cursor > 0 {
            self.delete(self.previous(self.cursor)..self.cursor);
        }
    }

    pub fn delete_forward(&mut self) {
        if let Some(range) = self.selection() {
            self.delete(range);
        } else if self.cursor < self.text.len() {
            self.delete(self.cursor..self.next(self.cursor));
        }
    }

    pub fn move_to(&mut self, idx: usize) {
        let idx = self.clamp(idx);
        self.collapse_to(idx);
    }

    pub fn select(&mut self, anchor: usize, cursor: usize) {
        self.composing = None;
        self.anchor = self.clamp(anchor);
        self.cursor = self.clamp(cursor);
    }

    pub fn select_all(&mut self) {
        self.select(0, self.text.len());
    }

    /// Collapses a selection to its start, else steps one char back.
    pub fn move_left(&mut self) {
        let idx = match self.selection() {
            Some(range) => range.start,
            None => self.previous(self.cursor),
        };
        self.collapse_to(idx);
    }

    /// Collapses a selection to its end, else steps one char forward.
    pub fn move_right(&mut self) {
        let idx = match self.selection() {
            Some(range) => range.end,
            None => self.next(self.cursor),
        };
        self.collapse_to(idx);
    }

    /// Removes the selected text and returns it.
    pub fn cut(&mut self) -> String {
        let cut = self.selected().to_string();
        if let Some(range) = self.selection() {
            self.delete(range);
        }
        cut
    }

    /// The run of same-class chars (word, blanks or punctuation) under `idx`. A point on the
    /// blank right after a word means the word, which is what a finger on its end lands on.
    pub fn word_range_at(&self, idx: usize) -> Range<usize> {
        let idx = self.clamp(idx);
        let after = self.text[idx..].chars().next();
        let before = self.text[..idx].chars().next_back();
        let (probe, mut start, mut end) = match (after, before) {
            (Some(a), Some(b)) if class(a) == Class::Space && class(b) != Class::Space => {
                (b, idx - b.len_utf8(), idx)
            }
            (Some(a), _) => (a, idx, idx + a.len_utf8()),
            (None, Some(b)) => (b, idx - b.len_utf8(), idx),
            (None, None) => return idx..idx,
        };
        let kind = class(probe);
        while let Some(c) = self.text[..start].chars().next_back()
            && class(c) == kind
        {
            start -= c.len_utf8();
        }
        while let Some(c) = self.text[end..].chars().next()
            && class(c) == kind
        {
            end += c.len_utf8();
        }
        start..end
    }

    /// Replaces the whole text and puts the cursor at its end.
    pub fn replace_all(&mut self, text: &str) {
        self.composing = None;
        self.text = stripped(text);
        self.collapse_to(self.text.len());
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.collapse_to(0);
    }

    /// Takes the input method's state, in byte offsets. NULs are stripped and every offset moves
    /// back by the NULs before it; offsets are rounded down to char boundaries; an empty or
    /// inverted composing range means none.
    pub fn apply_ime(
        &mut self,
        text: &str,
        anchor: usize,
        cursor: usize,
        composing: Option<Range<usize>>,
    ) {
        let shift = |at: usize| {
            let end = at.min(text.len());
            let end = (0..=end)
                .rev()
                .find(|i| text.is_char_boundary(*i))
                .unwrap_or(0);
            end - text[..end].matches('\0').count()
        };
        let (anchor, cursor) = (shift(anchor), shift(cursor));
        let composing = composing.map(|r| shift(r.start)..shift(r.end));
        self.text = stripped(text);
        self.anchor = anchor;
        self.cursor = cursor;
        self.composing = composing.filter(|r| r.start < r.end);
    }

    /// The text, anchor, cursor and composing span as `apply_ime` takes them.
    pub fn ime_state(&self) -> (String, usize, usize, Option<Range<usize>>) {
        (
            self.text.clone(),
            self.anchor,
            self.cursor,
            self.composing.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str, cursor: usize) -> Field {
        let mut field = Field::default();
        field.replace_all(text);
        field.move_to(cursor);
        field
    }

    #[test]
    fn insert_lands_at_the_cursor() {
        let mut f = field("hello world", 5);
        f.insert_char('X');
        assert_eq!(f.as_str(), "helloX world");
        assert_eq!(f.cursor(), 6);
        f.move_to(0);
        f.insert("> ");
        assert_eq!(f.as_str(), "> helloX world");
        f.move_to(99);
        f.insert("!");
        assert_eq!(f.as_str(), "> helloX world!");
    }

    #[test]
    fn insert_replaces_the_selection() {
        let mut f = field("hello world", 0);
        f.select(6, 11);
        f.insert("there");
        assert_eq!(f.as_str(), "hello there");
        assert_eq!(f.selection(), None);
        assert_eq!(f.cursor(), 11);
    }

    #[test]
    fn a_backwards_selection_is_the_same_range() {
        let mut f = field("hello world", 0);
        f.select(5, 0);
        assert_eq!(f.selection(), Some(0..5));
        assert_eq!(f.selected(), "hello");
    }

    #[test]
    fn backspace_deletes_before_the_cursor_or_the_selection() {
        let mut f = field("abc", 0);
        f.backspace();
        assert_eq!(f.as_str(), "abc");
        f.move_to(2);
        f.backspace();
        assert_eq!(f.as_str(), "ac");
        assert_eq!(f.cursor(), 1);
        f.select_all();
        f.backspace();
        assert!(f.is_empty());
        f.backspace();
        assert!(f.is_empty());
    }

    #[test]
    fn delete_forward_deletes_after_the_cursor_or_the_selection() {
        let mut f = field("abc", 3);
        f.delete_forward();
        assert_eq!(f.as_str(), "abc");
        f.move_to(1);
        f.delete_forward();
        assert_eq!(f.as_str(), "ac");
        assert_eq!(f.cursor(), 1);
        f.select(0, 1);
        f.delete_forward();
        assert_eq!(f.as_str(), "c");
    }

    #[test]
    fn multi_byte_chars_are_never_split() {
        let mut f = field("aé😀b", 0);
        f.move_to(2);
        assert_eq!(f.cursor(), 1);
        f.move_to(1);
        f.move_right();
        assert_eq!(f.cursor(), 3);
        f.move_right();
        assert_eq!(f.cursor(), 7);
        f.backspace();
        assert_eq!(f.as_str(), "aéb");
        f.move_to(1);
        f.delete_forward();
        assert_eq!(f.as_str(), "ab");
        f.move_left();
        assert_eq!(f.cursor(), 0);
        f.select(0, 99);
        assert_eq!(f.selected(), "ab");
    }

    #[test]
    fn mid_char_selection_ends_round_down() {
        let mut f = field("é😀", 0);
        f.select(1, 3);
        assert_eq!(f.selected(), "é");
        f.select(0, 100);
        assert_eq!(f.selected(), "é😀");
    }

    #[test]
    fn move_and_select_clamp_out_of_range_indices() {
        let mut f = field("abc", 0);
        f.move_to(usize::MAX);
        assert_eq!(f.cursor(), 3);
        f.select(usize::MAX, usize::MAX);
        assert_eq!(f.selection(), None);
        f.select(1, usize::MAX);
        assert_eq!(f.selected(), "bc");
    }

    #[test]
    fn select_all_selects_everything() {
        let mut f = field("abc", 1);
        f.select_all();
        assert_eq!(f.selected(), "abc");
        let mut empty = Field::default();
        empty.select_all();
        assert_eq!(empty.selection(), None);
    }

    #[test]
    fn left_and_right_collapse_a_selection_else_step() {
        let mut f = field("abcd", 4);
        f.select(1, 3);
        f.move_left();
        assert_eq!((f.cursor(), f.selection()), (1, None));
        f.select(1, 3);
        f.move_right();
        assert_eq!((f.cursor(), f.selection()), (3, None));
        f.move_right();
        f.move_right();
        assert_eq!(f.cursor(), 4);
        f.move_to(0);
        f.move_left();
        assert_eq!(f.cursor(), 0);
    }

    #[test]
    fn cut_returns_and_removes_the_selection() {
        let mut f = field("hello world", 0);
        assert_eq!(f.cut(), "");
        assert_eq!(f.as_str(), "hello world");
        f.select(5, 11);
        assert_eq!(f.cut(), " world");
        assert_eq!(f.as_str(), "hello");
        assert_eq!(f.cursor(), 5);
    }

    #[test]
    fn replace_all_puts_the_cursor_at_the_end() {
        let mut f = field("old", 1);
        f.replace_all("new text");
        assert_eq!(f.as_str(), "new text");
        assert_eq!((f.cursor(), f.selection()), (8, None));
    }

    #[test]
    fn nul_bytes_are_stripped() {
        let mut f = Field::default();
        f.insert("a\0b");
        assert_eq!(f.as_str(), "ab");
        assert_eq!(f.cursor(), 2);
        f.replace_all("\0x\0");
        assert_eq!(f.as_str(), "x");
        assert_eq!(f.cursor(), 1);
    }

    #[test]
    fn clear_empties_the_field() {
        let mut f = field("abc", 2);
        f.clear();
        assert!(f.is_empty());
        assert_eq!(f.cursor(), 0);
    }

    #[test]
    fn word_range_at_picks_the_run_under_the_point() {
        let f = field("hello, wörld  ok", 0);
        let word = |i| f.text[f.word_range_at(i)].to_string();
        assert_eq!(word(1), "hello");
        assert_eq!(word(6), ",");
        assert_eq!(word(9), "wörld");
        assert_eq!(word(14), "  ");
        assert_eq!(word(99), "ok");
    }

    #[test]
    fn word_range_on_empty_text_is_empty() {
        let f = Field::default();
        assert_eq!(f.word_range_at(7), 0..0);
    }

    #[test]
    fn every_app_side_edit_ends_the_composition() {
        let compose = |f: &mut Field| f.apply_ime("hello world", 5, 5, Some(0..5));
        let mut f = Field::default();
        for edit in [
            (|f: &mut Field| f.move_to(3)) as fn(&mut Field),
            |f| f.select(0, 2),
            |f| f.replace_all("x"),
            |f| f.clear(),
            |f| f.insert("a"),
            |f| f.backspace(),
            |f| f.move_left(),
            |f| f.select_all(),
        ] {
            compose(&mut f);
            assert_eq!(f.composing(), Some(0..5));
            edit(&mut f);
            assert_eq!(f.composing(), None);
        }
        compose(&mut f);
        f.select(0, 5);
        f.cut();
        assert_eq!(f.composing(), None);
    }

    #[test]
    fn apply_ime_sets_text_selection_and_composing() {
        let mut f = Field::default();
        f.apply_ime("hello world", 6, 11, Some(6..11));
        assert_eq!(f.as_str(), "hello world");
        assert_eq!(f.selection(), Some(6..11));
        assert_eq!(f.composing(), Some(6..11));
    }

    #[test]
    fn apply_ime_drops_empty_and_inverted_composing() {
        let mut f = Field::default();
        f.apply_ime("abc", 1, 1, Some(1..1));
        assert_eq!(f.composing(), None);
        // The input method can report a span whose end precedes its start.
        #[allow(clippy::reversed_empty_ranges)]
        let inverted = Some(2..1);
        f.apply_ime("abc", 1, 1, inverted);
        assert_eq!(f.composing(), None);
    }

    #[test]
    fn apply_ime_strips_nuls_and_shifts_offsets() {
        let mut f = Field::default();
        f.apply_ime("a\0bc\0d", 4, 6, Some(2..4));
        assert_eq!(f.as_str(), "abcd");
        assert_eq!((f.cursor(), f.selection()), (4, Some(3..4)));
        assert_eq!(f.composing(), Some(1..3));
    }

    #[test]
    fn apply_ime_rounds_offsets_to_char_boundaries_and_clamps() {
        let mut f = Field::default();
        f.apply_ime("é", 1, 99, None);
        assert_eq!(f.selection(), Some(0..2));
    }

    #[test]
    fn ime_state_round_trips_with_apply_ime() {
        let mut f = Field::default();
        f.apply_ime("aé b", 1, 3, Some(1..3));
        let (text, anchor, cursor, composing) = f.ime_state();
        let mut g = Field::default();
        g.apply_ime(&text, anchor, cursor, composing);
        assert_eq!(f, g);
    }
}
