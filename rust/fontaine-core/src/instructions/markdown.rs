//! The two things instruction files need from markdown: dropping block-level HTML comments and
//! finding `@path` imports. Both leave fenced code blocks alone, and imports also skip inline
//! code spans.

fn split_line(text: &str) -> (&str, &str) {
    match text.find('\n') {
        Some(at) => text.split_at(at + 1),
        None => (text, ""),
    }
}

/// The fence character and length a line opens, if it opens one.
fn opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let c = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|&x| x == c).count();
    if len < 3 {
        return None;
    }
    // A backtick fence's info string cannot hold a backtick; otherwise it is an inline span.
    if c == '`' && trimmed[len..].contains('`') {
        return None;
    }
    Some((c, len))
}

fn closes_fence(line: &str, (c, len): (char, usize)) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= len && trimmed.chars().all(|x| x == c)
}

/// `text` without the HTML comments that start a line, outside code blocks. A comment that is
/// never closed stays as written.
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut fence: Option<(char, usize)> = None;
    while !rest.is_empty() {
        let (line, tail) = split_line(rest);
        if let Some(open) = fence {
            out.push_str(line);
            if closes_fence(line, open) {
                fence = None;
            }
            rest = tail;
            continue;
        }
        if let Some(open) = opening_fence(line) {
            fence = Some(open);
            out.push_str(line);
            rest = tail;
            continue;
        }
        let trimmed = rest.trim_start_matches([' ', '\t']);
        if let Some(after) = trimmed.strip_prefix("<!--")
            && let Some(end) = after.find("-->")
        {
            rest = &after[end + 3..];
            continue;
        }
        out.push_str(line);
        rest = tail;
    }
    out
}

/// The `@path` imports in `text`, in order. An import is an `@` at the start of a line or after
/// whitespace, followed by a path that runs to the next whitespace; `\ ` is a space inside it.
pub fn imports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        if let Some(open) = fence {
            if closes_fence(line, open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = opening_fence(line) {
            fence = Some(open);
            continue;
        }
        line_imports(line, &mut found);
    }
    found
}

fn line_imports(line: &str, found: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut at = 0;
    let mut after_space = true;
    while at < chars.len() {
        let c = chars[at];
        if c == '`' {
            let run = chars[at..].iter().take_while(|&&x| x == '`').count();
            at = span_end(&chars, at + run, run).unwrap_or(at + run);
            after_space = false;
            continue;
        }
        if c == '@' && after_space {
            let mut path = String::new();
            let mut end = at + 1;
            while end < chars.len() {
                if chars[end] == '\\' && chars.get(end + 1) == Some(&' ') {
                    path.push(' ');
                    end += 2;
                } else if chars[end].is_whitespace() {
                    break;
                } else {
                    path.push(chars[end]);
                    end += 1;
                }
            }
            if !path.is_empty() && !path.starts_with(['"', '\'', '`']) {
                found.push(path);
            }
            at = end;
            after_space = false;
            continue;
        }
        after_space = c.is_whitespace();
        at += 1;
    }
}

/// Where the code span opened by a run of `run` backticks ends, searching from `from`.
fn span_end(chars: &[char], from: usize, run: usize) -> Option<usize> {
    let mut at = from;
    while at < chars.len() {
        if chars[at] == '`' {
            let len = chars[at..].iter().take_while(|&&x| x == '`').count();
            if len == run {
                return Some(at + len);
            }
            at += len;
        } else {
            at += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_at_line_start_and_after_whitespace() {
        let found = imports("@a.md\nsee @b.md and @~/c.md\n  @/abs/d.md");
        assert_eq!(found, ["a.md", "b.md", "~/c.md", "/abs/d.md"]);
    }

    #[test]
    fn no_imports_in_fences_spans_emails_or_quotes() {
        let text = "```\n@fenced.md\n```\n~~~sh\n@tilde.md\n~~~\n`@span.md` and ``a ` @x.md``\nmail user@example.com\n@\"quoted.md\"\n@'single.md'\n@ alone\nreal @ok.md";
        assert_eq!(imports(text), ["ok.md"]);
    }

    #[test]
    fn a_fence_needs_a_matching_closer() {
        let text = "````\n```\n@inside.md\n````\n@after.md";
        assert_eq!(imports(text), ["after.md"]);
        assert!(imports("```\n@never-closed.md").is_empty());
    }

    #[test]
    fn a_backslash_escapes_a_space() {
        assert_eq!(imports("@my\\ notes/a\\ b.md next"), ["my notes/a b.md"]);
    }

    #[test]
    fn strips_block_comments_but_not_inside_fences() {
        let text =
            "keep\n<!-- one line -->\n<!--\nmulti\nline\n-->\nafter\n```\n<!-- code -->\n```\n";
        let stripped = strip_comments(text);
        assert!(!stripped.contains("one line"));
        assert!(!stripped.contains("multi"));
        assert!(stripped.contains("<!-- code -->"));
        assert!(stripped.contains("keep") && stripped.contains("after"));
    }

    #[test]
    fn keeps_text_after_a_comment_and_an_unterminated_comment() {
        assert_eq!(strip_comments("<!-- c --> tail\n").trim(), "tail");
        assert_eq!(strip_comments("<!-- open\nstill\n"), "<!-- open\nstill\n");
        assert_eq!(
            strip_comments("inline <!-- x --> stays\n"),
            "inline <!-- x --> stays\n"
        );
    }

    #[test]
    fn an_unterminated_fence_runs_to_the_end() {
        let text = "```\n<!-- kept -->\n";
        assert_eq!(strip_comments(text), text);
    }

    #[test]
    fn imports_inside_a_stripped_comment_are_gone() {
        let stripped = strip_comments("<!-- @hidden.md -->\n@shown.md\n");
        assert_eq!(imports(&stripped), ["shown.md"]);
    }
}
