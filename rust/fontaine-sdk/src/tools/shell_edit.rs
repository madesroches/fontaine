//! The classifier behind the `shell_edit` tier: whether a shell call is made only of read-only
//! commands and in-root edits and deletions, so `auto` need not ask the guardian.
//!
//! The walk is over the parser's syntax tree and recognises a short list of constructs; anything
//! else, however harmless, rejects the whole call and leaves it to the guardian. Every path the
//! call would touch is put to a [`TargetCheck`], which the permission pipeline implements with
//! the floor's rules (this module never sees them).

use std::io::Cursor;
use std::path::Path;

use brush_parser::ast::{
    AndOrList, AssignmentName, AssignmentValue, Command, CommandPrefixOrSuffixItem, CompoundList,
    IoFileRedirectKind, IoFileRedirectTarget, IoRedirect, Pipeline, SeparatorOperator,
    SimpleCommand,
};
use brush_parser::{Parser, ParserOptions};

use crate::tools::preclassify::{Environment, Preclassifier, literal_text, program_name};

/// What the pipeline answers for each path a shell call would read or change. Paths are
/// absolute, joined to the call's working directory as the model wrote them.
pub trait TargetCheck {
    /// Reading `path` with `program` is allowed: it is inside a root, is no credential file,
    /// and no rule or agent restriction forbids the read.
    fn read_target(&self, program: &str, path: &str) -> bool;
    /// An existing file the `edit` tool could change without a guardian verdict.
    fn edit_target(&self, path: &str) -> bool;
    /// Like [`edit_target`](Self::edit_target) for the `write` tool: the file may be absent.
    fn write_target(&self, path: &str) -> bool;
    /// `path` is in the primary root and is not a protected location.
    fn not_protected(&self, path: &str) -> bool;
    /// The repository has no hook git would run, so a commit or a `--continue` runs no code
    /// the workspace could have planted.
    fn hooks_inactive(&self) -> bool;
    /// `path` is an exact index file entry the `edit` tool could change without a verdict; the
    /// file itself need not exist.
    fn tracked_entry(&self, path: &str) -> bool;
}

pub fn classify(
    command: &str,
    env: &Environment<'_>,
    preclassifier: &Preclassifier,
    check: &dyn TargetCheck,
) -> bool {
    let Some(base) = env.base() else {
        return false;
    };
    let mut parser = Parser::new(Cursor::new(command.as_bytes()), &ParserOptions::default());
    let Ok(program) = parser.parse_program() else {
        return false;
    };
    let walk = Walk {
        env,
        base: &base,
        preclassifier,
        check,
    };
    !program.complete_commands.is_empty()
        && program.complete_commands.iter().all(|list| walk.list(list))
}

struct Walk<'a> {
    env: &'a Environment<'a>,
    base: &'a Path,
    preclassifier: &'a Preclassifier,
    check: &'a dyn TargetCheck,
}

/// The prefix variables a `git` command may carry, with the only values they may hold: they
/// stop git from opening an editor an unattended run cannot answer.
const GIT_PREFIX_VARIABLES: [&str; 2] = ["GIT_EDITOR", "GIT_SEQUENCE_EDITOR"];
const GIT_PREFIX_VALUES: [&str; 2] = ["true", ":"];

/// `git <op> --continue` operations.
const CONTINUABLE: [&str; 4] = ["rebase", "merge", "cherry-pick", "revert"];

struct Parts<'a> {
    assigned: bool,
    argv: Vec<String>,
    redirects: Vec<&'a IoRedirect>,
}

impl Walk<'_> {
    fn list(&self, list: &CompoundList) -> bool {
        list.0
            .iter()
            .all(|item| matches!(item.1, SeparatorOperator::Sequence) && self.and_or(&item.0))
    }

    fn and_or(&self, list: &AndOrList) -> bool {
        self.pipeline(&list.first)
            && list.additional.iter().all(|next| match next {
                brush_parser::ast::AndOr::And(p) | brush_parser::ast::AndOr::Or(p) => {
                    self.pipeline(p)
                }
            })
    }

    fn pipeline(&self, pipeline: &Pipeline) -> bool {
        if pipeline.timed.is_some() {
            return false;
        }
        match pipeline.seq.as_slice() {
            [only] => self.command(only, Stage::Only),
            stages => stages.iter().enumerate().all(|(at, stage)| {
                self.command(stage, if at == 0 { Stage::First } else { Stage::Later })
            }),
        }
    }

    fn command(&self, command: &Command, stage: Stage) -> bool {
        let Command::Simple(simple) = command else {
            return false;
        };
        let Some(parts) = parts(simple) else {
            return false;
        };
        if !parts.redirects.iter().all(|r| self.redirect(r)) {
            return false;
        }
        let argv = parts.argv.as_slice();
        // An extra must never receive model-written stdin (`sqlite3 <<EOF` runs `.shell`).
        let extras = !parts
            .redirects
            .iter()
            .any(|r| matches!(r, IoRedirect::HereDocument(..)));
        if stage != Stage::Only {
            return !parts.assigned && self.read_only(argv, extras && stage == Stage::First);
        }
        let name = argv[0].as_str();
        if parts.assigned && name != "git" {
            return false;
        }
        match name {
            "sed" => GNU_SED && self.sed(&argv[1..]),
            "mkdir" => self.mkdir(&argv[1..]),
            "mv" => self.mv(&argv[1..]),
            "rm" => self.rm(&argv[1..]),
            "git" => self.git(argv),
            _ => self.read_only(argv, extras),
        }
    }

    fn abs(&self, path: &str) -> String {
        self.base.join(path).to_string_lossy().into_owned()
    }

    fn read_only(&self, argv: &[String], extras: bool) -> bool {
        // A path-qualified `git` could be a program the workspace planted.
        if program_name(&argv[0]) == "git" && argv[0] != "git" {
            return false;
        }
        let found = if extras {
            self.preclassifier.read_only_argv(argv, self.env)
        } else {
            self.preclassifier.curated_read_only_argv(argv, self.env)
        };
        let program = program_name(&argv[0]);
        found.is_some_and(|paths| {
            paths
                .iter()
                .all(|path| self.check.read_target(&program, &self.abs(path)))
        })
    }

    fn redirect(&self, redirect: &IoRedirect) -> bool {
        let writes_to = |word: &brush_parser::ast::Word| match literal_text(&word.value) {
            Some(text) if text == "/dev/null" => true,
            Some(text) => self.check.write_target(&self.abs(&text)),
            None => false,
        };
        match redirect {
            IoRedirect::File(_, kind, target) => match (kind, target) {
                (
                    IoFileRedirectKind::Write
                    | IoFileRedirectKind::Append
                    | IoFileRedirectKind::Clobber,
                    IoFileRedirectTarget::Filename(word),
                ) => writes_to(word),
                (IoFileRedirectKind::DuplicateOutput, IoFileRedirectTarget::Fd(_)) => true,
                (IoFileRedirectKind::DuplicateOutput, IoFileRedirectTarget::Duplicate(word)) => {
                    literal_text(&word.value).is_some_and(|text| {
                        text == "-"
                            || (!text.is_empty() && text.chars().all(|c| c.is_ascii_digit()))
                    })
                }
                _ => false,
            },
            IoRedirect::OutputAndError(word, _) => writes_to(word),
            IoRedirect::HereDocument(_, doc) => !doc.requires_expansion,
            IoRedirect::HereString(..) => false,
        }
    }

    fn sed(&self, args: &[String]) -> bool {
        let mut in_place = false;
        let mut scripts: Vec<String> = Vec::new();
        let mut files: Vec<&str> = Vec::new();
        let mut options_done = false;
        let mut at = 0;
        while at < args.len() {
            let arg = args[at].as_str();
            at += 1;
            if options_done || arg == "-" || !arg.starts_with('-') {
                files.push(arg);
                continue;
            }
            if arg == "--" {
                options_done = true;
                continue;
            }
            if let Some(long) = arg.strip_prefix("--") {
                match long {
                    "in-place" => in_place = true,
                    "posix" => {}
                    _ => match long.strip_prefix("expression=") {
                        Some(script) => scripts.push(script.to_string()),
                        None => return false,
                    },
                }
                continue;
            }
            let letters = &arg[1..];
            for (offset, letter) in letters.char_indices() {
                match letter {
                    'E' | 'r' | 's' => {}
                    // An `i` that is not last would swallow the rest as a backup suffix.
                    'i' if offset + 1 == letters.len() => in_place = true,
                    'e' => {
                        let rest = &letters[offset + 1..];
                        if rest.is_empty() {
                            let Some(script) = args.get(at) else {
                                return false;
                            };
                            at += 1;
                            scripts.push(script.clone());
                        } else {
                            scripts.push(rest.to_string());
                        }
                        break;
                    }
                    _ => return false,
                }
            }
        }
        if !in_place {
            return false;
        }
        if scripts.is_empty() {
            if files.is_empty() {
                return false;
            }
            scripts.push(files.remove(0).to_string());
        }
        !files.is_empty()
            && scripts.iter().all(|script| sed_script_ok(script))
            && files.iter().all(|file| {
                let path = self.abs(file);
                !under_dot_git(&path) && self.check.edit_target(&path)
            })
    }

    fn mkdir(&self, args: &[String]) -> bool {
        let mut dirs = Vec::new();
        let mut options_done = false;
        for arg in args {
            match arg.as_str() {
                "--" if !options_done => options_done = true,
                "-p" if !options_done => {}
                other if options_done || !other.starts_with('-') => dirs.push(other),
                _ => return false,
            }
        }
        !dirs.is_empty()
            && dirs.iter().all(|dir| {
                let path = self.abs(dir);
                !under_dot_git(&path) && self.check.write_target(&path)
            })
    }

    /// Only regular files: no recursion, no directory removal, no prompt, so every removed path
    /// passes the per-path checks.
    fn rm(&self, args: &[String]) -> bool {
        let mut files = Vec::new();
        let mut options_done = false;
        for arg in args {
            let arg = arg.as_str();
            if options_done || !arg.starts_with('-') {
                files.push(arg);
            } else if arg == "--" {
                options_done = true;
            } else if let Some(name) = arg.strip_prefix("--") {
                if !matches!(name, "force" | "verbose") {
                    return false;
                }
            } else if arg.len() < 2 || !arg[1..].chars().all(|letter| matches!(letter, 'f' | 'v')) {
                return false;
            }
        }
        !files.is_empty()
            && files.iter().all(|file| {
                let path = self.abs(file);
                !under_dot_git(&path) && self.check.edit_target(&path)
            })
    }

    fn mv(&self, args: &[String]) -> bool {
        let mut operands = Vec::new();
        let mut options_done = false;
        for arg in args {
            match arg.as_str() {
                "--" if !options_done => options_done = true,
                "-n" if !options_done => {}
                other if options_done || !other.starts_with('-') => operands.push(other),
                _ => return false,
            }
        }
        let Some((dest, sources)) = operands.split_last() else {
            return false;
        };
        if sources.is_empty() {
            return false;
        }
        let dest = self.abs(dest);
        let into_dir = Path::new(&dest).is_dir();
        let missing = !Path::new(&dest).exists();
        if !into_dir && sources.len() != 1 {
            return false;
        }
        sources.iter().all(|source| {
            let from = self.abs(source);
            let inside = || {
                Path::new(&from)
                    .file_name()
                    .map(|name| Path::new(&dest).join(name).to_string_lossy().into_owned())
            };
            let Some(to) = (if into_dir {
                inside()
            } else {
                Some(dest.clone())
            }) else {
                return false;
            };
            // An earlier `mkdir` in the same call is not there yet, so a destination that is
            // absent might become the directory the file lands in.
            !under_dot_git(&from)
                && !under_dot_git(&to)
                && self.check.read_target("mv", &from)
                && self.check.edit_target(&from)
                && self.check.write_target(&to)
                && (!missing || inside().is_some_and(|inner| self.check.write_target(&inner)))
        })
    }

    fn git(&self, argv: &[String]) -> bool {
        let Some(sub) = argv.get(1).map(String::as_str) else {
            return false;
        };
        // Any option between `git` and the subcommand (`-c`, `-C`, `--git-dir`) changes which
        // repository or configuration the rest runs under.
        if sub.starts_with('-') {
            return false;
        }
        let args = &argv[2..];
        match sub {
            // Staging runs `post-index-change`, so every verb that touches the index needs the
            // same hook check as a commit.
            "add" => self.git_add(args) && self.check.hooks_inactive(),
            "rm" => self.git_rm(args) && self.check.hooks_inactive(),
            "mv" => self.git_mv(args) && self.check.hooks_inactive(),
            "commit" => self.git_commit(args) && self.check.hooks_inactive(),
            op if CONTINUABLE.contains(&op) => {
                args == ["--continue"] && self.check.hooks_inactive()
            }
            _ => self.read_only(argv, false),
        }
    }

    /// The pathspecs of a `git` invocation whose flags are `flags` (short clusters of the given
    /// letters or exact long names); `None` when any other flag appears.
    fn pathspecs<'a>(
        &self,
        args: &'a [String],
        short: &[char],
        long: &[&str],
        seen: &mut Vec<String>,
    ) -> Option<Vec<&'a str>> {
        let mut paths = Vec::new();
        let mut options_done = false;
        for arg in args {
            if options_done || arg == "-" || !arg.starts_with('-') {
                paths.push(arg.as_str());
            } else if arg == "--" {
                options_done = true;
            } else if let Some(name) = arg.strip_prefix("--") {
                if !long.contains(&name) {
                    return None;
                }
                seen.push(name.to_string());
            } else {
                for letter in arg[1..].chars() {
                    if !short.contains(&letter) {
                        return None;
                    }
                    seen.push(letter.to_string());
                }
            }
        }
        paths
            .iter()
            .all(|path| literal_pathspec(path))
            .then_some(paths)
    }

    fn git_add(&self, args: &[String]) -> bool {
        let mut seen = Vec::new();
        let Some(paths) = self.pathspecs(args, &['u', 'v'], &["update"], &mut seen) else {
            return false;
        };
        let update = seen.iter().any(|flag| flag == "u" || flag == "update");
        (update || !paths.is_empty())
            && paths.iter().all(|path| {
                let path = self.abs(path);
                !under_dot_git(&path)
                    && Path::new(&path).is_file()
                    && self.check.read_target("git", &path)
                    && self.check.not_protected(&path)
            })
    }

    fn git_rm(&self, args: &[String]) -> bool {
        let mut seen = Vec::new();
        let Some(paths) = self.pathspecs(args, &['q', 'f'], &["cached", "force"], &mut seen) else {
            return false;
        };
        let cached = seen.iter().any(|flag| flag == "cached");
        !paths.is_empty()
            && paths.iter().all(|path| {
                let path = self.abs(path);
                !under_dot_git(&path)
                    && if cached {
                        self.check.tracked_entry(&path)
                    } else {
                        self.check.edit_target(&path)
                    }
            })
    }

    fn git_mv(&self, args: &[String]) -> bool {
        let mut seen = Vec::new();
        let Some(paths) = self.pathspecs(args, &['v', 'f'], &["force"], &mut seen) else {
            return false;
        };
        let [from, to] = paths.as_slice() else {
            return false;
        };
        let (from, to) = (self.abs(from), self.abs(to));
        // Absent now, `to` may be a directory an earlier `mkdir` in this call creates.
        let inner = (!Path::new(&to).exists())
            .then(|| {
                Path::new(&from)
                    .file_name()
                    .map(|name| Path::new(&to).join(name))
            })
            .flatten();
        !under_dot_git(&from)
            && !under_dot_git(&to)
            && self.check.edit_target(&from)
            && self.check.write_target(&to)
            && inner.is_none_or(|inner| self.check.write_target(&inner.to_string_lossy()))
    }

    fn git_commit(&self, args: &[String]) -> bool {
        let mut messages = 0;
        let mut at = 0;
        let mut options_done = false;
        while at < args.len() {
            let arg = args[at].as_str();
            at += 1;
            if options_done || arg == "-" || !arg.starts_with('-') {
                // A pathspec would commit those files as they are, past what was staged.
                return false;
            }
            if arg == "--" {
                options_done = true;
                continue;
            }
            if let Some(long) = arg.strip_prefix("--") {
                let (name, attached) = match long.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (long, None),
                };
                match (name, attached) {
                    ("all" | "no-edit", None) => {}
                    ("message", value) => {
                        if value.is_none() {
                            if args.get(at).is_none() {
                                return false;
                            }
                            at += 1;
                        }
                        messages += 1;
                    }
                    ("file", value) => {
                        let Some(file) = value.or_else(|| {
                            at += 1;
                            args.get(at - 1).map(String::as_str)
                        }) else {
                            return false;
                        };
                        if !self.message_file(file) {
                            return false;
                        }
                        messages += 1;
                    }
                    _ => return false,
                }
                continue;
            }
            let letters = &arg[1..];
            for (offset, letter) in letters.char_indices() {
                match letter {
                    'a' | 'q' => {}
                    'm' | 'F' => {
                        let rest = &letters[offset + letter.len_utf8()..];
                        let value = if rest.is_empty() {
                            at += 1;
                            args.get(at - 1).map(String::as_str)
                        } else {
                            Some(rest)
                        };
                        let Some(value) = value else {
                            return false;
                        };
                        if letter == 'F' && !self.message_file(value) {
                            return false;
                        }
                        messages += 1;
                        break;
                    }
                    _ => return false,
                }
            }
        }
        messages > 0
    }

    fn message_file(&self, file: &str) -> bool {
        file != "-" && self.check.read_target("git", &self.abs(file))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Only,
    First,
    Later,
}

/// The words and redirects of one simple command, or `None` when anything is not a plain
/// literal word, an allowed prefix assignment or a redirect.
fn parts(simple: &SimpleCommand) -> Option<Parts<'_>> {
    let mut assigned = false;
    let mut redirects = Vec::new();
    for item in simple.prefix.iter().flat_map(|p| &p.0) {
        match item {
            CommandPrefixOrSuffixItem::AssignmentWord(assignment, _) => {
                let AssignmentName::VariableName(name) = &assignment.name else {
                    return None;
                };
                let AssignmentValue::Scalar(value) = &assignment.value else {
                    return None;
                };
                let value = literal_text(&value.value)?;
                if assignment.append
                    || !GIT_PREFIX_VARIABLES.contains(&name.as_str())
                    || !GIT_PREFIX_VALUES.contains(&value.as_str())
                {
                    return None;
                }
                assigned = true;
            }
            CommandPrefixOrSuffixItem::IoRedirect(redirect) => redirects.push(redirect),
            CommandPrefixOrSuffixItem::Word(_)
            | CommandPrefixOrSuffixItem::ProcessSubstitution(..) => {
                return None;
            }
        }
    }
    let mut argv = vec![literal_text(&simple.word_or_name.as_ref()?.value)?];
    for item in simple.suffix.iter().flat_map(|s| &s.0) {
        match item {
            CommandPrefixOrSuffixItem::Word(word)
            | CommandPrefixOrSuffixItem::AssignmentWord(_, word) => {
                argv.push(literal_text(&word.value)?);
            }
            CommandPrefixOrSuffixItem::IoRedirect(redirect) => redirects.push(redirect),
            CommandPrefixOrSuffixItem::ProcessSubstitution(..) => return None,
        }
    }
    Some(Parts {
        assigned,
        argv,
        redirects,
    })
}

/// A path word git would not expand into other paths.
fn literal_pathspec(word: &str) -> bool {
    !word.is_empty() && !word.contains(['*', '?', '[', '\\']) && !word.starts_with(':')
}

fn under_dot_git(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    })
}

/// BSD `sed -i` takes the next argument as a backup suffix, so the classifier and sed would
/// read different scripts; only GNU sed is modelled.
const GNU_SED: bool = cfg!(target_os = "linux");

/// Whether `script` holds only `d` and `s` commands, each with an optional address. GNU `sed`
/// runs a command for `e` and writes a file for `w`, so the grammar is a whitelist: anything
/// else, including blocks, labels, `y` and the text commands, rejects.
pub fn sed_script_ok(script: &str) -> bool {
    // GNU sed does not end a regex at a delimiter inside a bracket expression, which this
    // grammar does not model; a hidden `e` or `w` flag would follow.
    if script.contains('[') {
        return false;
    }
    let chars: Vec<char> = script.chars().collect();
    let mut at = 0;
    loop {
        while at < chars.len() && (chars[at].is_whitespace() || chars[at] == ';') {
            at += 1;
        }
        if at >= chars.len() {
            return true;
        }
        let Some(first) = address(&chars, &mut at) else {
            return false;
        };
        skip_blanks(&chars, &mut at);
        if first && chars.get(at) == Some(&',') {
            at += 1;
            skip_blanks(&chars, &mut at);
            if address(&chars, &mut at) != Some(true) {
                return false;
            }
            skip_blanks(&chars, &mut at);
        }
        match chars.get(at) {
            Some('d') => at += 1,
            Some('s') => {
                at += 1;
                if !substitution(&chars, &mut at) {
                    return false;
                }
            }
            _ => return false,
        }
        match chars.get(at) {
            None | Some(';') => {}
            Some(c) if c.is_whitespace() => {}
            Some(_) => return false,
        }
    }
}

fn skip_blanks(chars: &[char], at: &mut usize) {
    while chars.get(*at).is_some_and(|c| *c == ' ' || *c == '\t') {
        *at += 1;
    }
}

/// Consumes an address when one is there: `Some(true)` for one, `Some(false)` for none (the
/// command follows directly, and the caller judges it), `None` for a malformed one.
fn address(chars: &[char], at: &mut usize) -> Option<bool> {
    match chars.get(*at) {
        Some('$') => *at += 1,
        Some(c) if c.is_ascii_digit() => {
            while chars.get(*at).is_some_and(char::is_ascii_digit) {
                *at += 1;
            }
        }
        Some('/') => {
            *at += 1;
            if !until_delimiter(chars, at, '/') {
                return None;
            }
        }
        _ => return Some(false),
    }
    Some(true)
}

/// Skips to the unescaped `delimiter` and past it. A newline inside, or no closing delimiter,
/// fails.
fn until_delimiter(chars: &[char], at: &mut usize, delimiter: char) -> bool {
    while let Some(&c) = chars.get(*at) {
        *at += 1;
        match c {
            '\\' => {
                if chars.get(*at).is_none() {
                    return false;
                }
                *at += 1;
            }
            '\n' => return false,
            c if c == delimiter => return true,
            _ => {}
        }
    }
    false
}

fn substitution(chars: &[char], at: &mut usize) -> bool {
    let Some(&delimiter) = chars.get(*at) else {
        return false;
    };
    if delimiter == '\n' || delimiter == '\\' {
        return false;
    }
    *at += 1;
    if !until_delimiter(chars, at, delimiter) || !until_delimiter(chars, at, delimiter) {
        return false;
    }
    while let Some(&c) = chars.get(*at) {
        if c.is_whitespace() || c == ';' {
            break;
        }
        if !(matches!(c, 'g' | 'i' | 'I') || c.is_ascii_digit()) {
            return false;
        }
        *at += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::scope::Scope;

    /// Answers like a pipeline over a repository where every file under the root is tracked,
    /// except the names a floor would keep from an unjudged edit.
    struct Stub {
        root: std::path::PathBuf,
    }

    impl Stub {
        fn inside(&self, path: &str) -> Option<std::path::PathBuf> {
            Path::new(path)
                .strip_prefix(&self.root)
                .ok()
                .map(Path::to_path_buf)
        }

        fn protected(&self, path: &str) -> bool {
            let Some(rel) = self.inside(path) else {
                return true;
            };
            rel.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(
                        ".git"
                            | ".husky"
                            | ".claude"
                            | ".github"
                            | ".mcp.json"
                            | "AGENTS.md"
                            | "rules"
                    )
                )
            })
        }
    }

    impl TargetCheck for Stub {
        fn read_target(&self, _program: &str, path: &str) -> bool {
            self.inside(path).is_some_and(|rel| {
                !rel.to_string_lossy().contains(".env") && !rel.ends_with("secret.pem")
            })
        }

        fn edit_target(&self, path: &str) -> bool {
            !self.protected(path) && Path::new(path).is_file()
        }

        fn write_target(&self, path: &str) -> bool {
            !self.protected(path) && !Path::new(path).is_dir()
        }

        fn not_protected(&self, path: &str) -> bool {
            !self.protected(path)
        }

        fn hooks_inactive(&self) -> bool {
            true
        }

        fn tracked_entry(&self, path: &str) -> bool {
            !self.protected(path)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        scope: Scope,
        stub: Stub,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical root");
        for file in [
            "a.txt",
            "b.txt",
            "src/x.rs",
            "AGENTS.md",
            ".mcp.json",
            "secret.pem",
            ".env",
            "sub/mod/f.txt",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, "a\n").expect("write");
        }
        Fixture {
            scope: Scope::new(root.clone(), []),
            stub: Stub { root },
            _dir: dir,
        }
    }

    fn accepts(fixture: &Fixture, command: &str) -> bool {
        let env = Environment {
            scope: &fixture.scope,
            cwd: None,
        };
        classify(command, &env, &Preclassifier::default(), &fixture.stub)
    }

    #[test]
    fn each_verb_is_accepted_in_its_allowed_forms() {
        let f = fixture();
        for command in [
            "sed -i 's/a/b/' a.txt",
            "sed -i -e 's/a/b/' -e '/x/d' a.txt b.txt",
            "sed -i --expression='s/a/b/' a.txt",
            "sed --in-place -E -s 's/(a)/\\1/g' a.txt",
            "sed -Ei 's/a/b/' a.txt",
            "sed -Ee 's/a/b/' -i a.txt",
            "sed -i -- 's/a/b/' a.txt",
            "sed -i '1,3d' a.txt",
            "sed -i '$d' a.txt",
            "sed -i '/^<<<<<<< /,/^=======$/d' a.txt",
            "sed -i 's|a/b|c\\|d|2g' a.txt",
            "sed -i 's#a#b#I' a.txt",
            "sed -i 's/a/b/;s/c/d/' a.txt",
            "sed -i 's/a/b/\n/x/d' a.txt",
            "sed -i 's/\\//:/' a.txt",
            "mkdir -p docs/new deeper/still",
            "mkdir docs",
            "mv a.txt c.txt",
            "mv -n a.txt c.txt",
            "mv -- a.txt b.txt src",
            "git add a.txt src/x.rs",
            "git add -u",
            "git add -u -v a.txt",
            "git add -- a.txt",
            "git rm a.txt",
            "git rm -f a.txt",
            "git rm --force a.txt",
            "git rm -q --cached a.txt b.txt",
            "git mv -v a.txt c.txt",
            "git mv -f a.txt b.txt",
            "rm a.txt",
            "rm -f a.txt b.txt",
            "rm -v -- a.txt",
            "rm --force --verbose a.txt",
            "git commit -m msg",
            "git commit -m 'two words' -m 'second'",
            "git commit -am msg",
            "git commit --message=msg --all -q --no-edit",
            "git commit -F a.txt",
            "git commit --file=a.txt",
            "GIT_EDITOR=true git rebase --continue",
            "GIT_SEQUENCE_EDITOR=: GIT_EDITOR=: git merge --continue",
            "git cherry-pick --continue",
            "git revert --continue",
            "! grep -n '^<<<<<<<' a.txt",
            "ls -la src; pwd",
            "grep -rn x src | head -n 3 | wc -l",
            "cat a.txt || ls",
            "git status && git diff --cached a.txt",
            "cat > c.txt <<'EOF'\nhello $x\nEOF",
            "cat >> a.txt <<'EOF'\nmore\nEOF",
            "grep x a.txt > /dev/null 2>&1",
            "sed -i 's/a/b/' a.txt && git add a.txt && git commit -m x && git status",
        ] {
            assert_eq!(
                accepts(&f, command),
                GNU_SED || !command.contains("sed "),
                "{command}"
            );
        }
    }

    #[test]
    fn the_golden_adversarial_set_is_rejected() {
        let f = fixture();
        for command in [
            // sed constructs that run a command or write a file
            "sed -i 'e echo x' a.txt",
            "sed -i 'w out' a.txt",
            "sed -i 'r a.txt' a.txt",
            "sed -i 's/a/b/e' a.txt",
            "sed -i 's/a/b/w out' a.txt",
            "sed -i 's/[/]/;sdx/e;d;d' a.txt",
            "sed -i 's/[/]/;sdx/w PWN2;d;d' a.txt",
            "sed -i 's/a/b/p' a.txt",
            "sed -i 's/a/b/m' a.txt",
            "sed -i '/a/{d}' a.txt",
            "sed -i 'y/a/b/' a.txt",
            "sed -i '1a text' a.txt",
            "sed -i '1i text' a.txt",
            "sed -i '1c text' a.txt",
            "sed -i ':l;s/a/b/;tl' a.txt",
            "sed -i '/a/!d' a.txt",
            "sed -i '#n' a.txt",
            "sed -i '1~2d' a.txt",
            "sed -i 's/a/b' a.txt",
            "sed -i 's/a/b/\\' a.txt",
            "sed -i -f script.sed a.txt",
            "sed -i --follow-symlinks 's/a/b/' a.txt",
            "sed -i -z 's/a/b/' a.txt",
            "sed -i.bak 's/a/b/' a.txt",
            "sed -ie 's/a/b/' a.txt",
            "sed -n 's/a/b/p' a.txt",
            "sed 's/a/b/' a.txt",
            "sed -i 's/a/b/'",
            "sed -i 's/a/b/' missing.txt",
            "sed -i 's/a/b/' AGENTS.md",
            "sed -i 's/a/b/' .mcp.json",
            "sed -i 's/a/b/' src",
            "sed -i 's/a/b/' $F",
            "sed -i 's/a/b/' *.txt",
            "sed -i 's/a/b/' ~/a.txt",
            // redirects
            "echo x > a.txt",
            "cat a.txt > /etc/hosts",
            "cat a.txt > $T",
            "cat a.txt > ~/x",
            "cat a.txt > .git/config",
            "cat a.txt > .mcp.json",
            "cat < /etc/passwd",
            "cat <<< hi",
            "cat <<EOF\n$(id)\nEOF",
            "cat a.txt >& other",
            "cat a.txt <> a.txt",
            "cat a.txt > a.txt 3< /etc/passwd",
            // environment, directories and substitutions
            "cd .. && sed -i 's/a/b/' a.txt",
            "pushd src && git status",
            "FOO=1 git commit -m x",
            "GIT_EDITOR=vim git rebase --continue",
            "GIT_EDITOR=$(id) git rebase --continue",
            "GIT_EDITOR=true sed -i 's/a/b/' a.txt",
            "GIT_EDITOR=true ls",
            "GIT_EDITOR=true",
            "echo $(id)",
            "cat `id`",
            "git commit -m $(id)",
            "git commit -m \"$HOME\"",
            // control structures
            "sed -i 's/a/b/' a.txt &",
            "git status & git add a.txt",
            "sed -i 's/a/b/' a.txt | sh",
            "ls | sh",
            "ls | xargs rm",
            "time ls",
            "for f in a.txt; do git add $f; done",
            "if true; then git status; fi",
            "(git status)",
            "{ git status; }",
            "f() { git status; }; f",
            "[[ -f a.txt ]] && git status",
            "git status; (rm -rf x)",
            "diff <(ls) <(ls)",
            "((1+1))",
            "",
            "git status &&",
            "ls 'unterminated",
            // git
            "git -c core.hooksPath=x commit -m x",
            "git -C .. status",
            "git --git-dir=x status",
            "git commit --amend -m x",
            "git commit --amend",
            "git commit --no-verify -m x",
            "git commit -n -m x",
            "git commit -m x a.txt",
            "git commit",
            "git commit --fixup=HEAD",
            "git commit -c HEAD",
            "git commit -C HEAD",
            "git commit -F /etc/passwd",
            "git commit -F -",
            "git rm -r src",
            "git rm -rf src",
            "rm -r src",
            "rm -R src",
            "rm -d src",
            "rm --recursive src",
            "rm -i a.txt",
            "rm --no-preserve-root a.txt",
            "rm src",
            "rm",
            "rm -f",
            "rm -",
            "rm .git/x",
            "rm $(ls)",
            "rm *",
            "rm missing.txt",
            "rm a.txt && rm /tmp/x",
            "git rm src",
            "git rm --cached '.git*'",
            "git rm --cached ':/'",
            "git rm AGENTS.md",
            "git rm --cached .mcp.json",
            "git rm --cached",
            "git mv -k a.txt b.txt",
            "git mv a.txt",
            "git mv a.txt b.txt c.txt",
            "git mv a.txt .husky/pre-commit",
            "git mv .mcp.json x",
            "git mv AGENTS.md x",
            "git mv src moved",
            "git add -A",
            "git add --all",
            "git add .",
            "git add :/",
            "git add ':(top)a.txt'",
            "git add '*.txt'",
            "git add 'a.tx?'",
            "git add 'a[.]txt'",
            "git add src",
            "git add missing.txt",
            "git add .mcp.json",
            "git add secret.pem",
            "git add .env",
            "git add .git/config",
            "git add -f a.txt",
            "git add -p a.txt",
            "git add",
            "git rebase --abort",
            "git rebase --skip",
            "git rebase --continue --abort",
            "git rebase main",
            "git rebase -i HEAD~3",
            "git merge --abort",
            "git merge --continue x",
            "git push",
            "git push --force origin main",
            "git reset --hard",
            "git checkout -- .",
            "git stash",
            "git clean -fd",
            "git config user.name x",
            "git -h",
            "/usr/bin/git status",
            "./git status",
            // other commands
            "rm -rf src",
            "touch a.txt",
            "cp a.txt b.txt",
            "python3 x.py",
            "make",
            "echo hi",
            "xargs rm",
            // mv, mkdir
            "mv secret.pem x",
            "mv .env x",
            "mv a.txt",
            "mv a.txt .husky/x",
            "mv a.txt /tmp/x",
            "mv /etc/hosts x",
            "mv AGENTS.md x",
            "mv src moved",
            "mv -f a.txt b.txt",
            "mv a.txt .git/x",
            "mkdir .claude/x",
            "mkdir .git/x",
            "mkdir /tmp/x",
            "mkdir -m 777 x",
            "mkdir",
            "mkdir -p .husky && git mv a.txt .husky/pre-commit",
            "mkdir -p docs/rules && mv a.txt docs/rules",
            "mkdir -p docs/rules && git mv a.txt docs/rules",
            "mkdir -p sub/mod",
        ] {
            assert!(!accepts(&f, command), "{command:?} must not take the tier");
        }
    }

    #[test]
    fn a_working_directory_outside_every_root_rejects() {
        let f = fixture();
        let env = Environment {
            scope: &f.scope,
            cwd: Some("/etc"),
        };
        assert!(!classify(
            "git status",
            &env,
            &Preclassifier::default(),
            &f.stub
        ));
        let env = Environment {
            scope: &f.scope,
            cwd: Some("src"),
        };
        assert_eq!(
            classify(
                "sed -i 's/a/b/' x.rs && git add x.rs",
                &env,
                &Preclassifier::default(),
                &f.stub
            ),
            GNU_SED
        );
    }

    #[test]
    fn a_user_extra_never_receives_a_heredoc() {
        let f = fixture();
        let extras = Preclassifier::new(&["make".to_string(), "sqlite3".to_string()]);
        let check = |command: &str| {
            let env = Environment {
                scope: &f.scope,
                cwd: None,
            };
            classify(command, &env, &extras, &f.stub)
        };
        assert!(!check("make <<'EOF'\nx\nEOF"));
        assert!(!check("sqlite3 data.db <<'EOF'\n.shell touch x\nEOF"));
        assert!(check("cat > c.txt <<'EOF'\nhello\nEOF"));
    }

    #[test]
    fn a_user_extra_is_a_first_stage_only() {
        let f = fixture();
        let extras = Preclassifier::new(&["xargs".to_string(), "make".to_string()]);
        let check = |command: &str| {
            let env = Environment {
                scope: &f.scope,
                cwd: None,
            };
            classify(command, &env, &extras, &f.stub)
        };
        assert!(check("make"));
        assert!(check("make && git add a.txt"));
        assert!(check("make | wc -l"));
        assert!(!check("ls | xargs rm"));
        assert!(!check("ls | make"));
    }

    #[test]
    fn sed_scripts_follow_the_restricted_grammar() {
        for script in [
            "d",
            "5d",
            "$d",
            "/x/d",
            "/x/,/y/d",
            "1,$d",
            "s/a/b/",
            "s/a/b/gI2",
            "s,a\\,b,c,",
            "d;d",
            "d\nd\n",
            "  s/a/b/  ;  d ",
            "",
        ] {
            assert!(sed_script_ok(script), "{script:?}");
        }
        for script in [
            "p",
            "q",
            "n",
            "=",
            "l",
            "{d}",
            "}",
            "e",
            "w f",
            "r f",
            "R f",
            "W f",
            "s/a/b/e",
            "s/a/b/w f",
            "s/a/b/pp",
            "s/a/b/;p",
            "1,d",
            ",5d",
            "/a",
            "s/a",
            "s/a/b",
            "s\n",
            "s\\a\\b\\",
            "dd",
            "d p",
            "s/[/]/;sdx/e;d;d",
            "s/[/]/;sdx/w PWN2;d;d",
        ] {
            assert!(!sed_script_ok(script), "{script:?}");
        }
    }
}
