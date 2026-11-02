//! `grep` and `ls` with the argument names models know from Claude Code, built on rig's
//! workspace walker: walking, ignore rules, confinement and reading stay rig's.

use std::collections::BTreeSet;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::types::TypesBuilder;
use regex::RegexBuilder;
use rig_core::tool::{
    FileRead, FilesRead, KeptEnd, OutputCut, Scope, ToolContext, ToolExecutionError, Touches,
    content_hash,
};
use rig_tools::fs::io_error;
use rig_tools::workspace::{Listing, Stat, Workspace};
use rig_tools::{
    FilesListed, List, MAX_BYTES, MAX_FILE_BYTES, MAX_FILES, MAX_LINES, MAX_MATCHES, Search,
    blocking, clip,
};
use schemars::JsonSchema;
use serde::Deserialize;

// rig's own limit is private.
const MAX_LINE: usize = 300;

/// What `grep` prints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// `path:line: text` for every matching line, with context lines when asked.
    Content,
    /// The paths of the files with a match, one per line.
    #[default]
    FilesWithMatches,
    /// `path:N` per file, N the number of matching lines.
    Count,
}

/// Arguments of `grep`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrepArgs {
    /// The regular expression (Rust regex syntax). Empty is allowed only with
    /// `output_mode` `files_with_matches`, where it lists every file.
    pub pattern: String,
    /// The directory or file to search; default the current directory.
    pub path: Option<String>,
    /// Only files matching this glob, such as `*.rs`.
    pub glob: Option<String>,
    /// Also hidden files and directories (named with a leading `.`), such
    /// as `.github/`; files git ignores stay out.
    #[serde(default)]
    pub hidden: bool,
    /// `content` (matching lines), `files_with_matches` (paths only; the default) or `count`
    /// (matching lines per file).
    pub output_mode: Option<OutputMode>,
    /// Ignore case.
    #[serde(default, rename = "-i")]
    pub case_insensitive: bool,
    /// Show line numbers in `content` mode; default true.
    #[serde(default, rename = "-n")]
    pub line_numbers: Option<bool>,
    /// Lines of context after each match (`content` mode).
    #[serde(default, rename = "-A")]
    pub after: Option<usize>,
    /// Lines of context before each match (`content` mode).
    #[serde(default, rename = "-B")]
    pub before: Option<usize>,
    /// Lines of context before and after each match (`content` mode); `-A` and `-B` override it
    /// for their side.
    #[serde(default, rename = "-C")]
    pub context: Option<usize>,
    /// Only files of this type, such as `rust` or `py` (ripgrep's file types).
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// At most this many output entries: matching lines in `content` mode, files in the
    /// others. 0 or none is the tool's own limit.
    pub head_limit: Option<usize>,
    /// Deprecated: `true` is `output_mode` `files_with_matches`, which `output_mode` overrides.
    #[serde(default)]
    pub files: bool,
}

impl GrepArgs {
    fn mode(&self) -> OutputMode {
        self.output_mode.unwrap_or_default()
    }

    fn listing(&self) -> Listing {
        listing(self.hidden, self.glob.as_deref())
    }
}

fn listing(hidden: bool, glob: Option<&str>) -> Listing {
    match hidden {
        true => Listing::Hidden,
        false => Listing::Visible,
    }
    .for_glob(glob)
}

/// Arguments of `ls`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LsArgs {
    /// The directory to list; default the current directory.
    pub path: Option<String>,
    /// How many levels down to list: 1 lists the directory's own entries;
    /// default every level.
    pub depth: Option<usize>,
    /// Only files matching this glob, such as `*.rs`.
    pub glob: Option<String>,
    /// Also hidden files and directories (named with a leading `.`), such
    /// as `.github/`; files git ignores stay out.
    #[serde(default)]
    pub hidden: bool,
    /// Globs of files and directories to leave out, such as `node_modules` or `*.log`. A glob
    /// without a `/` matches a name at any depth; one with a `/` matches a path below the
    /// listed directory.
    pub ignore: Option<Vec<String>>,
}

/// The footprint of a call that reads `path`, which is all the path rules look at.
pub fn footprint(
    workspace: &Arc<dyn Workspace>,
    base: Option<&Path>,
    path: Option<&str>,
) -> Touches {
    Touches::Reads(Scope::Paths(vec![
        workspace.resolve(base, path.unwrap_or(".")),
    ]))
}

struct Params {
    pattern: String,
    mode: OutputMode,
    case_insensitive: bool,
    numbers: bool,
    before: usize,
    after: usize,
    file_type: Option<String>,
    limit: usize,
    glob: Option<String>,
    listing: Listing,
}

/// Searches as `search` would for `args`.
pub async fn run_grep(
    search: &Search,
    context: &mut ToolContext,
    args: GrepArgs,
) -> Result<String, ToolExecutionError> {
    let mode = args.mode();
    if args.pattern.is_empty() && mode != OutputMode::FilesWithMatches {
        return Err(ToolExecutionError::invalid_args(
            "an empty pattern is only allowed with output_mode files_with_matches",
        ));
    }
    let path = args.path.as_deref().unwrap_or(".");
    search
        .workspace
        .place(search.base.as_deref(), search.confine, path)?;
    let base = search
        .base
        .clone()
        .filter(|_| Path::new(path).is_relative());
    let root = match &base {
        Some(base) => search.workspace.join(base, Path::new(path)),
        None => PathBuf::from(path),
    };
    let cap = match mode {
        OutputMode::Content => MAX_MATCHES,
        _ => MAX_FILES,
    };
    let params = Params {
        limit: effective_limit(args.head_limit, cap),
        before: args.before.or(args.context).unwrap_or(0),
        after: args.after.or(args.context).unwrap_or(0),
        numbers: args.line_numbers.unwrap_or(true),
        listing: args.listing(),
        pattern: args.pattern,
        mode,
        case_insensitive: args.case_insensitive,
        file_type: args.file_type,
        glob: args.glob,
    };
    let workspace = search.workspace.clone();
    let found = blocking(move || scan(&*workspace, &root, base.as_deref(), &params)).await?;
    if !found.read.is_empty() {
        context.insert_result(FilesRead(found.read))?;
    }
    Ok(found.text)
}

fn effective_limit(head_limit: Option<usize>, cap: usize) -> usize {
    match head_limit {
        None | Some(0) => cap,
        Some(limit) => limit.min(cap),
    }
}

struct Scanned {
    text: String,
    read: Vec<FileRead>,
}

fn bad_pattern(error: regex::Error) -> ToolExecutionError {
    ToolExecutionError::invalid_args(format!("bad pattern: {error}"))
}

/// The files `workspace` lists under `root`, with a missing root told apart from an empty one.
fn listed(
    workspace: &dyn Workspace,
    root: &Path,
    glob: Option<&str>,
    listing: Listing,
) -> Result<Vec<PathBuf>, ToolExecutionError> {
    let shown = root.to_string_lossy();
    let missing = || {
        let missing = io::Error::new(ErrorKind::NotFound, "no such file or directory");
        io_error(&shown, missing)
    };
    let files = workspace
        .files(root, glob, listing)
        .map_err(|error| match error.kind() {
            ErrorKind::InvalidInput => ToolExecutionError::invalid_args(error.to_string()),
            ErrorKind::NotFound => missing(),
            _ => io_error(&shown, error),
        })?;
    if files.is_empty()
        && workspace
            .stat(root)
            .map_err(|error| io_error(&shown, error))?
            == Stat::Missing
    {
        return Err(missing());
    }
    Ok(files)
}

/// `path` as the walk reached it, without a leading `./`.
fn walked(path: &Path) -> &Path {
    path.strip_prefix("./").unwrap_or(path)
}

fn scan(
    workspace: &dyn Workspace,
    root: &Path,
    base: Option<&Path>,
    params: &Params,
) -> Result<Scanned, ToolExecutionError> {
    let regex = match params.pattern.is_empty() {
        true => None,
        false => Some(
            RegexBuilder::new(&params.pattern)
                .case_insensitive(params.case_insensitive)
                .build()
                .map_err(bad_pattern)?,
        ),
    };
    let types = match &params.file_type {
        None => None,
        Some(name) => {
            let mut builder = TypesBuilder::new();
            builder.add_defaults().select(name);
            Some(builder.build().map_err(|error| {
                ToolExecutionError::invalid_args(format!("bad type {name}: {error}"))
            })?)
        }
    };
    let mut files = listed(workspace, root, params.glob.as_deref(), params.listing)?;
    files.sort();
    if let Some(types) = &types {
        files.retain(|file| types.matched(file, false).is_whitelist());
    }
    let shown = |path: &Path| -> PathBuf {
        let path = walked(path);
        base.and_then(|base| path.strip_prefix(base).ok())
            .unwrap_or(path)
            .to_path_buf()
    };

    let mut read = Vec::new();
    let mut entries: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut counted = 0;
    let mut cut = false;
    let push_entry = |entries: &mut Vec<String>, cut: &mut bool, entry: String| {
        if entries.len() == params.limit {
            *cut = true;
        } else {
            entries.push(entry);
        }
    };
    match &regex {
        None => {
            for file in &files {
                push_entry(&mut entries, &mut cut, shown(file).display().to_string());
            }
        }
        Some(regex) => {
            let chunks = files.chunks(100).flat_map(|chunk| {
                let bytes = workspace.read_files(chunk, MAX_FILE_BYTES);
                chunk.iter().zip(bytes)
            });
            for (file, bytes) in chunks {
                let Some(text) = bytes.and_then(|bytes| String::from_utf8(bytes).ok()) else {
                    continue;
                };
                let lines: Vec<&str> = text.lines().collect();
                let hits: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| regex.is_match(line))
                    .map(|(index, _)| index)
                    .collect();
                if hits.is_empty() {
                    continue;
                }
                read.push(FileRead::new(
                    workspace.canonical(walked(file)).to_string_lossy(),
                    content_hash(text.as_bytes()),
                ));
                let name = shown(file).display().to_string();
                match params.mode {
                    OutputMode::FilesWithMatches => push_entry(&mut entries, &mut cut, name),
                    OutputMode::Count => {
                        push_entry(&mut entries, &mut cut, render_count(&name, hits.len()))
                    }
                    OutputMode::Content => {
                        let take = hits.len().min(params.limit - counted);
                        cut |= take < hits.len();
                        counted += take;
                        groups.extend(render_content(
                            &name,
                            &lines,
                            &hits[..take],
                            params.before,
                            params.after,
                            params.numbers,
                        ));
                    }
                }
            }
        }
    }

    let mut out: Vec<String> = match params.mode {
        OutputMode::Content => {
            let separated = params.before + params.after > 0;
            let mut out = Vec::new();
            for (index, group) in groups.into_iter().enumerate() {
                if separated && index > 0 {
                    out.push("--".to_owned());
                }
                out.extend(group);
            }
            out
        }
        _ => entries,
    };
    if out.is_empty() {
        out.push("No matches.".to_owned());
    } else if cut {
        out.push(match params.mode {
            OutputMode::Content => format!("[stopped at {} matches]", params.limit),
            _ => format!(
                "[stopped at {} files; narrow the search with path or glob]",
                params.limit
            ),
        });
    }
    Ok(Scanned {
        text: out.join("\n"),
        read,
    })
}

/// `path:N`, N the number of lines that matched.
pub fn render_count(path: &str, matches: usize) -> String {
    format!("{path}:{matches}")
}

/// The lines of one file `hits` (indices into `lines`) put in front of the model: each match
/// as `path:line: text`, the context lines around them as `path-line- text`, in groups that are
/// not adjacent to each other.
pub fn render_content(
    path: &str,
    lines: &[&str],
    hits: &[usize],
    before: usize,
    after: usize,
    numbers: bool,
) -> Vec<Vec<String>> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for &hit in hits {
        let start = hit.saturating_sub(before);
        let end = hit.saturating_add(after).min(lines.len().saturating_sub(1));
        match spans.last_mut() {
            Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
            _ => spans.push((start, end)),
        }
    }
    spans
        .into_iter()
        .map(|(start, end)| {
            (start..=end)
                .map(|index| {
                    let mark = if hits.binary_search(&index).is_ok() {
                        ':'
                    } else {
                        '-'
                    };
                    let text = clip(lines[index], MAX_LINE);
                    match numbers {
                        true => format!("{path}{mark}{}{mark} {text}", index + 1),
                        false => format!("{path}{mark} {text}"),
                    }
                })
                .collect()
        })
        .collect()
}

/// Lists as `list` would for `args`, leaving out what `ignore` names.
pub async fn run_ls(
    list: &List,
    context: &mut ToolContext,
    args: LsArgs,
) -> Result<String, ToolExecutionError> {
    let root = list.workspace.place(
        list.base.as_deref(),
        list.confine,
        args.path.as_deref().unwrap_or("."),
    )?;
    let ignored = Ignored::new(args.ignore.as_deref().unwrap_or_default())?;
    let workspace = list.workspace.clone();
    let (text, listed, cut) =
        blocking(move || list_files(&*workspace, &root, &args, &ignored)).await?;
    context.insert_result(listed)?;
    if let Some(cut) = cut {
        context.insert_result(cut)?;
    }
    Ok(text)
}

/// The `ignore` globs of a listing, in gitignore's spirit: one without a `/` names a file or
/// directory at any depth, one with a `/` a path below the listed directory.
struct Ignored {
    names: GlobSet,
    paths: GlobSet,
}

impl Ignored {
    fn new(globs: &[String]) -> Result<Self, ToolExecutionError> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        for glob in globs {
            let trimmed = glob.trim_end_matches('/');
            let (set, anchored) = match trimmed.contains('/') {
                true => (&mut paths, trimmed.trim_start_matches('/')),
                false => (&mut names, trimmed),
            };
            let compiled = GlobBuilder::new(anchored)
                .literal_separator(true)
                .build()
                .map_err(|error| {
                    ToolExecutionError::invalid_args(format!("bad ignore glob {glob}: {error}"))
                })?;
            set.add(compiled);
        }
        let build = |set: GlobSetBuilder| {
            set.build().map_err(|error| {
                ToolExecutionError::invalid_args(format!("bad ignore glob: {error}"))
            })
        };
        Ok(Self {
            names: build(names)?,
            paths: build(paths)?,
        })
    }

    /// Whether `below`, a path below the listed directory, or a directory it is in, is named.
    fn names(&self, below: &Path) -> bool {
        let mut prefix = PathBuf::new();
        below.iter().any(|part| {
            prefix.push(part);
            self.names.is_match(part) || self.paths.is_match(&prefix)
        })
    }
}

type Listed = (String, FilesListed, Option<OutputCut>);

fn list_files(
    workspace: &dyn Workspace,
    root: &Path,
    args: &LsArgs,
    ignored: &Ignored,
) -> Result<Listed, ToolExecutionError> {
    let files = listed(
        workspace,
        root,
        args.glob.as_deref(),
        listing(args.hidden, args.glob.as_deref()),
    )?;
    let depth = args.depth.unwrap_or(usize::MAX).max(1);
    let entries: BTreeSet<String> = files
        .iter()
        .filter_map(|file| {
            let below = file.strip_prefix(root).ok()?;
            if ignored.names(below) {
                return None;
            }
            let parts: Vec<_> = below.iter().collect();
            Some(match parts.get(..depth) {
                Some(kept) if kept.len() < parts.len() => {
                    format!("{}/", kept.iter().collect::<PathBuf>().display())
                }
                _ if below.as_os_str().is_empty() => file.to_string_lossy().into_owned(),
                _ => below.to_string_lossy().into_owned(),
            })
        })
        .collect();
    let total = entries.len();
    let mut text = String::new();
    let mut kept = Vec::new();
    for entry in entries {
        if kept.len() == MAX_LINES || text.len() + entry.len() + 1 > MAX_BYTES {
            break;
        }
        text.push_str(&entry);
        text.push('\n');
        kept.push(entry);
    }
    let cut = (kept.len() < total).then(|| {
        text.push_str(&format!(
            "[{} more entries; narrow the listing with path, depth or glob]\n",
            total - kept.len()
        ));
        OutputCut {
            kept: KeptEnd::Start,
            kept_lines: kept.len(),
            total_lines: total,
            spill: None,
        }
    });
    if total == 0 {
        text.push_str("No files.\n");
    }
    let listed = FilesListed {
        root: root.to_string_lossy().into_owned(),
        entries: kept,
    };
    Ok((text, listed, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> Result<GrepArgs, serde_json::Error> {
        serde_json::from_value(json)
    }

    #[test]
    fn grep_args_take_claude_codes_names() {
        let args = parse(serde_json::json!({
            "pattern": "x", "-i": true, "-n": false, "-A": 2, "-B": 1, "-C": 3,
            "type": "rust", "head_limit": 5, "output_mode": "content",
        }))
        .unwrap();
        assert!(args.case_insensitive);
        assert_eq!(args.line_numbers, Some(false));
        assert_eq!(
            (args.after, args.before, args.context),
            (Some(2), Some(1), Some(3))
        );
        assert_eq!(args.file_type.as_deref(), Some("rust"));
        assert_eq!(args.head_limit, Some(5));
        assert_eq!(args.mode(), OutputMode::Content);
    }

    #[test]
    fn grep_args_default_to_paths_only() {
        let args = parse(serde_json::json!({"pattern": "x"})).unwrap();
        assert_eq!(args.mode(), OutputMode::FilesWithMatches);
        assert!(!args.case_insensitive);
        assert_eq!(args.line_numbers, None);
    }

    #[test]
    fn grep_args_reject_what_they_do_not_know() {
        assert!(parse(serde_json::json!({"pattern": "x", "bogus": 1})).is_err());
        assert!(parse(serde_json::json!({"pattern": "x", "-A": -1})).is_err());
        assert!(parse(serde_json::json!({"pattern": "x", "-C": 1.5})).is_err());
        assert!(parse(serde_json::json!({"pattern": "x", "-B": "2"})).is_err());
        assert!(parse(serde_json::json!({"pattern": "x", "output_mode": "lines"})).is_err());
    }

    #[test]
    fn the_files_alias_is_still_accepted() {
        let args = parse(serde_json::json!({"pattern": "", "files": true})).unwrap();
        assert!(args.files);
        assert_eq!(args.mode(), OutputMode::FilesWithMatches);
    }

    #[test]
    fn effective_limit_is_clamped_to_the_cap() {
        assert_eq!(effective_limit(None, 200), 200);
        assert_eq!(effective_limit(Some(0), 200), 200);
        assert_eq!(effective_limit(Some(5), 200), 5);
        assert_eq!(effective_limit(Some(999), 200), 200);
    }

    #[test]
    fn content_groups_merge_when_adjacent_or_overlapping() {
        let lines = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let groups = render_content("f", &lines, &[1, 3, 7], 1, 0, true);
        assert_eq!(
            groups,
            vec![
                vec!["f-1- a", "f:2: b", "f-3- c", "f:4: d"],
                vec!["f-7- g", "f:8: h"],
            ]
        );
    }

    #[test]
    fn content_without_numbers_drops_them() {
        let lines = ["a", "b", "c"];
        let groups = render_content("f", &lines, &[1], 1, 1, false);
        assert_eq!(groups, vec![vec!["f- a", "f: b", "f- c"]]);
    }

    #[test]
    fn content_context_stops_at_the_file_edges() {
        let lines = ["a", "b"];
        let groups = render_content("f", &lines, &[0], 5, 5, true);
        assert_eq!(groups, vec![vec!["f:1: a", "f-2- b"]]);
    }

    #[test]
    fn count_is_path_and_number() {
        assert_eq!(render_count("a.rs", 3), "a.rs:3");
    }

    #[test]
    fn ignore_names_match_at_any_depth_and_paths_below_the_root() {
        let ignored = Ignored::new(&[
            "node_modules".to_owned(),
            "*.log".to_owned(),
            "src/gen/".to_owned(),
        ])
        .unwrap();
        assert!(ignored.names(Path::new("node_modules/x.js")));
        assert!(ignored.names(Path::new("a/b/node_modules/x.js")));
        assert!(ignored.names(Path::new("a/run.log")));
        assert!(ignored.names(Path::new("src/gen/out.rs")));
        assert!(!ignored.names(Path::new("lib/src/gen/out.rs")));
        assert!(!ignored.names(Path::new("src/main.rs")));
        assert!(!ignored.names(Path::new("")));
    }

    #[test]
    fn a_bad_ignore_glob_is_invalid_args() {
        assert!(Ignored::new(&["[".to_owned()]).is_err());
    }
}
