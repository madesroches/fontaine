//! Turns a burst of key events that contains a newline into one paste.
//!
//! The Windows console reader never produces `Event::Paste`, so pasted text arrives as `Char` and
//! `Enter` keys and every line would submit. A pasted block lands in the console input queue in one
//! write, so successive reads are far closer together than a human can type.

use std::io;
use std::time::Duration;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::Input;

/// The longest pause between two events of one burst.
pub(crate) const BURST_GAP: Duration = Duration::from_millis(15);

pub(crate) fn enabled() -> bool {
    cfg!(windows)
}

/// The text a burst key stands for, or `None` when the key is not a burst key.
fn burst_text(key: &KeyEvent) -> Option<char> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mods = key.modifiers;
    // Windows reports AltGr as CONTROL|ALT.
    let altgr = mods.contains(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Char(c)
            if altgr || !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(c)
        }
        KeyCode::Enter if mods.is_empty() => Some('\n'),
        KeyCode::Tab if mods.is_empty() => Some('\t'),
        _ => None,
    }
}

pub(crate) fn is_burst_key(key: &KeyEvent) -> bool {
    burst_text(key).is_some()
}

fn event_input(event: Event) -> Option<Input> {
    match event {
        Event::Key(key) => Some(Input::Key(key)),
        Event::Paste(text) => Some(Input::Paste(text)),
        Event::Resize(..) => Some(Input::Resize),
        _ => None,
    }
}

/// Collects the burst that `first` starts and returns the inputs to send, in order. `next` waits
/// up to the given gap for one more event and returns `None` when none came, or when the reader
/// must stop or pause. An error from `next` follows the inputs collected so far.
pub(crate) fn coalesce(
    first: KeyEvent,
    mut next: impl FnMut(Duration) -> io::Result<Option<Event>>,
) -> (Vec<Input>, Option<io::Error>) {
    let mut keys = vec![first];
    let mut trailing = None;
    let mut error = None;
    loop {
        match next(BURST_GAP) {
            Ok(None) => break,
            Ok(Some(Event::Key(key))) if key.kind == KeyEventKind::Release => {}
            Ok(Some(Event::Key(key))) if is_burst_key(&key) => keys.push(key),
            Ok(Some(event)) => {
                trailing = event_input(event);
                break;
            }
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }

    let newline = keys.iter().any(|k| k.code == KeyCode::Enter);
    let mut inputs = if keys.len() >= 2 && newline {
        let text = keys.iter().filter_map(burst_text).collect();
        vec![Input::Paste(text)]
    } else {
        keys.into_iter().map(Input::Key).collect()
    };
    inputs.extend(trailing);
    (inputs, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn enter() -> KeyEvent {
        key(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn release(c: char) -> KeyEvent {
        KeyEvent::new_with_kind(KeyCode::Char(c), KeyModifiers::NONE, KeyEventKind::Release)
    }

    /// Runs `coalesce` over a script; `None` entries are gaps.
    fn run(
        first: KeyEvent,
        script: Vec<io::Result<Option<Event>>>,
    ) -> (Vec<Input>, Option<io::Error>) {
        let mut script: VecDeque<_> = script.into();
        coalesce(first, |_| script.pop_front().unwrap_or(Ok(None)))
    }

    fn events(events: Vec<Event>) -> Vec<io::Result<Option<Event>>> {
        events.into_iter().map(|e| Ok(Some(e))).collect()
    }

    fn pasted(inputs: &[Input]) -> Option<&str> {
        match inputs {
            [Input::Paste(text)] => Some(text),
            _ => None,
        }
    }

    #[test]
    fn a_multi_line_burst_is_one_paste_and_never_submits() {
        let script = events(vec![
            Event::Key(ch('b')),
            Event::Key(enter()),
            Event::Key(ch('c')),
            Event::Key(ch('d')),
        ]);
        let (inputs, error) = run(ch('a'), script);
        assert!(error.is_none());
        assert_eq!(pasted(&inputs), Some("ab\ncd"));
    }

    #[test]
    fn two_enters_are_a_paste_of_two_newlines() {
        let (inputs, _) = run(enter(), events(vec![Event::Key(enter())]));
        assert_eq!(pasted(&inputs), Some("\n\n"));
    }

    #[test]
    fn a_lone_enter_stays_a_key() {
        let (inputs, _) = run(enter(), vec![]);
        assert!(matches!(inputs.as_slice(), [Input::Key(k)] if k.code == KeyCode::Enter));
    }

    #[test]
    fn a_gap_before_enter_leaves_the_enter_unread() {
        let (inputs, _) = run(ch('a'), vec![Ok(None), Ok(Some(Event::Key(enter())))]);
        assert!(matches!(inputs.as_slice(), [Input::Key(k)] if k.code == KeyCode::Char('a')));
    }

    #[test]
    fn a_burst_without_a_newline_is_replayed_as_keys() {
        let (inputs, _) = run(
            ch('a'),
            events(vec![Event::Key(ch('b')), Event::Key(ch('c'))]),
        );
        let codes: Vec<_> = inputs
            .iter()
            .map(|i| match i {
                Input::Key(k) => k.code,
                other => panic!("expected a key, got {other:?}"),
            })
            .collect();
        assert_eq!(
            codes,
            [KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')]
        );
    }

    #[test]
    fn a_trailing_newline_is_kept() {
        let (inputs, _) = run(ch('a'), events(vec![Event::Key(enter())]));
        assert_eq!(pasted(&inputs), Some("a\n"));
    }

    #[test]
    fn tab_joins_a_burst_as_a_tab() {
        let tab = key(KeyCode::Tab, KeyModifiers::NONE);
        let (inputs, _) = run(ch('a'), events(vec![Event::Key(tab), Event::Key(enter())]));
        assert_eq!(pasted(&inputs), Some("a\t\n"));
    }

    #[test]
    fn shift_chars_join_a_burst() {
        let upper = key(KeyCode::Char('A'), KeyModifiers::SHIFT);
        let (inputs, _) = run(upper, events(vec![Event::Key(enter())]));
        assert_eq!(pasted(&inputs), Some("A\n"));
    }

    #[test]
    fn altgr_joins_a_burst_as_its_char() {
        let altgr = key(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let (inputs, _) = run(
            ch('a'),
            events(vec![Event::Key(altgr), Event::Key(enter())]),
        );
        assert_eq!(pasted(&inputs), Some("a@\n"));
    }

    #[test]
    fn pure_ctrl_or_alt_chars_neither_start_nor_join_a_burst() {
        let ctrl = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let alt = key(KeyCode::Char('b'), KeyModifiers::ALT);
        assert!(!is_burst_key(&ctrl));
        assert!(!is_burst_key(&alt));

        let (inputs, _) = run(ch('a'), events(vec![Event::Key(ctrl), Event::Key(enter())]));
        assert!(matches!(
            inputs.as_slice(),
            [Input::Key(a), Input::Key(c)] if a.code == KeyCode::Char('a') && c == &ctrl
        ));
    }

    #[test]
    fn a_release_is_dropped_and_does_not_end_the_burst() {
        let script = events(vec![
            Event::Key(release('a')),
            Event::Key(enter()),
            Event::Key(release('b')),
        ]);
        let (inputs, _) = run(ch('a'), script);
        assert_eq!(pasted(&inputs), Some("a\n"));
    }

    #[test]
    fn releases_are_dropped_from_a_replayed_burst() {
        let (inputs, _) = run(
            ch('a'),
            events(vec![Event::Key(release('a')), Event::Key(ch('b'))]),
        );
        assert_eq!(inputs.len(), 2);
        assert!(
            inputs
                .iter()
                .all(|i| matches!(i, Input::Key(k) if k.kind != KeyEventKind::Release))
        );
    }

    #[test]
    fn a_release_resets_the_gap_timer() {
        let mut gaps = Vec::new();
        let mut script: VecDeque<Option<Event>> = vec![
            Some(Event::Key(release('a'))),
            Some(Event::Key(enter())),
            None,
        ]
        .into();
        coalesce(ch('a'), |gap| {
            gaps.push(gap);
            Ok(script.pop_front().flatten())
        });
        assert_eq!(gaps, [BURST_GAP; 3]);
    }

    #[test]
    fn a_non_burst_event_that_ends_a_burst_is_delivered_after_it() {
        let ctrl = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let script = events(vec![
            Event::Key(enter()),
            Event::Key(ctrl),
            Event::Key(ch('z')),
        ]);
        let (inputs, _) = run(ch('a'), script);
        assert!(matches!(
            inputs.as_slice(),
            [Input::Paste(text), Input::Key(k)] if text == "a\n" && k == &ctrl
        ));
    }

    #[test]
    fn a_resize_that_ends_a_burst_is_delivered_after_it() {
        let (inputs, _) = run(
            ch('a'),
            events(vec![Event::Key(enter()), Event::Resize(80, 24)]),
        );
        assert!(matches!(
            inputs.as_slice(),
            [Input::Paste(_), Input::Resize]
        ));
    }

    #[test]
    fn an_error_flushes_the_collected_inputs_first() {
        let script = vec![
            Ok(Some(Event::Key(enter()))),
            Err(io::Error::other("console closed")),
        ];
        let (inputs, error) = run(ch('a'), script);
        assert_eq!(pasted(&inputs), Some("a\n"));
        assert_eq!(
            error.expect("the error is returned").to_string(),
            "console closed"
        );
    }
}
