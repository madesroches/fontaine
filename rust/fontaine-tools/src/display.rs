//! Making untrusted text safe to print on a terminal.

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

#[cfg(test)]
mod tests {
    use super::escape_controls;

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
