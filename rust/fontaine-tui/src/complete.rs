//! `@` file mentions: finding the token being typed and ranking the workspace files that could
//! complete it. Completion only edits the input; the message is sent as typed.

use std::path::Path;

/// The most candidates the list offers.
pub const MAX_MATCHES: usize = 8;

/// A `@` token in the input, as char offsets (`end` exclusive) and the text after the `@`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    pub start: usize,
    pub end: usize,
    pub query: String,
}

/// The `@` token the cursor is inside or at the end of: one that starts the input or follows
/// whitespace, with the cursor after its `@`.
pub fn mention_at(text: &str, cursor: usize) -> Option<Mention> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    if chars.get(start) != Some(&'@') || cursor == start {
        return None;
    }
    let mut end = cursor;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    Some(Mention {
        start,
        end,
        query: chars[start + 1..end].iter().collect(),
    })
}

/// A path as the list shows it: relative to the primary root when it is under it.
pub fn display(path: &str, root: &Path) -> String {
    Path::new(path).strip_prefix(root).map_or_else(
        |_| path.to_string(),
        |relative| relative.to_string_lossy().into_owned(),
    )
}

/// The files whose displayed path contains `query`, ignoring case: basename prefix matches
/// first, then basename substring matches, then the rest, shorter paths first within each.
pub fn rank(query: &str, files: &[String], root: &Path) -> Vec<String> {
    let query = query.to_lowercase();
    let mut found: Vec<(u8, usize, String)> = Vec::new();
    for file in files {
        let shown = display(file, root);
        let lower = shown.to_lowercase();
        if !lower.contains(&query) {
            continue;
        }
        let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
        let class = if base.starts_with(&query) {
            0
        } else if base.contains(&query) {
            1
        } else {
            2
        };
        found.push((class, shown.chars().count(), shown));
    }
    found.sort();
    found
        .into_iter()
        .take(MAX_MATCHES)
        .map(|(_, _, shown)| shown)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mention(text: &str, cursor: usize) -> Option<(usize, String)> {
        mention_at(text, cursor).map(|m| (m.start, m.query))
    }

    #[test]
    fn a_mention_starts_the_input_or_follows_whitespace() {
        assert_eq!(mention("@src", 4), Some((0, "src".into())));
        assert_eq!(mention("look at @src/m", 14), Some((8, "src/m".into())));
        assert_eq!(mention("a\n@x", 4), Some((2, "x".into())));
        assert_eq!(mention("@", 1), Some((0, String::new())));
    }

    #[test]
    fn an_at_inside_a_word_is_not_a_mention() {
        assert_eq!(mention("a@b", 3), None);
        assert_eq!(mention("mail me@host", 12), None);
    }

    #[test]
    fn the_cursor_must_be_in_the_token() {
        assert_eq!(mention("@src and", 8), None);
        assert_eq!(mention("@src ", 5), None);
        assert_eq!(mention("@src", 0), None);
        assert_eq!(mention("see @src and", 6), Some((4, "src".into())));
        assert_eq!(mention_at("see @src and", 6).unwrap().end, 8);
    }

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn ranking_prefers_basename_prefix_then_substring_then_shorter_paths() {
        let all = files(&[
            "/w/docs/main_notes.md",
            "/w/src/domain.rs",
            "/w/src/main.rs",
            "/w/main/readme.txt",
            "/w/src/deep/er/main.rs",
            "/w/unrelated.rs",
        ]);
        assert_eq!(
            rank("main", &all, Path::new("/w")),
            vec![
                "src/main.rs",
                "docs/main_notes.md",
                "src/deep/er/main.rs",
                "src/domain.rs",
                "main/readme.txt"
            ]
        );
        assert_eq!(
            rank("MAIN.R", &all, Path::new("/w")),
            vec!["src/main.rs", "src/deep/er/main.rs", "src/domain.rs"]
        );
    }

    #[test]
    fn at_most_eight_are_offered() {
        let all: Vec<String> = (0..20).map(|i| format!("/w/f{i}.rs")).collect();
        assert_eq!(rank("f", &all, Path::new("/w")).len(), MAX_MATCHES);
    }

    #[test]
    fn paths_outside_the_primary_root_stay_absolute() {
        let all = files(&["/w/a.rs", "/other/a.rs"]);
        assert_eq!(
            rank("a.rs", &all, Path::new("/w")),
            vec!["a.rs", "/other/a.rs"]
        );
    }
}
