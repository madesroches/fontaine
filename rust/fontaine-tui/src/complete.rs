//! `@` file mentions and `/` commands: finding the token being typed, and ranking the workspace
//! files that could complete a mention. Completion only edits the input; the message is sent as
//! typed.

use std::path::Path;

/// A sigil token in the input, as char offsets (`end` exclusive) and the text after the sigil.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub query: String,
}

/// The `sigil` token the cursor is inside or at the end of: one that starts the input or
/// follows whitespace, with the cursor after its sigil.
pub fn token_at(text: &str, sigil: char, cursor: usize) -> Option<Token> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    if chars.get(start) != Some(&sigil) || cursor == start {
        return None;
    }
    let mut end = cursor;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    Some(Token {
        start,
        end,
        query: chars[start + 1..end].iter().collect(),
    })
}

/// The `@` token the cursor is in.
pub fn mention_at(text: &str, cursor: usize) -> Option<Token> {
    token_at(text, '@', cursor)
}

/// A path as the list shows it: relative to the primary root when it is under it. A directory's
/// trailing separator is set aside around the `Path` operations, which would drop it, and comes
/// back as `/`.
pub fn display(path: &str, root: &Path) -> String {
    let (bare, directory) = match path.strip_suffix(['/', '\\']) {
        Some(bare) => (bare, true),
        None => (path, false),
    };
    let mut shown = Path::new(bare).strip_prefix(root).map_or_else(
        |_| bare.to_string(),
        |relative| relative.to_string_lossy().into_owned(),
    );
    if directory {
        shown.push('/');
    }
    shown
}

/// The files and directories whose displayed path contains `query`, ignoring case: basename
/// prefix matches first, then basename substring matches, then the rest, shorter paths first
/// within each. Comparisons treat `\` as `/`; the returned strings keep their spelling.
pub fn rank(query: &str, files: &[String], root: &Path) -> Vec<String> {
    let query = query.to_lowercase().replace('\\', "/");
    let mut found: Vec<(u8, usize, String)> = Vec::new();
    for file in files {
        let shown = display(file, root);
        let lower = shown.to_lowercase().replace('\\', "/");
        if !lower.contains(&query) {
            continue;
        }
        // A directory listing itself among its children is no completion.
        if lower.ends_with('/') && lower == query {
            continue;
        }
        let base = lower
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&lower);
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
    found.into_iter().map(|(_, _, shown)| shown).collect()
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
    fn a_slash_token_follows_the_same_rules_as_a_mention() {
        let slash = |text: &str, cursor| token_at(text, '/', cursor).map(|t| (t.start, t.query));
        assert_eq!(slash("/mod", 4), Some((0, "mod".into())));
        assert_eq!(slash("run /mod", 8), Some((4, "mod".into())));
        assert_eq!(slash("a\n/mod", 6), Some((2, "mod".into())));
        assert_eq!(slash("a/b", 3), None);
        assert_eq!(slash("see src/main", 12), None);
        assert_eq!(slash("/mod", 0), None);
        assert_eq!(slash("/mod x", 6), None);
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
    fn the_full_match_set_is_returned() {
        let all: Vec<String> = (0..20).map(|i| format!("/w/f{i}.rs")).collect();
        assert_eq!(rank("f", &all, Path::new("/w")).len(), 20);
    }

    #[test]
    fn paths_outside_the_primary_root_stay_absolute() {
        let all = files(&["/w/a.rs", "/other/a.rs"]);
        assert_eq!(
            rank("a.rs", &all, Path::new("/w")),
            vec!["a.rs", "/other/a.rs"]
        );
    }

    #[test]
    fn a_directory_matches_by_its_own_name_and_keeps_its_marker() {
        let all = files(&["/w/rust/core/src/tasks.rs", "/w/tasks/", "/w/tasks/plan.md"]);
        assert_eq!(
            rank("task", &all, Path::new("/w")),
            vec!["tasks/", "rust/core/src/tasks.rs", "tasks/plan.md"]
        );
    }

    #[test]
    fn a_directory_is_not_listed_among_its_own_children() {
        let all = files(&["/w/tasks/", "/w/tasks/plan.md", "/w/src/main.rs"]);
        assert_eq!(rank("tasks/", &all, Path::new("/w")), vec!["tasks/plan.md"]);
        assert_eq!(rank("TASKS/", &all, Path::new("/w")), vec!["tasks/plan.md"]);
        assert_eq!(
            rank("src/main.rs", &all, Path::new("/w")),
            vec!["src/main.rs"]
        );
    }

    #[test]
    fn mixed_separators_still_descend_into_a_directory() {
        let all = files(&["/w/src\\sub/", "/w/src\\sub/a.rs", "/w/src\\sub\\b.rs"]);
        assert_eq!(
            rank("src/sub/", &all, Path::new("/w")),
            vec!["src\\sub/a.rs", "src\\sub\\b.rs"]
        );
        assert_eq!(
            rank("src\\sub\\", &all, Path::new("/w")),
            vec!["src\\sub/a.rs", "src\\sub\\b.rs"]
        );
    }
}
