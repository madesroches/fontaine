//! The input method's view of the field and the sync between the two. Java mirrors the field in an
//! `Editable` the keyboard edits and reports in UTF-16 code units; the field is in bytes and also
//! changes from taps, keys and paste. `ImeSync` decides which side has news so a change is
//! never echoed back to where it came from.

use crate::field::Field;

/// The text, selection and composing span as Java reports them, in UTF-16 code units.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImeState {
    pub text: String,
    /// Anchor and cursor, in that order.
    pub sel: (usize, usize),
    pub composing: Option<(usize, usize)>,
}

/// The byte offset of the char that starts at or contains `units` UTF-16 units into `text`; an
/// offset inside a surrogate pair rounds down, and one past the end clamps.
pub fn utf16_to_byte(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (at, c) in text.char_indices() {
        if seen >= units || seen + c.len_utf16() > units {
            return at;
        }
        seen += c.len_utf16();
    }
    text.len()
}

/// The UTF-16 offset of the char boundary at or before `byte`; clamps past the end.
pub fn byte_to_utf16(text: &str, byte: usize) -> usize {
    text.char_indices()
        .take_while(|(at, c)| at + c.len_utf8() <= byte)
        .map(|(_, c)| c.len_utf16())
        .sum()
}

impl ImeState {
    pub fn apply_to(&self, field: &mut Field) {
        let byte = |units| utf16_to_byte(&self.text, units);
        field.apply_ime(
            &self.text,
            byte(self.sel.0),
            byte(self.sel.1),
            self.composing.map(|(start, end)| byte(start)..byte(end)),
        );
    }

    pub fn from_field(field: &Field) -> Self {
        let (text, anchor, cursor, composing) = field.ime_state();
        let units = |byte| byte_to_utf16(&text, byte);
        Self {
            sel: (units(anchor), units(cursor)),
            composing: composing.map(|r| (units(r.start), units(r.end))),
            text,
        }
    }
}

/// Which side of the field/mirror pair has news. `generation` counts the pushes sent to Java; Java echoes
/// the last one it applied in every report, so a report built before a push landed can be told
/// from one built after it.
#[derive(Debug, Default)]
pub struct ImeSync {
    last: Option<ImeState>,
    generation: u32,
    wholesale: bool,
}

impl ImeSync {
    /// Applies a report from Java. The state recorded as already known is the field's own
    /// normalized form (NULs stripped, offsets rounded), so normalization is not echoed back. A
    /// report older than the last push is applied too, so the IME's edit is not lost, but it is
    /// forgotten so the field is pushed again and Java converges.
    pub fn incoming(&mut self, field: &mut Field, state: ImeState, report_generation: u32) {
        state.apply_to(field);
        if report_generation >= self.generation {
            self.last = Some(ImeState::from_field(field));
        } else {
            self.last = None;
            self.wholesale = true;
        }
    }

    /// The field's state when Java does not have it yet. Pure: a push that failed is offered
    /// again until `mark_sent`.
    pub fn outgoing(&self, field: &Field) -> Option<ImeState> {
        let state = ImeState::from_field(field);
        (self.last.as_ref() != Some(&state)).then_some(state)
    }

    /// The generation to tag the next push with.
    pub fn next_gen(&self) -> u32 {
        self.generation + 1
    }

    /// Whether the next push replaces the IME's session instead of updating its cursor.
    pub fn wholesale(&self) -> bool {
        self.wholesale
    }

    /// Marks the text as replaced rather than edited (submit, paste, cut), so the IME restarts.
    pub fn note_wholesale(&mut self) {
        self.wholesale = true;
    }

    /// Commits a push that reached Java.
    pub fn mark_sent(&mut self, state: ImeState) {
        self.last = Some(state);
        self.generation += 1;
        self.wholesale = false;
    }
}

/// What Java has reported since the last frame: the newest state wins, a submit is sticky.
#[derive(Debug, Default)]
pub struct Pending {
    state: Option<(ImeState, u32)>,
    submit: bool,
}

impl Pending {
    pub const fn new() -> Self {
        Self {
            state: None,
            submit: false,
        }
    }

    pub fn set_state(&mut self, state: ImeState, generation: u32) {
        self.state = Some((state, generation));
    }

    pub fn set_submit(&mut self) {
        self.submit = true;
    }

    pub fn take(&mut self) -> (Option<(ImeState, u32)>, bool) {
        (self.state.take(), std::mem::take(&mut self.submit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str, sel: (usize, usize), composing: Option<(usize, usize)>) -> ImeState {
        ImeState {
            text: text.to_string(),
            sel,
            composing,
        }
    }

    #[test]
    fn conversions_on_ascii_are_the_identity() {
        assert_eq!(utf16_to_byte("abc", 2), 2);
        assert_eq!(byte_to_utf16("abc", 2), 2);
    }

    #[test]
    fn conversions_count_bmp_multi_byte_chars_once() {
        let text = "aéb";
        assert_eq!(utf16_to_byte(text, 2), 3);
        assert_eq!(byte_to_utf16(text, 3), 2);
        assert_eq!(byte_to_utf16(text, 2), 1);
    }

    #[test]
    fn conversions_count_astral_chars_as_a_pair() {
        let text = "a😀b";
        assert_eq!(utf16_to_byte(text, 3), 5);
        assert_eq!(byte_to_utf16(text, 5), 3);
        assert_eq!(byte_to_utf16(text, 3), 1);
    }

    #[test]
    fn an_offset_inside_a_surrogate_pair_rounds_down() {
        assert_eq!(utf16_to_byte("a😀b", 2), 1);
    }

    #[test]
    fn out_of_range_offsets_clamp() {
        assert_eq!(utf16_to_byte("a😀", 99), 5);
        assert_eq!(byte_to_utf16("a😀", 99), 3);
        assert_eq!(utf16_to_byte("", 4), 0);
    }

    #[test]
    fn apply_to_and_from_field_round_trip() {
        let original = state("a😀é b", (1, 4), Some((3, 4)));
        let mut field = Field::default();
        original.apply_to(&mut field);
        assert_eq!(field.selection(), Some(1..7));
        assert_eq!(field.composing(), Some(5..7));
        assert_eq!(ImeState::from_field(&field), original);
    }

    #[test]
    fn nothing_is_pushed_back_after_incoming_of_the_same_state() {
        let mut field = Field::default();
        let mut sync = ImeSync::default();
        sync.incoming(&mut field, state("hi", (2, 2), Some((0, 2))), 0);
        assert_eq!(sync.outgoing(&field), None);
    }

    #[test]
    fn an_unchanged_field_yields_nothing_after_a_push() {
        let mut field = Field::default();
        field.insert("hi");
        let mut sync = ImeSync::default();
        let pushed = sync.outgoing(&field).expect("first push");
        sync.mark_sent(pushed);
        assert_eq!(sync.outgoing(&field), None);
    }

    #[test]
    fn a_tap_paste_or_clear_is_pushed() {
        let mut field = Field::default();
        let mut sync = ImeSync::default();
        sync.incoming(&mut field, state("hello", (5, 5), None), 0);
        field.move_to(1);
        assert_eq!(sync.outgoing(&field).map(|s| s.sel), Some((1, 1)));
        sync.incoming(&mut field, state("hello", (5, 5), None), 0);
        field.replace_all("pasted");
        assert_eq!(sync.outgoing(&field).expect("paste").text, "pasted");
        sync.incoming(&mut field, state("pasted", (6, 6), None), 0);
        field.clear();
        assert_eq!(sync.outgoing(&field).expect("clear").text, "");
    }

    #[test]
    fn normalization_is_not_echoed() {
        for incoming in [
            state("a\0b", (3, 3), None),
            state("a😀", (2, 2), None),
            state("abc", (3, 3), Some((2, 1))),
        ] {
            let mut field = Field::default();
            let mut sync = ImeSync::default();
            sync.incoming(&mut field, incoming, 0);
            assert_eq!(sync.outgoing(&field), None);
        }
    }

    #[test]
    fn outgoing_is_pure_until_mark_sent() {
        let mut field = Field::default();
        field.insert("x");
        let mut sync = ImeSync::default();
        let first = sync.outgoing(&field).expect("push");
        assert_eq!(sync.outgoing(&field), Some(first.clone()));
        assert_eq!(sync.next_gen(), 1);
        sync.mark_sent(first);
        assert_eq!(sync.outgoing(&field), None);
        assert_eq!(sync.next_gen(), 2);
    }

    #[test]
    fn the_wholesale_flag_lasts_until_the_push_is_sent() {
        let mut field = Field::default();
        let mut sync = ImeSync::default();
        assert!(!sync.wholesale());
        sync.note_wholesale();
        assert!(sync.wholesale());
        field.insert("x");
        let pushed = sync.outgoing(&field).expect("push");
        assert!(sync.wholesale());
        sync.mark_sent(pushed);
        assert!(!sync.wholesale());
    }

    #[test]
    fn a_stale_report_is_applied_and_the_field_is_pushed_again() {
        let mut field = Field::default();
        let mut sync = ImeSync::default();
        sync.incoming(&mut field, state("ab", (2, 2), None), 0);
        field.insert("c");
        let pushed = sync.outgoing(&field).expect("push");
        sync.mark_sent(pushed);
        // Built on the mirror from before the push landed.
        sync.incoming(&mut field, state("abd", (3, 3), None), 0);
        assert_eq!(field.as_str(), "abd");
        assert_eq!(sync.outgoing(&field).expect("re-push").text, "abd");
        assert!(sync.wholesale());
    }

    #[test]
    fn a_report_of_the_current_generation_is_not_pushed_back() {
        let mut field = Field::default();
        let mut sync = ImeSync::default();
        field.insert("c");
        let pushed = sync.outgoing(&field).expect("push");
        sync.mark_sent(pushed);
        sync.incoming(&mut field, state("cd", (2, 2), None), 1);
        assert_eq!(sync.outgoing(&field), None);
        assert!(!sync.wholesale());
    }

    #[test]
    fn pending_keeps_the_newest_state_and_a_sticky_submit() {
        let mut pending = Pending::new();
        assert_eq!(pending.take(), (None, false));
        pending.set_state(state("a", (1, 1), None), 1);
        pending.set_state(state("ab", (2, 2), None), 2);
        pending.set_submit();
        pending.set_state(state("abc", (3, 3), None), 3);
        let (taken, submit) = pending.take();
        assert_eq!(taken, Some((state("abc", (3, 3), None), 3)));
        assert!(submit);
        assert_eq!(pending.take(), (None, false));
    }

    #[test]
    fn a_swipe_typed_sentence_end_to_end() {
        let mut field = Field::default();
        let mut sync = ImeSync::default();
        sync.incoming(&mut field, state("hello", (5, 5), Some((0, 5))), 0);
        assert_eq!(field.composing(), Some(0..5));
        sync.incoming(&mut field, state("help", (4, 4), Some((0, 4))), 0);
        assert_eq!(field.as_str(), "help");
        sync.incoming(&mut field, state("help ", (5, 5), None), 0);
        assert_eq!(field.composing(), None);
        assert_eq!(sync.outgoing(&field), None);
        sync.incoming(&mut field, state("help wor", (8, 8), Some((5, 8))), 0);
        field.move_to(2);
        assert_eq!(field.composing(), None);
        assert_eq!(sync.outgoing(&field).map(|s| s.composing), Some(None));
    }
}
