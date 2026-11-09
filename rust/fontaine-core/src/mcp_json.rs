//! The `.mcp.json` format Claude Code reads: one reader for a plugin's file and a repository's.
//!
//! A plugin's references are expanded when the file is read. A repository's stay raw in the
//! parsed server and are expanded when the server starts, so what the user approves is the text
//! as written and a rotated token does not invalidate the approval.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;

use serde_json::Value;

use crate::config::McpServer;

/// The rule for a server name, as an error would say it.
pub const NAME_RULE: &str = "it must be non-empty, contain no `__`, neither start nor end with `_`, and use only letters, digits, `_` and `-`";

pub fn valid_server_name(name: &str) -> bool {
    !(name.is_empty()
        || name.contains("__")
        || name.starts_with('_')
        || name.ends_with('_')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
}

/// When a file's `${...}` references are replaced.
pub enum Expansion<'a> {
    /// Now, with this expander.
    Now(&'a mut dyn FnMut(&str) -> String),
    /// When the server starts; the parsed text is raw and the server is marked `expand`.
    AtStart,
}

/// What one `.mcp.json` held.
#[derive(Debug, Default)]
pub struct ParsedFile {
    /// Bare server names, in file order.
    pub servers: Vec<(String, McpServer)>,
    /// Entries that cannot be used, each as a sentence naming the entry.
    pub problems: Vec<String>,
    /// Entries that load with a caveat, or are skipped on purpose.
    pub warnings: Vec<String>,
}

/// Reads a `.mcp.json` document. `Err` is a file that is not JSON at all; a bad entry is a
/// problem, so the caller decides whether it is fatal.
pub fn parse(text: &str, mut expansion: Expansion<'_>) -> Result<ParsedFile, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let mut out = ParsedFile::default();
    let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
        return Ok(out);
    };
    for (name, spec) in servers {
        if !valid_server_name(name) {
            out.problems
                .push(format!("`{name}` is not an MCP server name: {NAME_RULE}"));
            continue;
        }
        match parse_entry(name, spec, &mut expansion, &mut out.warnings) {
            Ok(Some(server)) => out.servers.push((name.clone(), server)),
            Ok(None) => {}
            Err(problem) => out.problems.push(problem),
        }
    }
    Ok(out)
}

fn parse_entry(
    name: &str,
    spec: &Value,
    expansion: &mut Expansion<'_>,
    warnings: &mut Vec<String>,
) -> Result<Option<McpServer>, String> {
    let object = spec
        .as_object()
        .ok_or_else(|| format!("the `.mcp.json` entry `{name}` is not an object"))?;
    let kind =
        match object.get("type") {
            None => None,
            Some(value) => Some(value.as_str().ok_or_else(|| {
                format!("the `.mcp.json` entry `{name}` has a non-string `type`")
            })?),
        };
    match kind {
        None | Some("stdio") | Some("http") => {}
        Some(other @ ("sse" | "ws")) => {
            warnings.push(format!(
                "MCP server `{name}` uses the `{other}` transport, which Fontaine does not support yet (#36); it is skipped"
            ));
            return Ok(None);
        }
        Some(other) => {
            warnings.push(format!(
                "MCP server `{name}` has an unknown transport `type` `{other}`; it is skipped"
            ));
            return Ok(None);
        }
    }
    if object.contains_key("headers") {
        warnings.push(format!(
            "MCP server `{name}` sets `headers`, which Fontaine does not support yet (#32); it connects without them"
        ));
    }
    let (at_start, mut expand) = match expansion {
        Expansion::Now(f) => (false, Some(f)),
        Expansion::AtStart => (true, None),
    };
    let mut expand_text = |text: &str| match expand.as_mut() {
        Some(f) => f(text),
        None => text.to_string(),
    };

    let env = match object.get("env") {
        None => None,
        Some(value) => {
            let map = value.as_object().ok_or_else(|| {
                format!("the `.mcp.json` entry `{name}` has an `env` that is not an object")
            })?;
            let mut out = BTreeMap::new();
            for (var, spec) in map {
                let text = spec.as_str().ok_or_else(|| {
                    format!(
                        "the `.mcp.json` entry `{name}` has a non-string `env` value for `{var}`"
                    )
                })?;
                out.insert(var.clone(), expand_text(text));
            }
            Some(out)
        }
    };
    let url = match object.get("url") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("the `.mcp.json` entry `{name}` has a non-string `url`"))?;
            Some(expand_text(text))
        }
    };
    let command = match object.get("command") {
        None => None,
        Some(value) => {
            let text = value.as_str().ok_or_else(|| {
                format!("the `.mcp.json` entry `{name}` has a non-string `command`")
            })?;
            Some(expand_text(text))
        }
    };
    let args = if command.is_some() {
        match object.get("args") {
            None => None,
            Some(value) => {
                let items = value.as_array().ok_or_else(|| {
                    format!("the `.mcp.json` entry `{name}` has an `args` that is not a list")
                })?;
                let mut out = Vec::new();
                for item in items {
                    let text = item.as_str().ok_or_else(|| {
                        format!("a `.mcp.json` `args` entry of `{name}` is not a string")
                    })?;
                    out.push(expand_text(text));
                }
                Some(out)
            }
        }
    } else {
        None
    };
    if kind == Some("stdio") && command.is_none() {
        return Err(format!(
            "the `.mcp.json` entry `{name}` has `type` `stdio` but no `command`"
        ));
    }
    if kind == Some("http") && url.is_none() {
        return Err(format!(
            "the `.mcp.json` entry `{name}` has `type` `http` but no `url`"
        ));
    }
    match (command, url) {
        (Some(command), None) => Ok(Some(McpServer {
            command: Some(command),
            args,
            env_vars: None,
            url: None,
            env,
            expand: at_start,
        })),
        (None, Some(_)) if env.is_some() || object.contains_key("args") => Err(format!(
            "the `.mcp.json` entry `{name}` sets `args` or `env`, which apply to stdio servers only"
        )),
        (None, Some(url)) => Ok(Some(McpServer {
            command: None,
            args: None,
            env_vars: None,
            url: Some(url),
            env: None,
            expand: at_start,
        })),
        (Some(_), Some(_)) => Err(format!(
            "the `.mcp.json` entry `{name}` has both `command` and `url`; set exactly one"
        )),
        (None, None) => Err(format!(
            "the `.mcp.json` entry `{name}` has neither `command` nor `url`"
        )),
    }
}

/// A piece of text: literal, or a `${NAME}` / `${NAME:-default}` reference.
enum Piece<'a> {
    Literal(&'a str),
    Reference {
        name: &'a str,
        default: Option<&'a str>,
        raw: &'a str,
    },
}

fn scan(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut literal_from = 0;
    let mut at = 0;
    while at < text.len() {
        if text[at..].starts_with("${")
            && let Some(end) = text[at + 2..].find('}')
        {
            if literal_from < at {
                pieces.push(Piece::Literal(&text[literal_from..at]));
            }
            let inner = &text[at + 2..at + 2 + end];
            let (name, default) = match inner.split_once(":-") {
                Some((name, default)) => (name, Some(default)),
                None => (inner, None),
            };
            let stop = at + 2 + end + 1;
            pieces.push(Piece::Reference {
                name,
                default,
                raw: &text[at..stop],
            });
            at = stop;
            literal_from = stop;
            continue;
        }
        at += text[at..]
            .chars()
            .next()
            .expect("at is a char boundary")
            .len_utf8();
    }
    if literal_from < text.len() {
        pieces.push(Piece::Literal(&text[literal_from..]));
    }
    pieces
}

/// The variable names `text` references, in order of appearance, once each.
pub fn references(text: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for piece in scan(text) {
        if let Piece::Reference { name, .. } = piece
            && !seen.iter().any(|s| s == name)
        {
            seen.push(name.to_string());
        }
    }
    seen
}

/// Expands `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PROJECT_DIR}`, `${VAR}` and `${VAR:-default}` from
/// the environment; an unset `${VAR}` with no default stays literal and warns once.
pub(crate) fn expand_vars(
    text: &str,
    plugin: &str,
    plugin_root: &Path,
    ctx: &crate::plugins::PluginContext<'_>,
    warnings: &mut Vec<String>,
    env_vars: &mut BTreeSet<String>,
) -> String {
    let root = plugin_root.display().to_string();
    let project = ctx.project_root.display().to_string();
    let mut out = String::new();
    for piece in scan(text) {
        match piece {
            Piece::Literal(literal) => out.push_str(literal),
            Piece::Reference { name, default, raw } => match name {
                "CLAUDE_PLUGIN_ROOT" => out.push_str(&root),
                "CLAUDE_PROJECT_DIR" => out.push_str(&project),
                _ => {
                    env_vars.insert(name.to_string());
                    match ((ctx.env)(name), default) {
                        (Some(value), _) => out.push_str(&value.to_string_lossy()),
                        (None, Some(default)) => out.push_str(default),
                        (None, None) => {
                            out.push_str(raw);
                            let message =
                                format!("plugin `{plugin}` leaves `${{{name}}}` unexpanded");
                            if !warnings.contains(&message) {
                                warnings.push(message);
                            }
                        }
                    }
                }
            },
        }
    }
    out
}

/// What a repository's definition may expand when its server starts.
pub struct StartContext<'a> {
    /// What `${CLAUDE_PROJECT_DIR}` is: the session's root, never the process environment.
    pub project_dir: &'a Path,
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// Whether a variable holds a credential, which a repository's text never expands.
    pub is_credential: &'a dyn Fn(&str) -> bool,
}

/// Expands a repository definition's `${VAR}` and `${VAR:-default}` references. A credential
/// variable, or one that is unset with no default, is an error naming it.
pub fn expand_at_start(text: &str, cx: &StartContext<'_>) -> Result<String, String> {
    let mut out = String::new();
    for piece in scan(text) {
        match piece {
            Piece::Literal(literal) => out.push_str(literal),
            Piece::Reference { name, default, .. } => {
                if name == "CLAUDE_PROJECT_DIR" {
                    out.push_str(&cx.project_dir.display().to_string());
                } else if (cx.is_credential)(name) {
                    return Err(format!(
                        "`${{{name}}}` names a credential variable, which a repository's MCP server definition never expands"
                    ));
                } else {
                    match ((cx.env)(name), default) {
                        (Some(value), _) => out.push_str(&value.to_string_lossy()),
                        (None, Some(default)) => out.push_str(default),
                        (None, None) => {
                            return Err(format!(
                                "`${{{name}}}` is not set and has no default (use `${{{name}:-default}}`)"
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginContext;

    fn at_start(text: &str) -> ParsedFile {
        parse(text, Expansion::AtStart).expect("valid JSON")
    }

    #[test]
    fn type_stdio_and_http_load_and_sse_and_ws_are_skipped_with_a_warning() {
        let parsed = at_start(
            r#"{"mcpServers":{
                "a":{"type":"stdio","command":"x"},
                "b":{"type":"http","url":"https://h/mcp"},
                "c":{"url":"https://h/mcp"},
                "d":{"type":"sse","url":"https://h/sse"},
                "e":{"type":"ws","url":"wss://h"}
            }}"#,
        );
        let names: Vec<&str> = parsed.servers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert!(parsed.problems.is_empty());
        assert_eq!(parsed.warnings.len(), 2);
        assert!(parsed.warnings[0].contains("`sse`") && parsed.warnings[0].contains("#36"));
        assert!(parsed.warnings[1].contains("`ws`"));
    }

    #[test]
    fn headers_warn_and_keep_the_server() {
        let parsed = at_start(
            r#"{"mcpServers":{"h":{"type":"http","url":"https://h","headers":{"A":"b"}}}}"#,
        );
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings[0].contains("#32"));
    }

    #[test]
    fn at_start_keeps_the_text_raw_and_marks_the_server() {
        let parsed = at_start(
            r#"{"mcpServers":{"s":{"command":"${BIN:-x}","args":["${A}"],"env":{"K":"${V}"}}}}"#,
        );
        let server = &parsed.servers[0].1;
        assert!(server.expand);
        assert_eq!(server.command.as_deref(), Some("${BIN:-x}"));
        assert_eq!(server.args, Some(vec!["${A}".to_string()]));
        assert_eq!(server.env.as_ref().unwrap()["K"], "${V}");
    }

    #[test]
    fn now_expansion_uses_the_default_only_when_the_variable_is_unset() {
        let env = |name: &str| (name == "SET").then(|| OsString::from("real"));
        let ctx = PluginContext {
            project_root: Path::new("/work"),
            env: &env,
        };
        let mut warnings = Vec::new();
        let mut env_vars = BTreeSet::new();
        let out = expand_vars(
            "${SET:-d} ${UNSET:-d} ${UNSET2:-} ${NONE}",
            "p",
            Path::new("/p"),
            &ctx,
            &mut warnings,
            &mut env_vars,
        );
        assert_eq!(out, "real d  ${NONE}");
        assert_eq!(warnings, vec!["plugin `p` leaves `${NONE}` unexpanded"]);
    }

    #[test]
    fn a_plugin_style_parse_expands_now_and_does_not_mark_the_server() {
        let mut expander = |text: &str| text.replace("${X}", "1");
        let parsed = parse(
            r#"{"mcpServers":{"s":{"command":"c${X}"}}}"#,
            Expansion::Now(&mut expander),
        )
        .unwrap();
        let server = &parsed.servers[0].1;
        assert_eq!(server.command.as_deref(), Some("c1"));
        assert!(!server.expand);
    }

    #[test]
    fn bad_entries_are_problems_and_the_good_ones_still_load() {
        let parsed = at_start(
            r#"{"mcpServers":{"bad__name":{"command":"x"},"both":{"command":"x","url":"u"},"ok":{"command":"x"}}}"#,
        );
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.problems.len(), 2);
        assert!(parsed.problems[0].contains("bad__name"));
        assert!(parsed.problems[1].contains("both `command` and `url`"));
        assert!(parse("{nope", Expansion::AtStart).is_err());
    }

    #[test]
    fn references_lists_each_name_once() {
        assert_eq!(references("a ${X} ${Y:-z} ${X}"), ["X", "Y"]);
        assert!(references("plain $X").is_empty());
    }

    #[test]
    fn start_expansion_covers_defaults_unset_and_credentials() {
        let env = |name: &str| match name {
            "SET" => Some(OsString::from("v")),
            "CLAUDE_PROJECT_DIR" => Some(OsString::from("/elsewhere")),
            _ => None,
        };
        let credential = |name: &str| name == "KEY";
        let cx = StartContext {
            project_dir: Path::new("/root"),
            env: &env,
            is_credential: &credential,
        };
        assert_eq!(
            expand_at_start("${SET}-${NO:-d}-${CLAUDE_PROJECT_DIR}", &cx).unwrap(),
            "v-d-/root"
        );
        assert!(expand_at_start("${NO}", &cx).unwrap_err().contains("${NO}"));
        assert!(
            expand_at_start("u/${KEY:-x}", &cx)
                .unwrap_err()
                .contains("credential")
        );
    }
}
