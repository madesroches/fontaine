//! The `Tool(specifier)` rule grammar behind `[permissions] deny`, `ask` and `allow`: parsing,
//! matching, and the narrowest rule that covers a call.
//!
//! Matching is structured and never a regex over raw arguments. Deny and ask fail toward
//! matching, allow fails toward not matching.

use std::fmt;
use std::path::{Path, PathBuf};

use fontaine_tools::names::{is_builtin, is_mcp};
use fontaine_tools::preclassify::{ShellAnalysis, analyze, raw_mentions_word};
use fontaine_tools::scope::{Scope, glob_target};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;

use crate::config::Slot;

/// A parsed `Tool` / `Tool(specifier)` rule. The specifier is kept as written; it is parsed
/// again, per tool kind, wherever it is matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub tool: String,
    pub specifier: Option<String>,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.specifier {
            Some(spec) => write!(f, "{}({spec})", self.tool),
            None => f.write_str(&self.tool),
        }
    }
}

/// Where a rule came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleSource {
    Config(Slot),
    /// An "allow session" answer at an approval prompt.
    Session,
}

impl RuleSource {
    pub fn label(self) -> &'static str {
        match self {
            RuleSource::Config(slot) => slot.label(),
            RuleSource::Session => "session",
        }
    }
}

/// A rule together with the scope that contributed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcedRule {
    pub rule: Rule,
    pub source: RuleSource,
}

impl SourcedRule {
    pub fn new(rule: Rule, source: RuleSource) -> SourcedRule {
        SourcedRule { rule, source }
    }

    /// The rule text and its source, as the audit log records it: `shell(rm) [user]`.
    pub fn label(&self) -> String {
        format!("{} [{}]", self.rule, self.source.label())
    }
}

/// Displays as the bare rule text.
impl fmt::Display for SourcedRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.rule.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleError {
    pub rule: String,
    pub reason: String,
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid permission rule `{}`: {}",
            self.rule, self.reason
        )
    }
}

impl std::error::Error for RuleError {}

const PATH_TOOLS: [&str; 6] = ["read", "edit", "write", "ls", "glob", "grep"];

pub fn parse_rule(text: &str) -> Result<Rule, RuleError> {
    let fail = |reason: &str| RuleError {
        rule: text.to_string(),
        reason: reason.to_string(),
    };

    let (tool, specifier) = match text.find('(') {
        None => (text, None),
        Some(open) => {
            if !text.ends_with(')') {
                return Err(fail("expected `Tool(specifier)` with a closing `)`"));
            }
            let inner = &text[open + 1..text.len() - 1];
            if inner.trim().is_empty() {
                return Err(fail("empty specifier; write the bare tool name instead"));
            }
            (&text[..open], Some(inner))
        }
    };

    if tool.is_empty() {
        return Err(fail("missing tool name"));
    }
    if !tool
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || (c == '*' && is_mcp(tool)))
    {
        return Err(fail(
            "tool names contain only letters, digits, `_` and `-` (`*` only ends an `mcp__` name)",
        ));
    }

    if is_mcp(tool) {
        validate_mcp_name(tool).map_err(|reason| fail(&reason))?;
        if specifier.is_some() {
            return Err(fail("MCP rules take no specifier"));
        }
    } else if !is_builtin(tool) {
        return Err(fail(
            "unknown tool; expected a built-in tool name or an `mcp__<server>[__<tool>]` name",
        ));
    }

    if let Some(spec) = specifier {
        validate_specifier(tool, spec).map_err(|reason| fail(&reason))?;
    }

    Ok(Rule {
        tool: tool.to_string(),
        specifier: specifier.map(str::to_string),
    })
}

fn validate_specifier(tool: &str, spec: &str) -> Result<(), String> {
    match tool {
        "shell" => shell_spec(spec).map(drop),
        "webfetch" => validate_domain_specifier(spec),
        "agent" | "skill" => {
            if spec.trim() != spec || spec.contains(char::is_whitespace) {
                Err(format!("`{tool}` rules take one name"))
            } else {
                Ok(())
            }
        }
        "send_message" => Err("`send_message` rules take no specifier".to_string()),
        _ if PATH_TOOLS.contains(&tool) => path_spec(spec).map(drop),
        _ => Ok(()),
    }
}

pub(crate) fn validate_mcp_name(name: &str) -> Result<(), String> {
    let rest = &name[fontaine_tools::names::MCP_TOOL_PREFIX.len()..];
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '*')
    {
        return Err(
            "MCP names contain only letters, digits, `_`, `-` and a trailing `*`".to_string(),
        );
    }
    let (server, tool) = match rest.split_once("__") {
        Some((server, tool)) => (server, Some(tool)),
        None => (rest, None),
    };
    if server.is_empty() || server.contains('*') {
        return Err("expected `mcp__<server>` with a non-empty server name".to_string());
    }
    if let Some(tool) = tool
        && (tool.is_empty() || (tool.contains('*') && tool != "*"))
    {
        return Err("expected `mcp__<server>__<tool>` or `mcp__<server>__*`".to_string());
    }
    Ok(())
}

fn validate_domain_specifier(spec: &str) -> Result<(), String> {
    domain_spec(spec).map(drop)
}

enum Domain {
    Exact(String),
    Subdomains(String),
}

/// Lowercased, punycode form of a host with one trailing root dot dropped, so a rule and a URL
/// compare equal however each spells the same name.
fn normalize_host(host: &str) -> Option<String> {
    let host = host.strip_suffix('.').unwrap_or(host);
    match url::Host::parse(host).ok()? {
        url::Host::Domain(name) => Some(name.strip_suffix('.').unwrap_or(&name).to_string()),
        other => Some(other.to_string()),
    }
}

fn domain_spec(spec: &str) -> Result<Domain, String> {
    let form = "webfetch rules take `domain:<host>` or `domain:*.<host>`".to_string();
    let Some(host) = spec.strip_prefix("domain:") else {
        return Err(form);
    };
    let (wild, host) = match host.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, host),
    };
    if host.is_empty()
        || host.contains(|c: char| c.is_whitespace() || matches!(c, '/' | ':' | '*' | '@'))
    {
        return Err(form);
    }
    let host = normalize_host(host).ok_or(form)?;
    Ok(if wild {
        Domain::Subdomains(host)
    } else {
        Domain::Exact(host)
    })
}

struct ShellSpec {
    words: Vec<String>,
    /// A trailing ` *`: the rule matches any command that starts with `words`.
    wildcard: bool,
}

fn shell_spec(spec: &str) -> Result<ShellSpec, String> {
    if spec.trim() == "*" {
        return Err("write the bare tool name `shell` to match every command".to_string());
    }
    let (body, wildcard) = match spec.strip_suffix(" *") {
        Some(body) => (body, true),
        None => (spec, false),
    };
    let words = analyze(body).simple.ok_or_else(|| {
        "`shell` rules are one simple command of literal words, optionally ending in ` *`; \
         quote a word to make a glob character literal"
            .to_string()
    })?;
    Ok(ShellSpec { words, wildcard })
}

/// How a path pattern is anchored.
#[derive(Debug, PartialEq, Eq)]
enum Anchor {
    /// Each workspace root.
    Roots,
    Home,
    /// The file system root, or the drive named in the pattern.
    Fs(Option<char>),
}

struct PathSpec {
    anchor: Anchor,
    matcher: Gitignore,
}

fn gitignore(line: &str) -> Result<Gitignore, String> {
    let mut builder = GitignoreBuilder::new(".");
    builder
        .case_insensitive(cfg!(windows))
        .map_err(|e| e.to_string())?;
    builder.add_line(None, line).map_err(|e| e.to_string())?;
    let matcher = builder.build().map_err(|e| e.to_string())?;
    if matcher.num_ignores() != 1 {
        return Err(
            "the pattern matches nothing (a comment or a `!` negation); escape a leading `#` or `!` with `\\`"
                .to_string(),
        );
    }
    Ok(matcher)
}

fn path_spec(spec: &str) -> Result<PathSpec, String> {
    let (anchor, line) = if let Some(rest) = spec.strip_prefix("//") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[1] == b':'
            && bytes[0].is_ascii_alphabetic()
            && matches!(bytes[2], b'/' | b'\\')
        {
            (
                Anchor::Fs(Some(
                    rest[..1].to_ascii_uppercase().chars().next().unwrap_or('C'),
                )),
                format!("/{}", &rest[3..]),
            )
        } else {
            (Anchor::Fs(None), format!("/{rest}"))
        }
    } else if let Some(rest) = spec.strip_prefix("~/") {
        (Anchor::Home, format!("/{rest}"))
    } else {
        (Anchor::Roots, spec.to_string())
    };
    Ok(PathSpec {
        anchor,
        matcher: gitignore(&line)?,
    })
}

fn drive_of(path: &Path) -> Option<char> {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some((letter as char).to_ascii_uppercase())
            }
            _ => None,
        },
        _ => None,
    }
}

fn strip_root(path: &Path, root: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        let mut p = path.components();
        for r in root.components() {
            let c = p.next()?;
            if !c
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&r.as_os_str().to_string_lossy())
            {
                return None;
            }
        }
        Some(p.as_path().to_path_buf())
    } else {
        path.strip_prefix(root).ok().map(Path::to_path_buf)
    }
}

impl PathSpec {
    fn hits(&self, path: &Path, roots: &[PathBuf], home: Option<&Path>) -> bool {
        let is_dir = path.is_dir();
        let test = |rel: &Path| {
            !rel.as_os_str().is_empty()
                && self
                    .matcher
                    .matched_path_or_any_parents(rel, is_dir)
                    .is_ignore()
        };
        match &self.anchor {
            Anchor::Roots => roots
                .iter()
                .filter_map(|root| strip_root(path, root))
                .any(|rel| test(&rel)),
            Anchor::Home => home
                .and_then(|home| strip_root(path, home))
                .is_some_and(|rel| test(&rel)),
            Anchor::Fs(drive) => {
                let Some(root) = path.ancestors().last() else {
                    return false;
                };
                if let Some(drive) = drive
                    && drive_of(path) != Some(*drive)
                {
                    return false;
                }
                strip_root(path, root).is_some_and(|rel| test(&rel))
            }
        }
    }
}

/// How a rule list is being used: deny and ask fail toward matching, allow toward not matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posture {
    Deny,
    Ask,
    Allow,
}

impl Posture {
    fn lenient(self) -> bool {
        self != Posture::Allow
    }
}

/// What rule matching needs to know about a call.
pub struct CallContext<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub scope: &'a Scope,
    pub home: Option<&'a Path>,
}

fn tool_matches(rule_tool: &str, name: &str) -> bool {
    if rule_tool == name {
        return true;
    }
    if !is_mcp(rule_tool) || !is_mcp(name) {
        return false;
    }
    let rule_server = &rule_tool[fontaine_tools::names::MCP_TOOL_PREFIX.len()..];
    let (server, tool) = match rule_server.split_once("__") {
        Some((server, tool)) => (server, Some(tool)),
        None => (rule_server, None),
    };
    let prefix = format!("{}{server}__", fontaine_tools::names::MCP_TOOL_PREFIX);
    match tool {
        None | Some("*") => name.starts_with(&prefix),
        Some(_) => false,
    }
}

/// Whether `rule` covers the call.
pub fn matches(rule: &Rule, call: &CallContext<'_>, posture: Posture) -> bool {
    if !tool_matches(&rule.tool, call.tool) {
        return false;
    }
    let Some(spec) = rule.specifier.as_deref() else {
        return true;
    };
    match call.tool {
        "shell" => shell_matches(spec, call, posture),
        "webfetch" => webfetch_matches(spec, call.args, posture),
        "agent" => string_arg(call.args, "agent_type").is_some_and(|name| name == spec),
        "skill" => string_arg(call.args, "name").is_some_and(|name| name == spec),
        tool if PATH_TOOLS.contains(&tool) => path_matches(spec, call, posture),
        _ => false,
    }
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn shell_matches(spec: &str, call: &CallContext<'_>, posture: Posture) -> bool {
    let Ok(spec) = shell_spec(spec) else {
        return false;
    };
    let Some(command) = string_arg(call.args, "command") else {
        // Nothing to compare: a deny/ask rule fails toward matching.
        return posture.lenient();
    };
    let analysis = analyze(command);
    if posture == Posture::Allow {
        return analysis.simple.as_ref().is_some_and(|argv| {
            if spec.wildcard {
                argv.starts_with(&spec.words)
            } else {
                *argv == spec.words
            }
        });
    }
    prefix_anywhere(&analysis, &spec.words) || cannot_tell(&analysis, command, &spec.words)
}

fn basename(word: &str) -> &str {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    name.strip_suffix(".exe").unwrap_or(name)
}

/// The rule's words appear contiguously in some command's word stream. A word the shell
/// expands never equals a rule word.
fn prefix_anywhere(analysis: &ShellAnalysis, words: &[String]) -> bool {
    analysis.commands.iter().any(|command| {
        let literal: Vec<Option<&str>> =
            command.words.iter().map(|w| w.literal.as_deref()).collect();
        literal.windows(words.len()).any(|window| {
            window
                .iter()
                .zip(words)
                .enumerate()
                .all(|(i, (have, want))| {
                    *have == Some(want.as_str())
                    // `/bin/rm` is `rm`; only a program name carries a directory.
                    || (i == 0 && have.is_some_and(|h| basename(h) == want))
                })
        })
    })
}

/// A command that cannot be read as a plain word stream (it failed to parse, hides something
/// from the walk, or has an expanded word where the command name goes) matches every rule
/// whose first word it contains as a whole word; an expanded command name matches every rule.
fn cannot_tell(analysis: &ShellAnalysis, command: &str, words: &[String]) -> bool {
    // An expanded command name could be any program, whatever the rule's words are.
    if analysis
        .commands
        .iter()
        .any(|c| c.words.first().is_some_and(|w| w.literal.is_none()))
    {
        return true;
    }
    (!analysis.parsed || analysis.unanalyzed)
        && words.first().is_some_and(|first| {
            let plain = first
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '+'));
            if plain {
                raw_mentions_word(command, &[first.as_str()])
            } else {
                command.contains(first.as_str())
            }
        })
}

fn webfetch_matches(spec: &str, args: &Value, posture: Posture) -> bool {
    let Ok(domain) = domain_spec(spec) else {
        return false;
    };
    let host = string_arg(args, "url")
        .and_then(|url| url::Url::parse(url).ok())
        .and_then(|url| url.host_str().and_then(normalize_host));
    let Some(host) = host else {
        return posture.lenient();
    };
    match domain {
        Domain::Exact(want) => host == want,
        Domain::Subdomains(base) => {
            host.len() > base.len() + 1 && host.ends_with(&format!(".{base}"))
        }
    }
}

/// The path arguments of a file-tool call: the path as written and as the tool would open it,
/// plus whether the tool works on a directory tree.
struct PathArgs {
    written: PathBuf,
    resolved: PathBuf,
    tree: bool,
}

fn path_args(call: &CallContext<'_>) -> Option<PathArgs> {
    let scope = call.scope;
    let from = |path: &str, tree: bool| PathArgs {
        written: scope.written(path),
        resolved: scope.resolved(path),
        tree,
    };
    match call.tool {
        "read" | "write" | "edit" => Some(from(string_arg(call.args, "path")?, false)),
        "ls" | "grep" => Some(from(string_arg(call.args, "path").unwrap_or("."), true)),
        "glob" => {
            let pattern = string_arg(call.args, "pattern")?;
            let (base, _) = glob_target(string_arg(call.args, "path"), pattern);
            Some(from(&base, true))
        }
        _ => None,
    }
}

fn path_matches(spec: &str, call: &CallContext<'_>, posture: Posture) -> bool {
    let Ok(spec) = path_spec(spec) else {
        return false;
    };
    let Some(paths) = path_args(call) else {
        return posture.lenient();
    };
    let roots = call.scope.roots();
    let test = |path: &Path| {
        spec.hits(path, roots, call.home)
            // A rule for `secrets/**` covers searching `secrets` itself.
            || (paths.tree && spec.hits(&path.join("x"), roots, call.home))
    };
    match posture {
        Posture::Allow => test(&paths.resolved),
        _ => test(&paths.written) || test(&paths.resolved),
    }
}

/// The rule that lets this one call run again and nothing wider, or `None` when no rule can
/// say it (a shell command that is not one literal simple command).
pub fn propose(call: &CallContext<'_>) -> Option<Rule> {
    let spec = match call.tool {
        "shell" => {
            let argv = analyze(string_arg(call.args, "command")?).simple?;
            Some(
                argv.iter()
                    .map(|word| shell_quote(word))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        "webfetch" => {
            let url = url::Url::parse(string_arg(call.args, "url")?).ok()?;
            Some(format!("domain:{}", url.host_str()?.to_ascii_lowercase()))
        }
        "agent" => Some(string_arg(call.args, "agent_type")?.to_string()),
        "skill" => Some(string_arg(call.args, "name")?.to_string()),
        tool if PATH_TOOLS.contains(&tool) => Some(path_pattern(&path_args(call)?.resolved)),
        tool if is_mcp(tool) || tool == "send_message" => None,
        _ => return None,
    };
    let rule = Rule {
        tool: call.tool.to_string(),
        specifier: spec,
    };
    // Whatever is shown must parse back and cover the call it was proposed for.
    let parsed = parse_rule(&rule.to_string()).ok()?;
    matches(&parsed, call, Posture::Allow).then_some(parsed)
}

/// An absolute path as a pattern that matches exactly it: `//` for the file system root and
/// gitignore escapes for characters that would otherwise be wildcards.
fn path_pattern(path: &Path) -> String {
    let text = if cfg!(windows) {
        path.to_string_lossy().replace('\\', "/")
    } else {
        path.to_string_lossy().into_owned()
    };
    // Windows canonical paths are verbatim (`\\?\C:\...`).
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let body = text.strip_prefix('/').unwrap_or(text);
    let mut out = String::from("//");
    for c in body.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    let trailing = out.len() - out.trim_end_matches(' ').len();
    out.truncate(out.len() - trailing);
    for _ in 0..trailing {
        out.push_str("\\ ");
    }
    out
}

/// A word quoted so the shell reads it back as the same single word.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | ',' | ':' | '@' | '%' | '+')
        });
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Entries whose bare form, or form with more arguments (`<entry> *`), runs arbitrary code.
/// `auto` suspends allow rules of this shape for as long as it is on; narrower rules such as
/// `shell(npm test)` stay.
pub const BROAD_CODE_EXECUTION: [&str; 21] = [
    "sh",
    "bash",
    "zsh",
    "python",
    "python3",
    "node",
    "ruby",
    "perl",
    "deno",
    "bun",
    "cargo run",
    "npm run",
    "npx",
    "pnpm",
    "yarn",
    "make",
    "eval",
    "exec",
    "xargs",
    "env",
    "sudo",
];

/// Whether an allow rule is one `auto` suspends.
pub fn is_broad_code_execution(rule: &Rule) -> bool {
    match (rule.tool.as_str(), rule.specifier.as_deref()) {
        ("shell" | "agent", None) => true,
        ("shell", Some(spec)) => {
            let Ok(spec) = shell_spec(spec) else {
                return false;
            };
            BROAD_CODE_EXECUTION
                .iter()
                .any(|entry| entry.split(' ').eq(spec.words.iter().map(String::as_str)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Rule {
        parse_rule(text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn denied(rule: &str, tool: &str, args: Value) -> bool {
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let call = CallContext {
            tool,
            args: &args,
            scope: &scope,
            home: None,
        };
        matches(&ok(rule), &call, Posture::Deny)
    }

    #[test]
    fn quoted_prose_is_not_a_command() {
        let shell = |rule: &str, command: &str| {
            denied(rule, "shell", serde_json::json!({ "command": command }))
        };
        assert!(!shell(
            "shell(git push)",
            "git commit -m \"Fix user's bug\""
        ));
        assert!(!shell("shell(rm)", "git commit -m \"it's a format fix\""));
        assert!(!shell("shell(git push)", "git commit -m 'fix git push'"));
        assert!(shell("shell(git push)", "git push origin"));
        assert!(shell("shell(rm)", "sh -c 'rm x'"));
        assert!(shell("shell(rm)", "echo 'unterminated; rm x"));
    }

    #[test]
    fn a_full_path_program_matches_deny_but_not_allow() {
        let cmd = |c: &str| serde_json::json!({ "command": c });
        assert!(denied("shell(rm)", "shell", cmd("/bin/rm -rf build")));
        assert!(denied(
            "shell(git push)",
            "shell",
            cmd("/usr/bin/git push -f")
        ));
        assert!(!denied(
            "shell(git push)",
            "shell",
            cmd("/usr/bin/git status")
        ));
        let scope = Scope::new(PathBuf::from("/home/me/proj"), []);
        let args = cmd("/tmp/evil/git status");
        let call = CallContext {
            tool: "shell",
            args: &args,
            scope: &scope,
            home: None,
        };
        assert!(!matches(&ok("shell(git status)"), &call, Posture::Allow));
    }

    #[test]
    fn an_expanded_command_name_matches_every_rule() {
        let shell = |rule: &str, command: &str| {
            denied(rule, "shell", serde_json::json!({ "command": command }))
        };
        assert!(shell("shell(rm)", "X=r; ${X}m -rf /"));
        assert!(shell("shell(rm)", "$(printf r)m -rf ~"));
        assert!(!shell("shell(rm)", "ls $HOME"));
        assert!(!shell("shell(rm)", "echo $(date)"));
    }

    #[test]
    fn webfetch_hosts_are_normalized() {
        let fetch =
            |rule: &str, url: &str| denied(rule, "webfetch", serde_json::json!({ "url": url }));
        assert!(fetch(
            "webfetch(domain:example.com)",
            "https://example.com./secret"
        ));
        assert!(fetch(
            "webfetch(domain:example.com.)",
            "https://example.com/secret"
        ));
        assert!(fetch(
            "webfetch(domain:*.example.com)",
            "https://a.example.com./x"
        ));
        assert!(fetch(
            "webfetch(domain:bücher.de)",
            "https://xn--bcher-kva.de/x"
        ));
        assert!(fetch(
            "webfetch(domain:Example.COM)",
            "https://EXAMPLE.com/x"
        ));
        assert!(!fetch(
            "webfetch(domain:example.com)",
            "https://example.org/"
        ));
    }

    #[test]
    fn bare_and_specifier_forms() {
        assert_eq!(
            ok("shell"),
            Rule {
                tool: "shell".into(),
                specifier: None
            }
        );
        assert_eq!(
            ok("shell(git push *)").specifier.as_deref(),
            Some("git push *")
        );
        assert_eq!(
            ok("read(~/.aws/**)").specifier.as_deref(),
            Some("~/.aws/**")
        );
        assert_eq!(
            ok("shell(echo '(nested)')").specifier.as_deref(),
            Some("echo '(nested)'")
        );
        assert_eq!(ok("shell(rm)").to_string(), "shell(rm)");
    }

    #[test]
    fn mcp_names() {
        for rule in ["mcp__github", "mcp__github__create_issue", "mcp__github__*"] {
            ok(rule);
        }
        for rule in [
            "mcp__",
            "mcp____tool",
            "mcp__github__",
            "mcp__git*__x",
            "mcp__github__cre*",
            "mcp__github(x)",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule}");
        }
    }

    #[test]
    fn webfetch_domain_form() {
        ok("webfetch");
        ok("webfetch(domain:example.com)");
        ok("webfetch(domain:*.example.com)");
        for rule in [
            "webfetch(example.com)",
            "webfetch(domain:)",
            "webfetch(domain:*.)",
            "webfetch(domain:http://example.com)",
            "webfetch(domain:a.com/path)",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule}");
        }
    }

    #[test]
    fn rejects_bad_shapes_and_unknown_tools() {
        for rule in [
            "", "(x)", "shell(", "shell()", "shell( )", "shell(x", "Shell", "bash", "shell x",
            "shell)(",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule:?}");
        }
    }

    #[test]
    fn specifiers_are_checked_per_tool() {
        for rule in [
            "shell(*)",
            "shell(rm *.o)",
            "shell(a; b)",
            "shell(a | b)",
            "shell($HOME/bin)",
            "read(#comment)",
            "read(!negated)",
            "send_message(x)",
            "agent(two words)",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule}");
        }
        for rule in [
            "shell(npm run test *)",
            "shell(rm '*.o')",
            "read(src/**)",
            "read(//etc/hosts)",
            "read(\\#literal)",
            "edit(~/notes/*.md)",
            "agent(worker)",
            "skill(review)",
            "send_message",
        ] {
            ok(rule);
        }
    }

    #[test]
    fn broad_code_execution_rules_are_recognized() {
        for rule in [
            "shell",
            "agent",
            "shell(bash)",
            "shell(python *)",
            "shell(cargo run *)",
            "shell(sudo)",
            "shell('bash' *)",
            "shell(\"python\")",
        ] {
            assert!(is_broad_code_execution(&ok(rule)), "{rule}");
        }
        for rule in [
            "shell(npm test)",
            "shell(python script.py)",
            "shell(cargo build)",
            "read",
            "shell(cargo run --release)",
        ] {
            assert!(!is_broad_code_execution(&ok(rule)), "{rule}");
        }
    }

    #[test]
    fn quoting_round_trips_through_the_shell_parser() {
        for word in [
            "plain", "a b", "it's", "*.rs", "$HOME", "", "a;b", "x=y", "~", "{a,b}",
        ] {
            let quoted = shell_quote(word);
            assert_eq!(
                analyze(&quoted).simple,
                Some(vec![word.to_string()]),
                "{word} -> {quoted}"
            );
        }
    }
}
