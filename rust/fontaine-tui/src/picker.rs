//! The list navigation the filterable overlay pickers share. The rows and what Enter picks
//! differ too much for a common widget, so these are plain functions.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The most rows a picker body shows at once.
pub(crate) const MAX_ROWS: usize = 14;

/// The selection one row up or down, wrapping at both ends.
pub(crate) fn step(selected: usize, count: usize, up: bool) -> usize {
    if count == 0 {
        selected
    } else if up {
        (selected + count - 1) % count
    } else {
        (selected + 1) % count
    }
}

/// Applies Backspace or a typed character (shifted is fine, Ctrl and Alt are not) to the filter
/// and moves the selection back to the first match. Returns whether the key was consumed.
pub(crate) fn edit_query(query: &mut String, selected: &mut usize, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Backspace => {
            query.pop();
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            query.push(c);
        }
        _ => return false,
    }
    *selected = 0;
    true
}

/// The first of `window` rows to show out of `total`, keeping the selected row near the middle.
pub(crate) fn window_start(selected_row: usize, total: usize, window: usize) -> usize {
    if total <= window {
        0
    } else {
        selected_row.saturating_sub(window / 2).min(total - window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_and_an_empty_list_stays_put() {
        assert_eq!(step(0, 3, true), 2);
        assert_eq!(step(2, 3, false), 0);
        assert_eq!(step(1, 3, false), 2);
        assert_eq!(step(5, 0, true), 5);
        assert_eq!(step(5, 0, false), 5);
    }

    #[test]
    fn the_window_centres_on_the_selection_and_stops_at_the_end() {
        assert_eq!(window_start(3, 5, 14), 0);
        assert_eq!(window_start(3, 30, 14), 0);
        assert_eq!(window_start(10, 30, 14), 3);
        assert_eq!(window_start(29, 30, 14), 16);
    }

    #[test]
    fn editing_resets_the_selection_and_ignores_modified_characters() {
        let mut query = String::new();
        let mut selected = 4;
        let key = |code, mods| KeyEvent::new(code, mods);
        assert!(edit_query(
            &mut query,
            &mut selected,
            &key(KeyCode::Char('a'), KeyModifiers::SHIFT)
        ));
        assert_eq!((query.as_str(), selected), ("a", 0));
        selected = 2;
        assert!(!edit_query(
            &mut query,
            &mut selected,
            &key(KeyCode::Char('b'), KeyModifiers::CONTROL)
        ));
        assert!(!edit_query(
            &mut query,
            &mut selected,
            &key(KeyCode::Enter, KeyModifiers::NONE)
        ));
        assert_eq!((query.as_str(), selected), ("a", 2));
        assert!(edit_query(
            &mut query,
            &mut selected,
            &key(KeyCode::Backspace, KeyModifiers::NONE)
        ));
        assert_eq!((query.as_str(), selected), ("", 0));
    }
}
