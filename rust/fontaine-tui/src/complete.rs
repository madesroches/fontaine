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

/// One candidate the `@` list can offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    /// A workspace file or directory: displayed relative to the primary root.
    File(String),
    /// An MCP resource of a connected server, inserted as `@<uri>`.
    Resource {
        server: String,
        uri: String,
        name: String,
        description: Option<String>,
    },
}

impl Candidate {
    /// The text the `@` token completes to.
    pub fn insertion(&self) -> &str {
        match self {
            Candidate::File(path) => path,
            Candidate::Resource { uri, .. } => uri,
        }
    }

    /// The text acceptance writes into the input: `insertion`, except a resource whose uri
    /// more than one connected server lists, which carries its server after a second `@`
    /// (`@<uri>@<server>`) so the reader can tell the duplicates apart; everything up to the
    /// final `@` is the uri.
    pub fn accepted(&self, mentions: &[fontaine_core::agent::McpResourceSummary]) -> String {
        match self {
            Candidate::Resource { server, uri, .. }
                if mentions
                    .iter()
                    .filter(|mention| mention.uri == *uri)
                    .count()
                    > 1 =>
            {
                format!("{uri}@{server}")
            }
            _ => self.insertion().to_string(),
        }
    }

    /// The row the list shows: the insertion text, with a resource labelled by its server.
    pub fn label(&self) -> String {
        match self {
            Candidate::File(path) => path.clone(),
            Candidate::Resource { server, uri, .. } => format!("{uri} — {server}"),
        }
    }
}

/// The files, directories and MCP resources that could complete a mention: files ranked as
/// before, then the resources whose uri contains `query`, ignoring case. The two sources never
/// mix: a resource only matches its uri, a file only its path.
pub fn rank(
    query: &str,
    files: &[String],
    resources: &[fontaine_core::agent::McpResourceSummary],
    root: &Path,
) -> Vec<Candidate> {
    let mut ranked: Vec<Candidate> = rank_files(query, files, root)
        .into_iter()
        .map(Candidate::File)
        .collect();
    let query = query.to_lowercase();
    let mut offered: Vec<(usize, Candidate)> = resources
        .iter()
        .filter(|resource| resource.uri.to_lowercase().contains(&query))
        .map(|resource| {
            (
                resource.uri.chars().count(),
                Candidate::Resource {
                    server: resource.server.clone(),
                    uri: resource.uri.clone(),
                    name: resource.name.clone(),
                    description: resource.description.clone(),
                },
            )
        })
        .collect();
    offered.sort_by_key(|(len, _)| *len);
    ranked.extend(offered.into_iter().map(|(_, candidate)| candidate));
    ranked
}

/// The files and directories whose displayed path contains `query`, ignoring case: basename
/// prefix matches first, then basename substring matches, then the rest, shorter paths first
/// within each. Comparisons treat `\` as `/`; the returned strings keep their spelling.
fn rank_files(query: &str, files: &[String], root: &Path) -> Vec<String> {
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

    fn ranked(query: &str, files: &[String], root: &Path) -> Vec<String> {
        rank(query, files, &[], root)
            .into_iter()
            .map(|candidate| match candidate {
                Candidate::File(path) => path,
                other => panic!("expected a file candidate, got {other:?}"),
            })
            .collect()
    }

    fn resources(uris: &[(&str, &str)]) -> Vec<fontaine_core::agent::McpResourceSummary> {
        uris.iter()
            .map(|(server, uri)| fontaine_core::agent::McpResourceSummary {
                server: server.to_string(),
                uri: uri.to_string(),
                name: String::new(),
                description: None,
            })
            .collect()
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
            ranked("main", &all, Path::new("/w")),
            vec![
                "src/main.rs",
                "docs/main_notes.md",
                "src/deep/er/main.rs",
                "src/domain.rs",
                "main/readme.txt"
            ]
        );
        assert_eq!(
            ranked("MAIN.R", &all, Path::new("/w")),
            vec!["src/main.rs", "src/deep/er/main.rs", "src/domain.rs"]
        );
    }

    #[test]
    fn the_full_match_set_is_returned() {
        let all: Vec<String> = (0..20).map(|i| format!("/w/f{i}.rs")).collect();
        assert_eq!(ranked("f", &all, Path::new("/w")).len(), 20);
    }

    #[test]
    fn paths_outside_the_primary_root_stay_absolute() {
        let all = files(&["/w/a.rs", "/other/a.rs"]);
        assert_eq!(
            ranked("a.rs", &all, Path::new("/w")),
            vec!["a.rs", "/other/a.rs"]
        );
    }

    #[test]
    fn a_directory_matches_by_its_own_name_and_keeps_its_marker() {
        let all = files(&["/w/rust/core/src/tasks.rs", "/w/tasks/", "/w/tasks/plan.md"]);
        assert_eq!(
            ranked("task", &all, Path::new("/w")),
            vec!["tasks/", "rust/core/src/tasks.rs", "tasks/plan.md"]
        );
    }

    #[test]
    fn a_directory_is_not_listed_among_its_own_children() {
        let all = files(&["/w/tasks/", "/w/tasks/plan.md", "/w/src/main.rs"]);
        assert_eq!(
            ranked("tasks/", &all, Path::new("/w")),
            vec!["tasks/plan.md"]
        );
        assert_eq!(
            ranked("TASKS/", &all, Path::new("/w")),
            vec!["tasks/plan.md"]
        );
        assert_eq!(
            ranked("src/main.rs", &all, Path::new("/w")),
            vec!["src/main.rs"]
        );
    }

    #[test]
    fn mixed_separators_still_descend_into_a_directory() {
        let all = files(&["/w/src\\sub/", "/w/src\\sub/a.rs", "/w/src\\sub\\b.rs"]);
        assert_eq!(
            ranked("src/sub/", &all, Path::new("/w")),
            vec!["src\\sub/a.rs", "src\\sub\\b.rs"]
        );
        assert_eq!(
            ranked("src\\sub\\", &all, Path::new("/w")),
            vec!["src\\sub/a.rs", "src\\sub\\b.rs"]
        );
    }

    #[test]
    fn resources_rank_after_files_and_match_by_uri_only() {
        let all = files(&["/w/src/info.rs"]);
        let offered = resources(&[("web", "web://info"), ("docs", "docs://readme.md")]);
        let ranked = rank("info", &all, &offered, Path::new("/w"));
        assert_eq!(
            ranked,
            vec![
                Candidate::File("src/info.rs".to_string()),
                Candidate::Resource {
                    server: "web".to_string(),
                    uri: "web://info".to_string(),
                    name: String::new(),
                    description: None,
                },
            ],
            "a file matches the query; a resource only matches by uri: {ranked:?}"
        );
        // No cross-contamination: the resource's server name does not match it.
        let ranked = rank("web", &all, &offered, Path::new("/w"));
        assert_eq!(
            ranked,
            vec![Candidate::Resource {
                server: "web".to_string(),
                uri: "web://info".to_string(),
                name: String::new(),
                description: None,
            }],
            "{ranked:?}"
        );
        let none = rank("zzz", &all, &offered, Path::new("/w"));
        assert!(none.is_empty(), "{none:?}");
    }
}
