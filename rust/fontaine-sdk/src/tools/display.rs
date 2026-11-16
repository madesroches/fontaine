//! Making untrusted text safe to print on a terminal, and values readable on one.

/// Shows control characters as escapes. Wrapping chunks by width and ratatui drops control
/// characters, so a rule's newlines and tabs would run its text together, and a raw escape
/// sequence from a settings file would reach the terminal; the stored text keeps them.
pub fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

/// Renders a count for a human: exact below 1000, then one decimal in thousands (`90.9k`) or
/// millions (`1.2M`), rounded half up. Machine-readable output keeps exact values.
pub fn compact_count(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        let tenths = n.saturating_add(50) / 100;
        // Rounding up to 1000.0k reads better as 1.0M.
        if tenths < 10_000 {
            return format!("{}.{}k", tenths / 10, tenths % 10);
        }
    }
    let tenths = n.saturating_add(50_000) / 100_000;
    format!("{}.{}M", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::{compact_count, escape_controls};

    #[test]
    fn counts_are_compact() {
        for (n, want) in [
            (0, "0"),
            (999, "999"),
            (1000, "1.0k"),
            (1049, "1.0k"),
            (1050, "1.1k"),
            (9950, "10.0k"),
            (90_887, "90.9k"),
            (99_949, "99.9k"),
            (999_949, "999.9k"),
            (999_950, "1.0M"),
            (999_999, "1.0M"),
            (1_000_000, "1.0M"),
            (1_234_567, "1.2M"),
        ] {
            assert_eq!(compact_count(n), want, "{n}");
        }
        assert!(compact_count(u64::MAX).ends_with('M'));
    }

    #[test]
    fn newlines_are_shown_as_markers() {
        assert_eq!(escape_controls("a\nb"), "a\\nb");
        assert_eq!(escape_controls("a\r\nb"), "a\\nb");
        assert_eq!(escape_controls("a\rb"), "a\\rb");
        assert_eq!(escape_controls("shell(ls -la)"), "shell(ls -la)");
    }

    #[test]
    fn tabs_and_other_control_characters_are_escaped() {
        assert_eq!(escape_controls("cut -d'\t' -f2"), "cut -d'\\t' -f2");
        assert_eq!(escape_controls("a\x1bb\x7f"), "a\\u{1b}b\\u{7f}");
    }
}
