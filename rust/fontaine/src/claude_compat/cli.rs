//! The command line of Claude SDK mode: Claude Code's flag spellings, the capability matrix that
//! drives both this parser and the control router, and the mapping onto Fontaine's own flags.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

use fontaine_core::config::{McpServer, PermissionMode};

/// The Claude Code version whose protocol the conformance suite was captured against. `init`
/// reports it so the SDKs' feature gates read the protocol level served.
pub const CLAUDE_CODE_PROTOCOL_VERSION: &str = "2.1.325";
#[cfg(test)]
pub const TESTED_TS_SDK: &str = "0.3.325";
#[cfg(test)]
pub const TESTED_PY_SDK: &str = "0.2.182";

/// The `compat_protocol` value of `init`.
pub const COMPAT_PROTOCOL: &str = "claude-sdk-1";

/// The page every refusal points to.
const DOCS: &str = "docs/agent-sdk.md";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    /// Takes no value.
    Flag,
    /// `--flag value` or `--flag=value`.
    Value,
    /// `--flag=value`, or `--flag value` when the next token is not a flag.
    Optional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Supported,
    Mapped(&'static str),
    Unsupported(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Flag,
    Control,
    Message,
}

#[cfg(test)]
impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Flag => "flag",
            Kind::Control => "control request",
            Kind::Message => "message field",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub kind: Kind,
    /// The first spelling is the one the docs show; the others are accepted aliases.
    pub names: &'static [&'static str],
    pub arity: Arity,
    pub state: State,
}

const fn flag(names: &'static [&'static str], arity: Arity, state: State) -> Row {
    Row {
        kind: Kind::Flag,
        names,
        arity,
        state,
    }
}

const fn control(name: &'static [&'static str], state: State) -> Row {
    Row {
        kind: Kind::Control,
        names: name,
        arity: Arity::Flag,
        state,
    }
}

const fn field(name: &'static [&'static str], state: State) -> Row {
    Row {
        kind: Kind::Message,
        names: name,
        arity: Arity::Flag,
        state,
    }
}

use Arity::{Flag as F, Optional as O, Value as V};
use State::{Mapped, Supported, Unsupported};

/// Every SDK flag, control subtype and message field group, with what Fontaine does with it.
/// The parser and the control router read this table, and a test renders it into the docs.
pub const MATRIX: &[Row] = &[
    flag(&["--output-format"], V, Mapped("must be `stream-json`")),
    flag(&["--input-format"], V, Mapped("must be `stream-json`")),
    flag(&["--verbose"], F, Supported),
    flag(
        &["--system-prompt"],
        V,
        Mapped("an empty value keeps Fontaine's base prompt; a non-empty one replaces it"),
    ),
    flag(
        &["--system-prompt-file"],
        V,
        Mapped("read as `--system-prompt`"),
    ),
    flag(&["--append-system-prompt"], V, Supported),
    flag(
        &["--tools"],
        V,
        Mapped("the base set of builtin tools; entries with a specifier are refused"),
    ),
    flag(
        &["--allowedTools", "--allowed-tools"],
        V,
        Mapped("each entry is a CLI-scope allow rule; it sits below the floor"),
    ),
    flag(&["--disallowedTools", "--disallowed-tools"], V, Supported),
    flag(&["--max-turns"], V, Supported),
    flag(&["--max-budget-usd"], V, Supported),
    flag(
        &["--model"],
        V,
        Mapped("a Fontaine model reference or role, not a Claude alias"),
    ),
    flag(
        &["--permission-mode"],
        V,
        Mapped("`default` is `manual`; `bypassPermissions` does not exist; `afk` is Fontaine's"),
    ),
    flag(
        &["--permission-prompt-tool"],
        V,
        Mapped("only `stdio`, the `canUseTool` callback; a named MCP tool is refused"),
    ),
    flag(&["--continue", "-c"], F, Supported),
    flag(&["--resume", "-r"], O, Supported),
    flag(&["--session-id"], V, Supported),
    flag(&["--fork-session"], F, Supported),
    flag(
        &["--settings"],
        V,
        Mapped("only an empty object: Claude's settings schema is not Fontaine's"),
    ),
    flag(&["--add-dir"], V, Supported),
    flag(
        &["--mcp-config"],
        V,
        Mapped("`stdio`, `http` and `sdk` servers; `sse` is refused"),
    ),
    flag(&["--strict-mcp-config"], F, Supported),
    flag(&["--include-partial-messages"], F, Supported),
    flag(
        &["--setting-sources"],
        V,
        Mapped("accepted, but never drops Fontaine's project-scope tightening"),
    ),
    flag(&["--effort"], V, Supported),
    flag(&["--json-schema"], V, Supported),
    flag(
        &["--fallback-model"],
        V,
        Unsupported("Fontaine falls through model lists configured in its own settings"),
    ),
    flag(
        &["--betas"],
        V,
        Unsupported("Anthropic beta headers are not Fontaine's to set"),
    ),
    flag(
        &["--thinking"],
        V,
        Unsupported("reasoning is set with `--effort`"),
    ),
    flag(
        &["--max-thinking-tokens"],
        V,
        Unsupported("reasoning is set with `--effort`"),
    ),
    flag(
        &["--thinking-display"],
        V,
        Unsupported("reasoning is not streamed"),
    ),
    flag(&["--task-budget"], V, Unsupported("use `--max-budget-usd`")),
    flag(
        &["--plugin-dir", "--plugin-dir-no-mcp"],
        V,
        Unsupported("Fontaine installs plugins with `fontaine plugin`"),
    ),
    flag(
        &["--resume-session-at"],
        V,
        Unsupported("sessions resume whole"),
    ),
    flag(
        &["--resume-drops-turn"],
        V,
        Unsupported("sessions resume whole"),
    ),
    flag(
        &["--session-mirror"],
        F,
        Unsupported("Fontaine keeps its own session store"),
    ),
    flag(
        &["--await-initialize"],
        F,
        Unsupported("not needed: `initialize` is optional"),
    ),
    flag(&["--channels"], V, Unsupported("no channels")),
    flag(&["--sdk-url"], V, Unsupported("no remote transport")),
    flag(
        &["--include-hook-events"],
        F,
        Unsupported("hook events are not streamed"),
    ),
    flag(
        &["--no-session-persistence"],
        F,
        Unsupported("every session is recorded in the ledger"),
    ),
    flag(
        &["--permission-prompts"],
        V,
        Unsupported("no prompt routing"),
    ),
    flag(&["--agent"], V, Unsupported("no agent selection")),
    flag(&["--replay-user-messages"], F, Unsupported("not echoed")),
    flag(
        &["--allow-dangerously-skip-permissions"],
        F,
        Unsupported("there is no bypass mode"),
    ),
    flag(
        &["--dangerously-skip-permissions"],
        F,
        Unsupported("there is no bypass mode"),
    ),
    flag(
        &["--debug", "--debug-to-stderr"],
        O,
        Unsupported("use `--debug-file` on a plain run"),
    ),
    flag(
        &["--debug-file"],
        V,
        Unsupported("use the debug environment variable of a plain run"),
    ),
    flag(&["--hard-fail"], F, Unsupported("no equivalent")),
    flag(
        &["--managed-settings", "--managed-settings-in-initialize"],
        V,
        Unsupported("no managed settings"),
    ),
    flag(&["--project-config-root"], V, Unsupported("no equivalent")),
    flag(&["--exec-path"], V, Unsupported("no equivalent")),
    control(
        &["initialize"],
        Mapped(
            "hooks, agents, system prompts, schema and SDK servers; see the unsupported fields below",
        ),
    ),
    control(&["interrupt"], Supported),
    control(
        &["set_permission_mode"],
        Mapped("never `bypassPermissions`, never `afk`"),
    ),
    control(
        &["set_model"],
        Mapped("a Fontaine model reference or role; applied when no turn is running"),
    ),
    control(&["mcp_status"], Supported),
    control(&["get_context_usage"], Unsupported("no equivalent report")),
    control(
        &["rewind_files"],
        Unsupported("Fontaine rewinds from its terminal UI"),
    ),
    control(
        &["mcp_reconnect"],
        Unsupported("restart a server from the terminal UI"),
    ),
    control(&["mcp_toggle"], Unsupported("no equivalent")),
    control(
        &["stop_task"],
        Unsupported("no `task_*` messages are streamed"),
    ),
    control(
        &["update_environment_variables"],
        Unsupported("the environment is fixed at start"),
    ),
    field(
        &["can_use_tool.blocked_path"],
        Unsupported("the pipeline has no source for it"),
    ),
    field(
        &["can_use_tool.title", "can_use_tool.display_name"],
        Unsupported("the pipeline has no source for them"),
    ),
    field(
        &["system/task_*"],
        Unsupported(
            "subagent messages are not streamed; the `Agent` tool call and result stand for them",
        ),
    ),
    field(
        &["parent_tool_use_id"],
        Mapped("always null: only the root conversation is streamed"),
    ),
    field(
        &["hook PreToolUse allow"],
        Mapped(
            "ignored: a host hook can deny, ask and rewrite the input, never skip the mode, `canUseTool` or the guardian",
        ),
    ),
];

/// The matrix as the table the docs carry.
#[cfg(test)]
pub fn render_matrix() -> String {
    let mut out = String::from("| Item | Kind | State | Note |\n|---|---|---|---|\n");
    for row in MATRIX {
        let (state, note) = match row.state {
            State::Supported => ("supported", ""),
            State::Mapped(note) => ("mapped", note),
            State::Unsupported(note) => ("unsupported", note),
        };
        out.push_str(&format!(
            "| `{}` | {} | {state} | {note} |\n",
            row.names.join("`, `"),
            row.kind.label()
        ));
    }
    out
}

/// The row of a control subtype, which the router reads.
pub fn control_row(subtype: &str) -> Option<&'static Row> {
    MATRIX
        .iter()
        .find(|row| row.kind == Kind::Control && row.names.contains(&subtype))
}

/// A refusal that ends the process with exit code 2 before anything runs.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (see {DOCS})", self.0)
    }
}

fn refuse(message: impl Into<String>) -> Refusal {
    Refusal(message.into())
}

fn flag_row(name: &str) -> Option<&'static Row> {
    MATRIX
        .iter()
        .find(|row| row.kind == Kind::Flag && row.names.contains(&name))
}

/// Flags that take a value, for the scan that tells whether argv is an SDK invocation: the
/// operand of one is never read as a flag.
fn takes_operand(name: &str) -> bool {
    flag_row(name).is_some_and(|row| row.arity == Arity::Value)
}

/// Whether the command line is the shape every SDK sends: stream-json in and out, `--verbose` and
/// no `-p`. Fontaine's plain flags only act under `-p`, so no other invocation has this shape.
pub fn is_sdk_invocation(args: &[OsString]) -> bool {
    let (mut output, mut input, mut verbose, mut print) = (false, false, false, false);
    let mut at = 1;
    while at < args.len() {
        let token = args[at].to_string_lossy();
        at += 1;
        let (name, inline) = match token.split_once('=') {
            Some((name, value)) if token.starts_with("--") => (name, Some(value.to_string())),
            _ => (token.as_ref(), None),
        };
        let mut operand = || match &inline {
            Some(value) => Some(value.clone()),
            None if takes_operand(name) => {
                let value = args.get(at).map(|v| v.to_string_lossy().into_owned());
                at += 1;
                value
            }
            None => None,
        };
        match name {
            "-p" | "--print" => print = true,
            "--verbose" => verbose = true,
            "--output-format" => output |= operand().as_deref() == Some("stream-json"),
            "--input-format" => input |= operand().as_deref() == Some("stream-json"),
            _ => {
                operand();
            }
        }
    }
    output && input && verbose && !print
}

/// What the flags ask for, beyond the arguments Fontaine's own parser reads.
#[derive(Debug, Default)]
pub struct Options {
    /// The arguments for Fontaine's own command line, program name first.
    pub fontaine_args: Vec<OsString>,
    /// `--permission-prompt-tool stdio`: a `canUseTool` callback answers prompts.
    pub permission_prompt: bool,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    /// The builtin tools the root agent has, as Fontaine names; `None` is all of them.
    pub root_tools: Option<Vec<String>>,
    /// `stdio` and `http` servers declared by `--mcp-config`.
    pub mcp_servers: Vec<(String, McpServer)>,
    /// The `sdk` servers `--mcp-config` declares: they live in the host.
    pub sdk_servers: Vec<String>,
    pub strict_mcp: bool,
    pub start_mode: Option<PermissionMode>,
}

/// One parsed flag, in command-line order.
#[derive(Debug)]
struct Seen {
    name: String,
    value: Option<String>,
}

fn scan(args: &[OsString]) -> Result<Vec<Seen>, Refusal> {
    let mut seen = Vec::new();
    let mut at = 1;
    while at < args.len() {
        let token = args[at]
            .to_str()
            .ok_or_else(|| refuse("an argument is not valid text"))?
            .to_string();
        at += 1;
        if !token.starts_with('-') {
            return Err(refuse(format!("unexpected argument `{token}`")));
        }
        let (name, inline) = match token.split_once('=') {
            Some((name, value)) if token.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ => (token.clone(), None),
        };
        let Some(row) = flag_row(&name) else {
            return Err(refuse(format!("unknown option `{name}`")));
        };
        if let State::Unsupported(why) = row.state {
            return Err(refuse(format!("`{name}` is not supported: {why}")));
        }
        let value = match (row.arity, inline) {
            (Arity::Flag, Some(_)) => {
                return Err(refuse(format!("`{name}` takes no value")));
            }
            (Arity::Flag, None) => None,
            (_, Some(value)) => Some(value),
            (Arity::Value, None) => {
                let value = args
                    .get(at)
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| refuse(format!("`{name}` needs a value")))?
                    .to_string();
                at += 1;
                Some(value)
            }
            (Arity::Optional, None) => {
                match args
                    .get(at)
                    .and_then(|v| v.to_str())
                    .filter(|v| !v.starts_with('-'))
                {
                    Some(value) => {
                        at += 1;
                        Some(value.to_string())
                    }
                    None => None,
                }
            }
        };
        seen.push(Seen { name, value });
    }
    Ok(seen)
}

/// Claude's modes as Fontaine's. `bypassPermissions` has no equivalent and is refused.
pub fn parse_mode(text: &str) -> Result<PermissionMode, String> {
    match text {
        "default" | "manual" => Ok(PermissionMode::Manual),
        "acceptEdits" => Ok(PermissionMode::AcceptEdits),
        "plan" => Ok(PermissionMode::Plan),
        "dontAsk" => Ok(PermissionMode::DontAsk),
        "auto" => Ok(PermissionMode::Auto),
        "afk" => Ok(PermissionMode::Afk),
        "bypassPermissions" => Err(
            "`bypassPermissions` does not exist in Fontaine: the floor and the guardian gate every mode"
                .to_string(),
        ),
        other => Err(format!("`{other}` is not a permission mode")),
    }
}

/// Splits a comma list, leaving commas inside parentheses alone.
pub fn split_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut current) = (0usize, String::new());
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
            ',' if depth == 0 => out.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    out.push(current);
    out.into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// Maps the entries of `--allowedTools` or `--disallowedTools` to Fontaine's rule syntax.
fn map_rules(text: &str, name: &str) -> Result<Vec<String>, Refusal> {
    split_list(text)
        .iter()
        .map(|entry| {
            fontaine_core::extensions::map_tool_entry(entry)
                .map_err(|why| refuse(format!("`{name}`: {why}")))
        })
        .collect()
}

fn root_tools(text: &str) -> Result<Option<Vec<String>>, Refusal> {
    match text.trim() {
        "default" => Ok(None),
        "" => Ok(Some(Vec::new())),
        list => {
            let mut tools = Vec::new();
            for entry in split_list(list) {
                if entry.contains('(') {
                    return Err(refuse(format!(
                        "`--tools` entry `{entry}` has a specifier: it names tools, not calls; use `--allowedTools` for rules"
                    )));
                }
                let mapped = fontaine_core::extensions::map_tool_entry(&entry)
                    .map_err(|why| refuse(format!("`--tools`: {why}")))?;
                if !tools.contains(&mapped) {
                    tools.push(mapped);
                }
            }
            Ok(Some(tools))
        }
    }
}

/// What `--mcp-config` holds: inline JSON or the path of a file with it.
fn mcp_config(value: &str, options: &mut Options) -> Result<(), Refusal> {
    let text = if value.trim_start().starts_with('{') {
        value.to_string()
    } else {
        std::fs::read_to_string(value).map_err(|e| {
            refuse(format!(
                "`--mcp-config` {value}: {}",
                fontaine_tools::error_chain::chain(&e)
            ))
        })?
    };
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| refuse(format!("`--mcp-config` is not JSON: {e}")))?;
    let servers = json
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| refuse("`--mcp-config` has no `mcpServers` object"))?;
    for (name, server) in servers {
        let kind = server
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(if server.get("url").is_some() {
                "http"
            } else {
                "stdio"
            });
        match kind {
            "sdk" => options.sdk_servers.push(name.clone()),
            "sse" => {
                return Err(refuse(format!(
                    "MCP server `{name}` uses `sse`: Fontaine's http transport is streamable HTTP"
                )));
            }
            "stdio" | "http" => {
                let text = |key: &str| {
                    server
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                };
                let strings = |key: &str| -> Option<Vec<String>> {
                    server
                        .get(key)
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                };
                let map = |key: &str| -> Option<BTreeMap<String, String>> {
                    server
                        .get(key)
                        .and_then(serde_json::Value::as_object)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                                .collect()
                        })
                };
                let built = McpServer {
                    command: text("command"),
                    args: strings("args"),
                    env_vars: None,
                    env: map("env"),
                    url: text("url"),
                    headers: map("headers"),
                    headers_helper: None,
                    expand: false,
                };
                if built.command.is_none() && built.url.is_none() {
                    return Err(refuse(format!(
                        "MCP server `{name}` names neither a `command` nor a `url`"
                    )));
                }
                options.mcp_servers.push((name.clone(), built));
            }
            other => {
                return Err(refuse(format!(
                    "MCP server `{name}` has the unknown type `{other}`"
                )));
            }
        }
    }
    Ok(())
}

/// The first non-blank of a prompt text: an empty or whitespace-only prompt is treated as absent.
pub fn nonblank(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty())
}

fn pair(argv: &mut Vec<OsString>, flag: &str, value: &str) {
    argv.push(flag.into());
    argv.push(value.into());
}

/// Parses the SDK command line.
pub fn parse(args: &[OsString]) -> Result<Options, Refusal> {
    let seen = scan(args)?;
    let mut options = Options::default();
    let mut argv: Vec<OsString> = vec![
        OsString::from(
            args.first()
                .map_or(OsStr::new("fontaine"), |a| a.as_os_str()),
        ),
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--input-format".into(),
        "stream-json".into(),
    ];
    let (mut system, mut system_file) = (None, None);
    for Seen { name, value, .. } in &seen {
        let text = value.as_deref().unwrap_or("");
        match name.as_str() {
            "--output-format" | "--input-format" => {
                if text != "stream-json" {
                    return Err(refuse(format!(
                        "`{name}` must be `stream-json`, not `{text}`"
                    )));
                }
            }
            "--verbose"
            | "--setting-sources"
            | "--strict-mcp-config"
            | "--include-partial-messages" => match name.as_str() {
                "--strict-mcp-config" => options.strict_mcp = true,
                "--include-partial-messages" => argv.push("--include-partial-messages".into()),
                "--setting-sources" => {
                    for source in split_list(text) {
                        if !matches!(source.as_str(), "user" | "project" | "local") {
                            return Err(refuse(format!(
                                "`--setting-sources` has the unknown source `{source}`"
                            )));
                        }
                    }
                }
                _ => {}
            },
            "--system-prompt" => system = Some(text.to_string()),
            "--system-prompt-file" => {
                system_file = Some(std::fs::read_to_string(text).map_err(|e| {
                    refuse(format!(
                        "`--system-prompt-file` {text}: {}",
                        fontaine_tools::error_chain::chain(&e)
                    ))
                })?);
            }
            "--append-system-prompt" => {
                options.append_system_prompt = nonblank(Some(text.to_string()))
            }
            "--tools" => options.root_tools = root_tools(text)?,
            "--allowedTools" | "--allowed-tools" => {
                for rule in map_rules(text, name)? {
                    pair(&mut argv, "--allowed-tools", &rule);
                }
            }
            "--disallowedTools" | "--disallowed-tools" => {
                for rule in map_rules(text, name)? {
                    pair(&mut argv, "--disallowed-tools", &rule);
                }
            }
            "--max-turns" => pair(&mut argv, "--max-turns", text),
            "--max-budget-usd" => pair(&mut argv, "--max-budget-usd", text),
            "--model" => pair(&mut argv, "--model", text),
            "--permission-mode" => {
                let mode = parse_mode(text).map_err(refuse)?;
                options.start_mode = Some(mode);
                pair(&mut argv, "--permission-mode", mode.as_str());
            }
            "--permission-prompt-tool" => {
                if text != "stdio" {
                    return Err(refuse(format!(
                        "`--permission-prompt-tool {text}` is not supported: only `stdio`, the `canUseTool` callback, is"
                    )));
                }
                options.permission_prompt = true;
            }
            "--continue" | "-c" => argv.push("--continue".into()),
            "--resume" | "-r" => {
                if text.is_empty() {
                    argv.push("--continue".into());
                } else {
                    pair(&mut argv, "--resume", text);
                }
            }
            "--session-id" => pair(&mut argv, "--session-id", text),
            "--fork-session" => argv.push("--fork-session".into()),
            "--settings" => {
                let empty = serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .is_some_and(|v| v.as_object().is_some_and(|o| o.is_empty()));
                if !empty {
                    return Err(refuse(
                        "`--settings` other than `{}` is not supported: Claude's settings schema is not Fontaine's",
                    ));
                }
            }
            "--add-dir" => pair(&mut argv, "--add-dir", text),
            "--mcp-config" => mcp_config(text, &mut options)?,
            "--effort" => pair(&mut argv, "--effort", text),
            "--json-schema" => pair(&mut argv, "--json-schema", text),
            other => return Err(refuse(format!("`{other}` is not handled"))),
        }
    }
    if system.is_some() && system_file.is_some() {
        return Err(refuse(
            "`--system-prompt` and `--system-prompt-file` cannot be combined",
        ));
    }
    options.system_prompt = nonblank(system.or(system_file));
    options.fontaine_args = argv;
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<OsString> {
        std::iter::once("fontaine")
            .chain(parts.iter().copied())
            .map(OsString::from)
            .collect()
    }

    const BASE: [&str; 5] = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
    ];

    #[test]
    fn the_shape_every_sdk_sends_is_sdk_mode() {
        assert!(is_sdk_invocation(&argv(&BASE)));
        assert!(is_sdk_invocation(&argv(&[
            "--output-format=stream-json",
            "--verbose",
            "--input-format=stream-json",
            "--permission-prompt-tool=stdio",
            "--resume=abc",
        ])));
        assert!(is_sdk_invocation(&argv(&[
            "--output-format",
            "stream-json",
            "--verbose",
            "--system-prompt",
            "",
            "--allowedTools",
            "Read,Bash(git *)",
            "--input-format",
            "stream-json",
        ])));
    }

    #[test]
    fn plain_runs_are_not_sdk_mode() {
        assert!(!is_sdk_invocation(&argv(&[])));
        assert!(!is_sdk_invocation(&argv(&[
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose"
        ])));
        assert!(!is_sdk_invocation(&argv(&[
            "--print",
            "--output-format=stream-json",
            "--input-format=stream-json",
            "--verbose"
        ])));
        assert!(!is_sdk_invocation(&argv(&[
            "--output-format",
            "stream-json",
            "hello"
        ])));
        assert!(!is_sdk_invocation(&argv(&[
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json"
        ])));
        assert!(!is_sdk_invocation(&argv(&[
            "--output-format",
            "json",
            "--input-format",
            "stream-json",
            "--verbose"
        ])));
    }

    #[test]
    fn an_operand_that_looks_like_a_flag_does_not_flip_detection() {
        for value in ["-p", "--verbose", "stream-json", "--print"] {
            let mut sdk = argv(&BASE);
            sdk.extend(["--system-prompt", value].map(OsString::from));
            assert!(is_sdk_invocation(&sdk), "{value}");
            let mut plain = argv(&[
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
            ]);
            plain.extend(["--append-system-prompt", value].map(OsString::from));
            assert!(!is_sdk_invocation(&plain), "{value}");
        }
    }

    #[test]
    fn supported_flags_map_to_fontaines_own() {
        let mut parts = BASE.to_vec();
        parts.extend([
            "--allowedTools",
            "Read,Bash(git status, git log),mcp__srv__tool",
            "--disallowedTools",
            "WebFetch",
            "--max-turns",
            "3",
            "--model",
            "main",
            "--permission-mode",
            "default",
            "--permission-prompt-tool",
            "stdio",
            "--resume=abc",
            "--fork-session",
            "--add-dir",
            "/tmp/x",
            "--include-partial-messages",
            "--effort",
            "low",
            "--settings",
            "{}",
            "--setting-sources=user,project",
        ]);
        let options = parse(&argv(&parts)).unwrap();
        let rendered: Vec<String> = options
            .fontaine_args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for expected in [
            "--allowed-tools",
            "read",
            "shell(git status, git log)",
            "mcp__srv__tool",
            "--disallowed-tools",
            "webfetch",
            "--max-turns",
            "3",
            "--model",
            "main",
            "--permission-mode",
            "manual",
            "--resume",
            "abc",
            "--fork-session",
            "--add-dir",
            "/tmp/x",
            "--include-partial-messages",
            "--effort",
            "low",
        ] {
            assert!(
                rendered.iter().any(|a| a == expected),
                "{expected} in {rendered:?}"
            );
        }
        assert!(options.permission_prompt);
        assert_eq!(options.start_mode, Some(PermissionMode::Manual));
    }

    #[test]
    fn unsupported_and_unknown_flags_name_themselves() {
        for flag in [
            "--fallback-model",
            "--betas",
            "--thinking",
            "--plugin-dir",
            "--hard-fail",
            "--include-hook-events",
        ] {
            let mut parts = BASE.to_vec();
            parts.push(flag);
            if !matches!(flag, "--hard-fail" | "--include-hook-events") {
                parts.push("x");
            }
            let refusal = parse(&argv(&parts)).unwrap_err();
            assert!(refusal.0.contains(flag), "{refusal}");
            assert!(refusal.to_string().contains("docs/agent-sdk.md"));
        }
        let mut parts = BASE.to_vec();
        parts.push("--no-such-flag");
        assert!(
            parse(&argv(&parts))
                .unwrap_err()
                .0
                .contains("--no-such-flag")
        );
    }

    #[test]
    fn every_row_of_the_matrix_is_classified_and_every_unsupported_flag_is_refused() {
        for row in MATRIX.iter().filter(|r| r.kind == Kind::Flag) {
            if let State::Unsupported(_) = row.state {
                for name in row.names {
                    let mut parts = BASE.to_vec();
                    parts.push(name);
                    if row.arity != Arity::Flag {
                        parts.push("x");
                    }
                    assert!(parse(&argv(&parts)).is_err(), "{name}");
                }
            }
        }
    }

    #[test]
    fn bypass_permissions_and_named_permission_tools_are_refused() {
        let mut parts = BASE.to_vec();
        parts.extend(["--permission-mode", "bypassPermissions"]);
        assert!(
            parse(&argv(&parts))
                .unwrap_err()
                .0
                .contains("bypassPermissions")
        );
        let mut parts = BASE.to_vec();
        parts.extend(["--permission-prompt-tool", "mcp__a__b"]);
        assert!(parse(&argv(&parts)).is_err());
        let mut parts = BASE.to_vec();
        parts.push("--allow-dangerously-skip-permissions");
        assert!(parse(&argv(&parts)).is_err());
        assert_eq!(parse_mode("default"), Ok(PermissionMode::Manual));
        assert_eq!(parse_mode("afk"), Ok(PermissionMode::Afk));
        assert!(parse_mode("bypassPermissions").is_err());
    }

    #[test]
    fn tools_restrict_the_base_set() {
        let tools = |value: &str| {
            let mut parts = BASE.to_vec();
            parts.extend(["--tools", value]);
            parse(&argv(&parts)).map(|o| o.root_tools)
        };
        assert_eq!(tools("default").unwrap(), None);
        assert_eq!(tools("").unwrap(), Some(Vec::new()));
        assert_eq!(
            tools("Read,Bash").unwrap(),
            Some(vec!["read".to_string(), "shell".to_string()])
        );
        assert!(tools("Bash(git *)").is_err());
        assert!(tools("Nope").is_err());
    }

    #[test]
    fn blank_system_prompts_are_absent() {
        for value in ["", "  \n"] {
            let mut parts = BASE.to_vec();
            parts.extend(["--system-prompt", value]);
            assert_eq!(parse(&argv(&parts)).unwrap().system_prompt, None);
        }
        let mut parts = BASE.to_vec();
        parts.extend(["--system-prompt", "be terse"]);
        assert_eq!(
            parse(&argv(&parts)).unwrap().system_prompt.as_deref(),
            Some("be terse")
        );
    }

    #[test]
    fn mcp_config_maps_servers_and_refuses_sse() {
        let config = r#"{"mcpServers":{"a":{"command":"x","args":["1"]},"b":{"type":"http","url":"https://e/mcp"},"c":{"type":"sdk","name":"c"}}}"#;
        let mut parts = BASE.to_vec();
        parts.extend(["--mcp-config", config, "--strict-mcp-config"]);
        let options = parse(&argv(&parts)).unwrap();
        assert_eq!(options.mcp_servers.len(), 2);
        assert_eq!(options.sdk_servers, ["c"]);
        assert!(options.strict_mcp);
        let sse = r#"{"mcpServers":{"a":{"type":"sse","url":"https://e"}}}"#;
        let mut parts = BASE.to_vec();
        parts.extend(["--mcp-config", sse]);
        assert!(parse(&argv(&parts)).unwrap_err().0.contains("sse"));
    }

    #[test]
    fn settings_other_than_an_empty_object_is_refused() {
        let mut parts = BASE.to_vec();
        parts.extend(["--settings", r#"{"permissions":{}}"#]);
        assert!(parse(&argv(&parts)).is_err());
    }

    #[test]
    fn the_docs_name_the_tested_versions() {
        let docs = include_str!("../../../../docs/agent-sdk.md");
        for version in [TESTED_TS_SDK, TESTED_PY_SDK, CLAUDE_CODE_PROTOCOL_VERSION] {
            assert!(docs.contains(version), "{version}");
        }
    }

    #[test]
    fn the_matrix_renders_to_the_docs_table() {
        let docs = include_str!("../../../../docs/agent-sdk.md");
        let start = docs
            .find("<!-- matrix:start -->")
            .expect("the docs mark where the matrix starts");
        let end = docs
            .find("<!-- matrix:end -->")
            .expect("the docs mark where the matrix ends");
        let table = docs[start + "<!-- matrix:start -->".len()..end].trim();
        assert_eq!(
            table,
            render_matrix().trim(),
            "regenerate the matrix table in docs/agent-sdk.md"
        );
    }
}
