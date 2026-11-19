//! How a hardware or soft-keyboard key press edits the paste or prompt field. NativeActivity
//! delivers keys, not composed text, so the field is built here from them.

/// A key press reduced to what the field cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Other,
}

/// Applies one key-down to `field`; returns true when the key asks to submit it.
pub fn apply(field: &mut String, key: Key) -> bool {
    match key {
        Key::Char(c) if !c.is_control() => field.push(c),
        Key::Backspace => {
            field.pop();
        }
        Key::Enter => return !field.is_empty(),
        Key::Char(_) | Key::Other => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_backspace_edit_the_field() {
        let mut field = String::new();
        for key in [
            Key::Char('a'),
            Key::Char('é'),
            Key::Char('b'),
            Key::Backspace,
        ] {
            assert!(!apply(&mut field, key));
        }
        assert_eq!(field, "aé");
    }

    #[test]
    fn backspace_on_an_empty_field_is_a_no_op() {
        let mut field = String::new();
        assert!(!apply(&mut field, Key::Backspace));
        assert!(field.is_empty());
    }

    #[test]
    fn enter_submits_only_a_non_empty_field() {
        let mut field = String::new();
        assert!(!apply(&mut field, Key::Enter));
        apply(&mut field, Key::Char('x'));
        assert!(apply(&mut field, Key::Enter));
        assert_eq!(field, "x");
    }

    #[test]
    fn control_characters_and_other_keys_are_ignored() {
        let mut field = String::from("x");
        assert!(!apply(&mut field, Key::Char('\n')));
        assert!(!apply(&mut field, Key::Other));
        assert_eq!(field, "x");
    }
}
