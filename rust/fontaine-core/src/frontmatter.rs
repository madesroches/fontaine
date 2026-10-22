//! The YAML header of a skill, agent or output-style file, read as far as Claude Code's files
//! use it: top-level keys whose values are a plain or quoted scalar, a flow list
//! (`[a, b]`), a block list (`- a`) or a block scalar (`|`, `>`). A nested value is noted and
//! skipped. Reading only this subset keeps files that Claude Code accepts loadable, such as
//! `argument-hint: [issue-number]`, which a strict YAML reader would turn into a list.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Scalar(String),
    List(Vec<String>),
    /// A mapping or a deeper structure; only its presence is kept.
    Nested,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    entries: Vec<(String, Value)>,
    /// The text after the header.
    pub body: String,
}

impl Document {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The key as text; a list reads as its items joined with commas.
    pub fn text(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            Value::Scalar(text) => Some(text.clone()),
            Value::List(items) => Some(items.join(", ")),
            Value::Nested => None,
        }
    }

    pub fn flag(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Scalar(text)) => match text.to_ascii_lowercase().as_str() {
                "true" | "yes" => Ok(Some(true)),
                "false" | "no" => Ok(Some(false)),
                _ => Err(format!("`{key}` is `{text}`, not true or false")),
            },
            Some(_) => Err(format!("`{key}` is not true or false")),
        }
    }

    /// The key as a list of items: a YAML list, or one string whose comma-separated items are
    /// split outside parentheses.
    pub fn list(&self, key: &str) -> Option<Vec<String>> {
        match self.get(key)? {
            Value::List(items) => Some(items.clone()),
            Value::Scalar(text) => Some(split_commas(text)),
            Value::Nested => Some(Vec::new()),
        }
    }
}

/// Splits `text` at commas that are not inside parentheses.
pub fn split_commas(text: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => {
                items.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    items.push(current);
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    for quote in ['"', '\''] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(quote) {
            return text[1..text.len() - 1].to_string();
        }
    }
    text.to_string()
}

fn strip_comment(text: &str) -> &str {
    // A comment starts at ` #` outside quotes; values here rarely hold quotes and `#`.
    let mut quote: Option<char> = None;
    let mut previous = ' ';
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), c) if c == open => quote = None,
            (None, '#') if previous.is_whitespace() => return text[..at].trim_end(),
            _ => {}
        }
        previous = c;
    }
    text
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Splits `text` into its header and body. A file with no `---` header has an empty header
/// and the whole text as its body.
pub fn parse(text: &str) -> Result<Document, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let normalized = text.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return Ok(Document {
            entries: Vec::new(),
            body: normalized,
        });
    };
    let (header, body) = match rest.find("\n---") {
        Some(at) => {
            let after = &rest[at + 4..];
            let body = after.strip_prefix('\n').unwrap_or(after);
            if !after.is_empty() && !after.starts_with('\n') {
                return Err("the closing `---` of the header must stand on its own line".into());
            }
            (&rest[..at], body)
        }
        None if rest.trim_end() == "---" => ("", ""),
        None => return Err("the header is not closed with `---`".into()),
    };
    let lines: Vec<&str> = header.lines().collect();
    let mut entries: Vec<(String, Value)> = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            at += 1;
            continue;
        }
        if indent_of(line) > 0 || line.trim_start().starts_with("- ") {
            return Err(format!("unexpected line in the header: `{}`", line.trim()));
        }
        let Some((key, rest)) = line.split_once(':') else {
            return Err(format!("expected `key: value` in the header, got `{line}`"));
        };
        let key = key.trim().to_string();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(format!("`{key}` is not a header key"));
        }
        at += 1;
        // What belongs to this key: the lines that follow it, indented or `- ` items.
        let mut block: Vec<&str> = Vec::new();
        while at < lines.len() {
            let next = lines[at];
            if next.trim().is_empty() {
                block.push(next);
                at += 1;
                continue;
            }
            if indent_of(next) > 0 || next.trim_start().starts_with("- ") {
                block.push(next);
                at += 1;
            } else {
                break;
            }
        }
        while block.last().is_some_and(|l| l.trim().is_empty()) {
            block.pop();
        }
        let rest = strip_comment(rest).trim();
        let value = if let Some(style) = rest.strip_prefix(['|', '>']).map(|_| rest) {
            let fold = style.starts_with('>');
            let indent = block
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| indent_of(l))
                .min()
                .unwrap_or(0);
            let lines: Vec<&str> = block
                .iter()
                .map(|l| l.get(indent..).unwrap_or("").trim_end())
                .collect();
            Value::Scalar(if fold {
                lines
                    .split(|l| l.is_empty())
                    .map(|paragraph| paragraph.join(" "))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string()
            } else {
                lines.join("\n").trim_end().to_string()
            })
        } else if rest.is_empty() {
            // YAML ignores comment lines, so one inside a block list does not make it a mapping.
            block.retain(|l| !l.trim_start().starts_with('#'));
            if block
                .iter()
                .all(|l| l.trim_start().starts_with("- ") || l.trim().is_empty())
            {
                Value::List(
                    block
                        .iter()
                        .filter_map(|l| l.trim_start().strip_prefix("- "))
                        .map(|item| unquote(strip_comment(item)))
                        .collect(),
                )
            } else {
                Value::Nested
            }
        } else if rest.starts_with('[') && rest.ends_with(']') {
            Value::List(
                split_commas(&rest[1..rest.len() - 1])
                    .into_iter()
                    .map(|item| unquote(&item))
                    .collect(),
            )
        } else {
            Value::Scalar(unquote(rest))
        };
        entries.push((key, value));
    }
    Ok(Document {
        entries,
        body: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_lists_and_flags_read_as_claude_code_writes_them() {
        let doc = parse(
            "---\nname: review\ndescription: \"Reviews a diff: carefully\"\nallowed-tools: Bash(git add:*), Read\nargument-hint: [issue-number]\ndisable-model-invocation: true\ntools:\n  - Read\n  - Grep\n---\nBody text\n",
        )
        .unwrap();
        assert_eq!(doc.text("name").as_deref(), Some("review"));
        assert_eq!(
            doc.text("description").as_deref(),
            Some("Reviews a diff: carefully")
        );
        assert_eq!(
            doc.list("allowed-tools").unwrap(),
            ["Bash(git add:*)", "Read"]
        );
        assert_eq!(doc.list("argument-hint").unwrap(), ["issue-number"]);
        assert_eq!(doc.flag("disable-model-invocation"), Ok(Some(true)));
        assert_eq!(doc.list("tools").unwrap(), ["Read", "Grep"]);
        let commented =
            parse("---\ndisallowedTools:\n  - Bash\n  # - WebFetch\n  - Write\n---\n").unwrap();
        assert_eq!(
            commented.list("disallowedTools").unwrap(),
            ["Bash", "Write"]
        );
        assert_eq!(doc.body, "Body text\n");
    }

    #[test]
    fn block_scalars_and_nested_values() {
        let doc = parse("---\ndescription: >\n  one\n  two\nhooks:\n  PreToolUse:\n    - a: b\nprompt: |\n  x\n  y\n---\nB").unwrap();
        assert_eq!(doc.text("description").as_deref(), Some("one two"));
        assert_eq!(doc.get("hooks"), Some(&Value::Nested));
        assert_eq!(doc.text("prompt").as_deref(), Some("x\ny"));
    }

    #[test]
    fn a_file_without_a_header_is_all_body() {
        let doc = parse("just text\n").unwrap();
        assert!(!doc.has("name"));
        assert_eq!(doc.body, "just text\n");
    }

    #[test]
    fn broken_headers_are_errors() {
        assert!(parse("---\nname: x\nbody").is_err());
        assert!(parse("---\nnot a pair\n---\n").is_err());
    }

    #[test]
    fn commas_split_outside_parentheses_only() {
        assert_eq!(
            split_commas("Bash(git add:*, git commit:*), Read"),
            ["Bash(git add:*, git commit:*)", "Read"]
        );
    }

    #[test]
    fn crlf_files_load() {
        let doc = parse("---\r\nname: x\r\n---\r\nhello\r\n").unwrap();
        assert_eq!(doc.text("name").as_deref(), Some("x"));
        assert_eq!(doc.body, "hello\n");
    }
}
