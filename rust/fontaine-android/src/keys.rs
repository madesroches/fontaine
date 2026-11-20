//! How a hardware or soft-keyboard key press edits the paste or prompt field. NativeActivity
//! delivers keys, not composed text, so the field is built here from them.

use crate::field::Field;

/// A key press reduced to what the field cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Other,
}

/// Applies one key-down to `field`; returns true when the key asks to submit it.
pub fn apply(field: &mut Field, key: Key) -> bool {
    match key {
        Key::Char(c) if !c.is_control() => field.insert_char(c),
        Key::Backspace => field.backspace(),
        Key::Delete => field.delete_forward(),
        Key::Left => field.move_left(),
        Key::Right => field.move_right(),
        Key::Enter => return !field.is_empty(),
        Key::Char(_) | Key::Other => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str) -> Field {
        let mut field = Field::default();
        field.replace_all(text);
        field
    }

    #[test]
    fn typing_and_backspace_edit_the_field() {
        let mut field = Field::default();
        for key in [
            Key::Char('a'),
            Key::Char('é'),
            Key::Char('b'),
            Key::Backspace,
        ] {
            assert!(!apply(&mut field, key));
        }
        assert_eq!(field.as_str(), "aé");
    }

    #[test]
    fn backspace_on_an_empty_field_is_a_no_op() {
        let mut field = Field::default();
        assert!(!apply(&mut field, Key::Backspace));
        assert!(field.is_empty());
    }

    #[test]
    fn enter_submits_only_a_non_empty_field() {
        let mut field = Field::default();
        assert!(!apply(&mut field, Key::Enter));
        apply(&mut field, Key::Char('x'));
        assert!(apply(&mut field, Key::Enter));
        assert_eq!(field.as_str(), "x");
    }

    #[test]
    fn control_characters_and_other_keys_are_ignored() {
        let mut field = field("x");
        assert!(!apply(&mut field, Key::Char('\n')));
        assert!(!apply(&mut field, Key::Other));
        assert_eq!(field.as_str(), "x");
    }

    #[test]
    fn typing_and_backspace_act_at_the_cursor() {
        let mut field = field("helo");
        field.move_to(3);
        apply(&mut field, Key::Char('l'));
        assert_eq!(field.as_str(), "hello");
        apply(&mut field, Key::Backspace);
        assert_eq!(field.as_str(), "helo");
        assert_eq!(field.cursor(), 3);
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut field = field("hello world");
        field.select(0, 5);
        apply(&mut field, Key::Char('J'));
        assert_eq!(field.as_str(), "J world");
    }

    #[test]
    fn left_right_and_delete_move_and_delete() {
        let mut field = field("abc");
        apply(&mut field, Key::Left);
        apply(&mut field, Key::Left);
        assert_eq!(field.cursor(), 1);
        apply(&mut field, Key::Delete);
        assert_eq!(field.as_str(), "ac");
        apply(&mut field, Key::Right);
        assert_eq!(field.cursor(), 2);
        apply(&mut field, Key::Delete);
        assert_eq!(field.as_str(), "ac");
    }
}
