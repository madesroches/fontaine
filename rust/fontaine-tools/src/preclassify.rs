//! Shell command analysis on a real shell parser (`brush-parser`, never a hand-rolled lexer or
//! a regex): the word stream that permission rules match, the targets of `rm`/`rmdir` for the
//! critical-path check, and the Tier-0.5 pre-classifier that lets a few read-only commands skip
//! the human prompt and the guardian.
//!
//! The soundness rule everywhere: whatever cannot be fully parsed as one simple command made of
//! literal words is reported as such, so callers fall through to the stricter path.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use brush_parser::ast::{
    AndOrList, Command, CommandPrefixOrSuffixItem, CompoundCommand, CompoundList,
    IoFileRedirectKind, IoFileRedirectTarget, IoRedirect, Program, SeparatorOperator, Word,
};
use brush_parser::word::{WordPiece, WordPieceWithSource};
use brush_parser::{Parser, ParserOptions};

use crate::scope::Scope;

/// Command substitutions nested deeper than this are not followed; the analysis is then marked
/// as incomplete.
const MAX_DEPTH: usize = 4;

/// One word of a command: its raw text and, when the shell would pass it through unchanged, the
/// literal text it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellWord {
    pub raw: String,
    /// `None` when the word expands (parameter, command substitution, tilde, brace pattern,
    /// unquoted glob character).
    pub literal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SimpleCommand {
    pub words: Vec<ShellWord>,
}

impl SimpleCommand {
    /// The literal text of every word, or `None` if any word expands.
    pub fn literal_argv(&self) -> Option<Vec<String>> {
        self.words.iter().map(|w| w.literal.clone()).collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ShellAnalysis {
    /// False when the parser rejected the text.
    pub parsed: bool,
    /// Every simple command found, including those inside pipelines, lists, compound commands
    /// and command substitutions.
    pub commands: Vec<SimpleCommand>,
    /// The command, when the text is exactly one simple command: no control operator, no
    /// redirection, no assignment prefix, no background marker, no substitution, and every word
    /// literal.
    pub simple: Option<Vec<String>>,
    /// A runner's command-line argument held more than one simple command (`bash -c 'a; b'`).
    pub nested_chain: bool,
    /// Some construct was not followed (arithmetic, a parameter expansion that may hold a
    /// substitution, a deep nesting); callers that need completeness fall back to the raw text.
    pub unanalyzed: bool,
    /// Some command, anywhere in the text, redirects output to a file (`>`, `>>`, `>|`, `<>`,
    /// `&>`). Duplicating a descriptor (`2>&1`) is not a file.
    pub output_redirect: bool,
    /// The target word of every redirect that writes a file, in source order, including those on
    /// compound commands, in substitutions and in nested runner command lines. Descriptor copies
    /// (`2>&1`) have none. A target that expands has `literal == None`.
    pub output_targets: Vec<ShellWord>,
    pub raw: String,
}

fn options() -> ParserOptions {
    ParserOptions::default()
}

pub fn analyze(command: &str) -> ShellAnalysis {
    analyze_at(command, 0)
}

fn analyze_at(command: &str, depth: usize) -> ShellAnalysis {
    let mut analysis = ShellAnalysis {
        raw: command.to_string(),
        ..ShellAnalysis::default()
    };
    if depth > MAX_DEPTH {
        analysis.unanalyzed = true;
        return analysis;
    }
    let mut parser = Parser::new(Cursor::new(command.as_bytes()), &options());
    let program = match parser.parse_program() {
        Ok(program) => program,
        Err(_) => return analysis,
    };
    analysis.parsed = true;
    let mut walk = Walk {
        out: &mut analysis,
        depth,
    };
    walk.program(&program);
    analysis.simple = simple_of(&program);
    if analysis.nested_chain {
        analysis.simple = None;
    }
    analysis
}

struct Walk<'a> {
    out: &'a mut ShellAnalysis,
    depth: usize,
}

impl Walk<'_> {
    fn program(&mut self, program: &Program) {
        for list in &program.complete_commands {
            self.list(list);
        }
    }

    fn list(&mut self, list: &CompoundList) {
        for item in &list.0 {
            self.and_or(&item.0);
        }
    }

    fn and_or(&mut self, list: &AndOrList) {
        self.pipeline(&list.first);
        for next in &list.additional {
            match next {
                brush_parser::ast::AndOr::And(p) | brush_parser::ast::AndOr::Or(p) => {
                    self.pipeline(p)
                }
            }
        }
    }

    fn pipeline(&mut self, pipeline: &brush_parser::ast::Pipeline) {
        for command in &pipeline.seq {
            self.command(command);
        }
    }

    fn command(&mut self, command: &Command) {
        match command {
            Command::Simple(simple) => self.simple(simple),
            Command::Compound(compound, redirects) => {
                self.compound(compound);
                if let Some(redirects) = redirects {
                    for redirect in &redirects.0 {
                        self.redirect(redirect);
                    }
                }
            }
            Command::Function(function) => {
                self.compound(&function.body.0);
                if let Some(redirects) = &function.body.1 {
                    for redirect in &redirects.0 {
                        self.redirect(redirect);
                    }
                }
            }
            Command::ExtendedTest(_, _) => self.out.unanalyzed = true,
        }
    }

    fn compound(&mut self, compound: &CompoundCommand) {
        match compound {
            CompoundCommand::Arithmetic(_) => self.out.unanalyzed = true,
            CompoundCommand::ArithmeticForClause(c) => {
                self.out.unanalyzed = true;
                self.list(&c.body.list);
            }
            CompoundCommand::BraceGroup(c) => self.list(&c.list),
            CompoundCommand::Subshell(c) => self.list(&c.list),
            CompoundCommand::ForClause(c) => {
                for word in c.values.iter().flatten() {
                    self.word(word);
                }
                self.list(&c.body.list);
            }
            CompoundCommand::CaseClause(c) => {
                self.word(&c.value);
                for item in &c.cases {
                    for pattern in &item.patterns {
                        self.word(pattern);
                    }
                    if let Some(list) = &item.cmd {
                        self.list(list);
                    }
                }
            }
            CompoundCommand::IfClause(c) => {
                self.list(&c.condition);
                self.list(&c.then);
                for other in c.elses.iter().flatten() {
                    if let Some(condition) = &other.condition {
                        self.list(condition);
                    }
                    self.list(&other.body);
                }
            }
            CompoundCommand::WhileClause(c) | CompoundCommand::UntilClause(c) => {
                self.list(&c.0);
                self.list(&c.1.list);
            }
            CompoundCommand::Coprocess(c) => self.command(&c.body),
        }
    }

    fn redirect(&mut self, redirect: &IoRedirect) {
        match redirect {
            IoRedirect::File(_, kind, target) => match target {
                IoFileRedirectTarget::Filename(word) => {
                    let writes = matches!(
                        kind,
                        IoFileRedirectKind::Write
                            | IoFileRedirectKind::Append
                            | IoFileRedirectKind::ReadAndWrite
                            | IoFileRedirectKind::Clobber
                    );
                    self.out.output_redirect |= writes;
                    let classified = self.word(word);
                    if writes {
                        self.out.output_targets.push(classified);
                    }
                }
                IoFileRedirectTarget::Duplicate(word) => {
                    // `>&2` copies a descriptor; `>&file` is how a shell spells `> file 2>&1`.
                    let classified = self.word(word);
                    let copies_descriptor = classified.literal.as_deref().is_some_and(|text| {
                        text == "-"
                            || (!text.is_empty() && text.chars().all(|c| c.is_ascii_digit()))
                    });
                    if matches!(kind, IoFileRedirectKind::DuplicateOutput) && !copies_descriptor {
                        self.out.output_redirect = true;
                        self.out.output_targets.push(classified);
                    }
                }
                IoFileRedirectTarget::Fd(_) => {}
                IoFileRedirectTarget::ProcessSubstitution(_, sub) => self.list(&sub.list),
            },
            IoRedirect::HereDocument(_, doc) => {
                if doc.requires_expansion {
                    self.word(&doc.doc);
                }
            }
            IoRedirect::OutputAndError(word, _) => {
                self.out.output_redirect = true;
                let classified = self.word(word);
                self.out.output_targets.push(classified);
            }
            IoRedirect::HereString(_, word) => {
                self.word(word);
            }
        }
    }

    fn simple(&mut self, simple: &brush_parser::ast::SimpleCommand) {
        let mut words = Vec::new();
        let prefix = simple.prefix.iter().flat_map(|p| &p.0);
        let suffix = simple.suffix.iter().flat_map(|s| &s.0);
        // The command name sits between the prefix (assignments, redirects) and the suffix.
        for item in prefix {
            self.item(item, &mut None);
        }
        if let Some(name) = &simple.word_or_name {
            words.push(self.word(name));
        }
        for item in suffix {
            self.item(item, &mut Some(&mut words));
        }
        // Only the arguments of a command runner hold a command line; any other quoted text
        // with spaces is data.
        if let Some(runner) = (0..words.len()).find(|&at| is_runner(&words, at)) {
            for word in &words[runner + 1..] {
                self.nested(word);
            }
        }
        self.out.commands.push(SimpleCommand { words });
    }

    /// A literal word that a runner executes (`sh -c 'rm -rf /'`) is read as a command line too.
    fn nested(&mut self, word: &ShellWord) {
        let Some(text) = &word.literal else { return };
        if !text.contains(char::is_whitespace) {
            return;
        }
        let nested = analyze_at(text, self.depth + 1);
        if !nested.parsed {
            self.out.unanalyzed = true;
        }
        self.out.unanalyzed |= nested.unanalyzed;
        self.out.output_redirect |= nested.output_redirect;
        self.out.output_targets.extend(nested.output_targets);
        self.out.nested_chain |= nested.nested_chain || (nested.parsed && nested.simple.is_none());
        self.out.commands.extend(nested.commands);
    }

    fn item(&mut self, item: &CommandPrefixOrSuffixItem, words: &mut Option<&mut Vec<ShellWord>>) {
        match item {
            CommandPrefixOrSuffixItem::Word(word) => {
                let classified = self.word(word);
                if let Some(words) = words {
                    words.push(classified);
                }
            }
            // `export A=b` carries its assignment as a word; a prefix assignment is not a word
            // of the command, but its value can still hold a substitution.
            CommandPrefixOrSuffixItem::AssignmentWord(_, word) => {
                let classified = self.word(word);
                if let Some(words) = words {
                    words.push(classified);
                }
            }
            CommandPrefixOrSuffixItem::IoRedirect(redirect) => self.redirect(redirect),
            CommandPrefixOrSuffixItem::ProcessSubstitution(_, sub) => self.list(&sub.list),
        }
    }

    /// Classifies a word and follows any command substitution inside it.
    fn word(&mut self, word: &Word) -> ShellWord {
        let pieces = match brush_parser::word::parse(&word.value, &options()) {
            Ok(pieces) => pieces,
            Err(_) => {
                self.out.unanalyzed = true;
                return ShellWord {
                    raw: word.value.clone(),
                    literal: None,
                };
            }
        };
        let mut literal = Some(String::new());
        let mut skeleton = String::new();
        self.pieces(&pieces, &mut literal, &mut skeleton);
        if literal.is_some() && has_brace_pattern(&skeleton) {
            literal = None;
        }
        ShellWord {
            raw: word.value.clone(),
            literal,
        }
    }

    /// `skeleton` collects the word with every quoted or expanded part replaced by `x`, which
    /// is what brace expansion sees.
    fn pieces(
        &mut self,
        pieces: &[WordPieceWithSource],
        literal: &mut Option<String>,
        skeleton: &mut String,
    ) {
        for piece in pieces {
            match &piece.piece {
                WordPiece::Text(text) => {
                    skeleton.push_str(text);
                    if text.contains(['*', '?', '[', ']', '(', ')']) {
                        *literal = None;
                    } else if let Some(out) = literal {
                        out.push_str(text);
                    }
                }
                WordPiece::SingleQuotedText(text) => {
                    skeleton.push('x');
                    if let Some(out) = literal {
                        out.push_str(text);
                    }
                }
                WordPiece::EscapeSequence(text) => {
                    skeleton.push('x');
                    if let Some(out) = literal {
                        out.push_str(text.strip_prefix('\\').unwrap_or(text));
                    }
                }
                WordPiece::DoubleQuotedSequence(inner)
                | WordPiece::GettextDoubleQuotedSequence(inner) => {
                    // Wildcards are literal inside double quotes.
                    skeleton.push('x');
                    for piece in inner {
                        match &piece.piece {
                            WordPiece::Text(text) => {
                                if let Some(out) = literal {
                                    out.push_str(text);
                                }
                            }
                            WordPiece::EscapeSequence(text) => {
                                if let Some(out) = literal {
                                    out.push_str(text.strip_prefix('\\').unwrap_or(text));
                                }
                            }
                            other => self.expanding(other, literal),
                        }
                    }
                }
                other => {
                    skeleton.push('x');
                    self.expanding(other, literal);
                }
            }
        }
    }

    /// A piece the shell expands at run time.
    fn expanding(&mut self, piece: &WordPiece, literal: &mut Option<String>) {
        *literal = None;
        match piece {
            WordPiece::CommandSubstitution(inner)
            | WordPiece::BackquotedCommandSubstitution(inner) => {
                let nested = analyze_at(inner, self.depth + 1);
                if !nested.parsed {
                    self.out.unanalyzed = true;
                }
                self.out.unanalyzed |= nested.unanalyzed;
                self.out.output_redirect |= nested.output_redirect;
                self.out.output_targets.extend(nested.output_targets);
                self.out.commands.extend(nested.commands);
                if !nested.parsed {
                    // The inner text still counts as part of the command for textual fallbacks.
                    self.out.raw.push(' ');
                    self.out.raw.push_str(inner);
                }
            }
            WordPiece::ParameterExpansion(_)
            | WordPiece::ArithmeticExpression(_)
            | WordPiece::TildeExpansion(_) => {
                // Defaults such as `${x:-$(cmd)}` can hold a substitution this walk does not see.
                self.out.unanalyzed = true;
            }
            _ => {}
        }
    }
}

/// Brace expansion (`{a,b}`, `{1..3}`) turns one word into several.
/// Programs that run a string argument as a command line. Launchers that take an argv, such as
/// `sudo` or `timeout`, are not here: their arguments are already words of the command.
const RUNNERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ash", "ksh", "csh", "tcsh", "fish", "eval", "su", "runuser",
    "watch", "flock", "script", "ssh",
];

/// The program a command word names: directory (either separator) and a trailing `.exe` are
/// dropped. A model can send Windows-style names to any host, so this never depends on the OS.
/// An `.exe` name is also lowercased, since Windows resolves it case-insensitively.
pub fn program_name(word: &str) -> std::borrow::Cow<'_, str> {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let stem_len = name.len().saturating_sub(4);
    if name.len() > 4
        && name.is_char_boundary(stem_len)
        && name[stem_len..].eq_ignore_ascii_case(".exe")
    {
        std::borrow::Cow::Owned(name[..stem_len].to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

fn is_runner(words: &[ShellWord], at: usize) -> bool {
    let Some(text) = words[at].literal.as_deref() else {
        return false;
    };
    let name = program_name(text);
    if name == "env" {
        // Only `env -S 'cmd args'` splits a string into a command; plain `env` takes an argv.
        return words[at + 1..].iter().any(|w| {
            w.literal
                .as_deref()
                .is_some_and(|a| a.starts_with("-S") || a.starts_with("--split-string"))
        });
    }
    RUNNERS.contains(&&*name)
}

fn has_brace_pattern(raw: &str) -> bool {
    if !raw.contains('{') {
        return false;
    }
    !matches!(
        brush_parser::word::parse_brace_expansions(raw, &options()),
        Ok(None)
    )
}

fn simple_of(program: &Program) -> Option<Vec<String>> {
    let [list] = program.complete_commands.as_slice() else {
        return None;
    };
    let [item] = list.0.as_slice() else {
        return None;
    };
    if !matches!(item.1, SeparatorOperator::Sequence) || !item.0.additional.is_empty() {
        return None;
    }
    let pipeline = &item.0.first;
    if pipeline.bang || pipeline.timed.is_some() {
        return None;
    }
    let [Command::Simple(simple)] = pipeline.seq.as_slice() else {
        return None;
    };
    if simple.prefix.as_ref().is_some_and(|p| !p.0.is_empty()) {
        return None;
    }
    let name = simple.word_or_name.as_ref()?;
    let mut raw: Vec<&Word> = vec![name];
    for item in simple.suffix.iter().flat_map(|s| &s.0) {
        match item {
            CommandPrefixOrSuffixItem::Word(word) => raw.push(word),
            _ => return None,
        }
    }
    let mut argv = Vec::with_capacity(raw.len());
    for word in raw {
        argv.push(literal_text(&word.value)?);
    }
    Some(argv)
}

/// The text of a word the shell passes through unchanged, or `None` if it would expand.
pub fn literal_text(raw: &str) -> Option<String> {
    let mut scratch = ShellAnalysis::default();
    let mut walk = Walk {
        out: &mut scratch,
        depth: MAX_DEPTH,
    };
    let word = walk.word(&Word::new(raw));
    word.literal
}

/// The words `rm` and `rmdir` as whole tokens of text the parser could not read, found by
/// splitting on everything that cannot be part of a command name.
pub fn raw_mentions_word(text: &str, words: &[&str]) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '+')))
        .any(|token| words.contains(&token))
}

/// Where the command runs and what it may touch.
pub struct Environment<'a> {
    pub scope: &'a Scope,
    /// The call's `cwd` argument, as the model wrote it.
    pub cwd: Option<&'a str>,
}

impl Environment<'_> {
    /// The resolved working directory, or `None` when it is outside every root.
    fn base(&self) -> Option<PathBuf> {
        match self.cwd {
            None => Some(self.scope.primary().to_path_buf()),
            Some(cwd) => self.scope.find(cwd).ok().map(|(_, path)| path),
        }
    }

    fn in_scope(&self, base: &Path, text: &str) -> bool {
        let absolute = Path::new(text).is_absolute() || text.starts_with(['/', '\\']);
        let joined = if absolute {
            text.to_string()
        } else {
            base.join(text).to_string_lossy().into_owned()
        };
        self.scope.find(&joined).is_ok()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Val {
    None,
    Int,
    Path,
    Word,
}

struct Spec {
    command: &'static str,
    short: &'static [(char, Val)],
    long: &'static [(&'static str, Val)],
}

const SPECS: &[Spec] = &[
    Spec {
        command: "ls",
        short: &[
            ('a', Val::None),
            ('A', Val::None),
            ('l', Val::None),
            ('h', Val::None),
            ('1', Val::None),
            ('d', Val::None),
            ('t', Val::None),
            ('r', Val::None),
            ('S', Val::None),
            ('F', Val::None),
        ],
        long: &[],
    },
    Spec {
        command: "pwd",
        short: &[],
        long: &[],
    },
    Spec {
        command: "wc",
        short: &[
            ('l', Val::None),
            ('w', Val::None),
            ('c', Val::None),
            ('m', Val::None),
        ],
        long: &[],
    },
    Spec {
        command: "file",
        short: &[('b', Val::None), ('i', Val::None), ('m', Val::Path)],
        long: &[
            ("mime", Val::None),
            ("mime-type", Val::None),
            ("brief", Val::None),
            ("magic-file", Val::Path),
        ],
    },
    Spec {
        command: "cat",
        short: &[('n', Val::None), ('b', Val::None), ('s', Val::None)],
        long: &[],
    },
    Spec {
        command: "head",
        short: &[
            ('n', Val::Int),
            ('c', Val::Int),
            ('q', Val::None),
            ('v', Val::None),
        ],
        long: &[],
    },
    Spec {
        command: "tail",
        short: &[
            ('n', Val::Int),
            ('c', Val::Int),
            ('q', Val::None),
            ('v', Val::None),
        ],
        long: &[],
    },
    Spec {
        command: "rg",
        short: &[
            ('i', Val::None),
            ('s', Val::None),
            ('S', Val::None),
            ('n', Val::None),
            ('N', Val::None),
            ('H', Val::None),
            ('w', Val::None),
            ('x', Val::None),
            ('F', Val::None),
            ('v', Val::None),
            ('c', Val::None),
            ('l', Val::None),
            ('A', Val::Int),
            ('B', Val::Int),
            ('C', Val::Int),
            ('m', Val::Int),
            ('t', Val::Word),
            ('T', Val::Word),
            ('f', Val::Path),
        ],
        long: &[
            ("hidden", Val::None),
            ("no-heading", Val::None),
            ("file", Val::Path),
            ("ignore-file", Val::Path),
        ],
    },
    Spec {
        command: "grep",
        short: &[
            ('i', Val::None),
            ('n', Val::None),
            ('v', Val::None),
            ('w', Val::None),
            ('x', Val::None),
            ('F', Val::None),
            ('E', Val::None),
            ('c', Val::None),
            ('l', Val::None),
            ('L', Val::None),
            ('H', Val::None),
            ('h', Val::None),
            ('o', Val::None),
            ('q', Val::None),
            ('s', Val::None),
            ('r', Val::None),
            ('A', Val::Int),
            ('B', Val::Int),
            ('C', Val::Int),
            ('f', Val::Path),
        ],
        long: &[("file", Val::Path)],
    },
];

/// The curated read-only set plus the user's flag-free additions.
#[derive(Clone, Debug, Default)]
pub struct Preclassifier {
    extra: Vec<Vec<String>>,
}

impl Preclassifier {
    /// `allow_extra` entries are a command (`make`) or a `command subcommand` pair
    /// (`cargo metadata`); anything else is ignored.
    pub fn new(allow_extra: &[String]) -> Preclassifier {
        let extra = allow_extra
            .iter()
            .map(|entry| {
                entry
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|words: &Vec<String>| {
                (1..=2).contains(&words.len()) && words.iter().all(|w| !w.starts_with('-'))
            })
            .collect();
        Preclassifier { extra }
    }

    /// Whether `command` is one simple command of the curated set (or an extra entry) that
    /// reads only inside the workspace roots. Anything not fully understood is `false`.
    pub fn is_read_only(&self, command: &str, env: &Environment<'_>) -> bool {
        self.read_paths(command, env).is_some()
    }

    /// The path operands (positionals and path-valued flags, however attached) that
    /// `is_read_only` checked against the workspace, as the command wrote them; `None` when the
    /// command is not read-only.
    pub fn read_paths(&self, command: &str, env: &Environment<'_>) -> Option<Vec<String>> {
        let analysis = analyze(command);
        let argv = analysis.simple.as_ref()?;
        let base = env.base()?;
        let name = argv[0].as_str();
        let mut paths = Vec::new();
        if let Some(spec) = SPECS.iter().find(|s| s.command == name) {
            return curated(spec, &argv[1..], env, &base, &mut paths).then_some(paths);
        }
        let allowed = self.extra.iter().any(|entry| {
            if entry[0] != name {
                return false;
            }
            let rest = &argv[1..];
            let rest = match entry.get(1) {
                Some(sub) => match rest.split_first() {
                    Some((first, tail)) if first == sub => tail,
                    _ => return false,
                },
                None => rest,
            };
            let ok = rest
                .iter()
                .all(|arg| !arg.starts_with('-') && env.in_scope(&base, arg));
            if ok {
                paths.extend(rest.iter().cloned());
            }
            ok
        });
        allowed.then_some(paths)
    }
}

fn value_ok(
    kind: Val,
    value: &str,
    env: &Environment<'_>,
    base: &Path,
    paths: &mut Vec<String>,
) -> bool {
    match kind {
        Val::None => false,
        Val::Int => value.parse::<i64>().is_ok(),
        Val::Path => {
            let ok = !value.starts_with('-') && env.in_scope(base, value);
            if ok {
                paths.push(value.to_string());
            }
            ok
        }
        Val::Word => {
            !value.is_empty()
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && !value.starts_with('-')
        }
    }
}

fn curated(
    spec: &Spec,
    args: &[String],
    env: &Environment<'_>,
    base: &Path,
    paths: &mut Vec<String>,
) -> bool {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if arg == "-" || !arg.starts_with('-') {
            if !env.in_scope(base, arg) {
                return false;
            }
            paths.push(arg.to_string());
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            if long.is_empty() {
                return false;
            }
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let Some((_, kind)) = spec.long.iter().find(|(n, _)| *n == name) else {
                return false;
            };
            match (*kind, attached) {
                (Val::None, None) => {}
                (Val::None, Some(_)) => return false,
                (kind, Some(value)) => {
                    if !value_ok(kind, value, env, base, paths) {
                        return false;
                    }
                }
                (kind, None) => {
                    let Some(value) = args.get(i) else {
                        return false;
                    };
                    i += 1;
                    if !value_ok(kind, value, env, base, paths) {
                        return false;
                    }
                }
            }
            continue;
        }
        let letters: Vec<(usize, char)> = arg.char_indices().skip(1).collect();
        for (n, (at, letter)) in letters.iter().enumerate() {
            let Some((_, kind)) = spec.short.iter().find(|(c, _)| c == letter) else {
                return false;
            };
            if *kind == Val::None {
                continue;
            }
            let rest = &arg[at + letter.len_utf8()..];
            if !rest.is_empty() {
                if !value_ok(*kind, rest, env, base, paths) {
                    return false;
                }
            } else {
                debug_assert_eq!(n, letters.len() - 1);
                let Some(value) = args.get(i) else {
                    return false;
                };
                i += 1;
                if !value_ok(*kind, value, env, base, paths) {
                    return false;
                }
            }
            break;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str) -> Option<String> {
        literal_text(text)
    }

    #[test]
    fn quoted_words_are_literal_and_expansions_are_not() {
        assert_eq!(lit("plain").as_deref(), Some("plain"));
        assert_eq!(lit("'a;b.txt'").as_deref(), Some("a;b.txt"));
        assert_eq!(lit("\"a b\"").as_deref(), Some("a b"));
        assert_eq!(lit("'*.rs'").as_deref(), Some("*.rs"));
        assert_eq!(lit("a\\ b").as_deref(), Some("a b"));
        for text in [
            "*",
            "a*",
            "~",
            "~/x",
            "$HOME",
            "\"$HOME\"",
            "$(pwd)",
            "`pwd`",
            "{a,b}",
            "[ab]",
            "$'x'",
            "@(a|b)",
        ] {
            assert_eq!(lit(text), None, "{text}");
        }
    }

    #[test]
    fn only_a_lone_literal_command_is_simple() {
        assert_eq!(
            analyze("ls -la src").simple,
            Some(vec!["ls".into(), "-la".into(), "src".into()])
        );
        for text in [
            "ls; ls",
            "ls | wc",
            "ls && ls",
            "ls &",
            "ls > out",
            "FOO=1 ls",
            "(ls)",
            "ls $(pwd)",
            "ls\nls",
            "! ls",
            "if true; then ls; fi",
        ] {
            assert_eq!(analyze(text).simple, None, "{text}");
        }
    }

    #[test]
    fn commands_inside_every_construct_are_found() {
        let names = |text: &str| -> Vec<String> {
            let mut names: Vec<String> = analyze(text)
                .commands
                .iter()
                .filter_map(|c| c.words.first().and_then(|w| w.literal.clone()))
                .collect();
            names.sort();
            names
        };
        assert_eq!(names("a; b | c && d"), ["a", "b", "c", "d"]);
        assert_eq!(names("echo $(rm -rf x)"), ["echo", "rm"]);
        assert_eq!(names("(a; b)"), ["a", "b"]);
        assert_eq!(names("for f in $(x); do y; done"), ["x", "y"]);
        assert_eq!(names("ls ; rm x"), ["ls", "rm"]);
    }

    #[test]
    fn only_runner_arguments_are_read_as_command_lines() {
        let count = |text: &str| analyze(text).commands.len();
        assert_eq!(count("git commit -m 'fix git push'"), 1);
        assert_eq!(count("sh -c 'rm x'"), 2);
        assert_eq!(count("sudo bash -c 'rm x'"), 2);
        assert_eq!(count("su -c 'rm x'"), 2);
        assert_eq!(count("env -S 'rm x'"), 2);
        assert_eq!(count("env FOO=1 git commit -m 'a b'"), 1);
        assert!(analyze("git commit -m \"don't rm this\"").parsed);
        assert!(!analyze("git commit -m \"don't rm this\"").unanalyzed);
    }

    #[test]
    fn only_a_redirect_to_a_file_sets_output_redirect() {
        for text in [
            "echo x > f",
            "echo x >> f",
            "echo x >| f",
            "cat <> f",
            "echo x &> f",
            "echo x &>> f",
            "echo x >&f",
            "ls | tee out > f",
            "cat a && echo b > f",
            "{ echo x; } > f",
            "sh -c 'echo x > f'",
            "echo $(echo x > f)",
            "echo x 2> err.log",
        ] {
            assert!(analyze(text).output_redirect, "{text}");
        }
        for text in [
            "cargo test 2>&1",
            "cargo test 2>&1 | tail",
            "ls >&2",
            "ls 1>&-",
            "cat < in.txt",
            "cat <<< word",
            "ls | wc",
            "echo '>' x",
            "echo a\\>b",
        ] {
            assert!(!analyze(text).output_redirect, "{text}");
            assert!(analyze(text).output_targets.is_empty(), "{text}");
        }
        for text in ["cat < in", "cat <<< w"] {
            assert!(analyze(text).output_targets.is_empty(), "{text}");
        }
    }

    #[test]
    fn output_targets_are_listed_in_source_order() {
        let targets = |text: &str| -> Vec<Option<String>> {
            analyze(text)
                .output_targets
                .into_iter()
                .map(|w| w.literal)
                .collect()
        };
        let f = || vec![Some("f".to_string())];
        for text in [
            "echo x > f",
            "echo x >> f",
            "echo x >| f",
            "cat <> f",
            "echo x &> f",
            "echo x &>> f",
            "echo x >&f",
            "{ echo x; } > f",
            "ls | tee out > f",
            "echo $(echo x > f)",
            "sh -c 'echo x > f'",
        ] {
            let got = targets(text);
            assert!(got.last() == f().last(), "{text}: {got:?}");
        }
        assert_eq!(targets("echo x > f"), f());
        assert_eq!(targets("{ echo x; } > f"), f());
        assert_eq!(targets("sh -c 'echo x > f'"), f());
        assert_eq!(
            targets("echo x > a 2> b"),
            vec![Some("a".to_string()), Some("b".to_string())]
        );
        assert_eq!(targets("echo x > $F"), vec![None]);
        assert_eq!(targets("echo x > -x"), vec![Some("-x".to_string())]);
    }

    #[test]
    fn unparseable_text_is_reported() {
        assert!(!analyze("ls 'unterminated").parsed);
        assert!(!analyze("if then").parsed);
    }

    #[test]
    fn raw_word_search_ignores_longer_names() {
        assert!(raw_mentions_word("echo 'x; rm -rf /", &["rm"]));
        assert!(!raw_mentions_word("echo format", &["rm"]));
        assert!(!raw_mentions_word("echo rm-rf", &["rm"]));
    }
}
