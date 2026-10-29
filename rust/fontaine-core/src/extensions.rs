//! Extension files: skills, agent files and output styles. They live beside the settings files,
//! in `<config dir>/` and `<root>/.fontaine/`, with Claude Code's frontmatter so files port by
//! copying. Loading is strict about everything that touches permissions: a name that cannot be
//! mapped, a rule the validator rejects, or a key that would run something is an error naming
//! the file, because ignoring it could widen what an agent may do.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use fontaine_agents::mode::ModeSetting;
use fontaine_agents::registry::{AgentType, ModelSpec, TypeOrigin};
use fontaine_tools::names::{is_builtin, is_mcp};

use crate::frontmatter::{self, Document, Value};
use crate::permission_rules::{Rule, parse_rule};
use crate::plugins::{PluginDir, rewrite_mcp_reference};

pub const SKILLS_DIR: &str = "skills";
pub const SKILL_FILE: &str = "SKILL.md";
pub const AGENTS_DIR: &str = "agents";
pub const STYLES_DIR: &str = "output-styles";

/// A file that cannot be loaded. Startup stops with exit code 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionError {
    pub file: PathBuf,
    pub message: String,
}

impl fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.file.display(), self.message)
    }
}

impl std::error::Error for ExtensionError {}

impl ExtensionError {
    pub fn exit_code(&self) -> u8 {
        crate::config::CONFIG_EXIT_CODE
    }
}

/// Which side of the trust line a file is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    /// A plugin: installed by the user, but its content is third-party and tighten-only, so it
    /// follows the project permission rules rather than the user's.
    Plugin,
}

impl Scope {
    /// Whether the scope can only narrow permissions. Plugin content is third-party even though
    /// the user installed it.
    pub fn is_tightened(self) -> bool {
        matches!(self, Scope::Project | Scope::Plugin)
    }
}

#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub argument_hint: Option<String>,
    /// Listed in the `/` menu and run as `/name`.
    pub user_invocable: bool,
    /// Kept out of the skill list and refused by the `skill` tool; only the user runs it.
    pub disable_model_invocation: bool,
    /// Rules the skill lends to the agent that runs it for the rest of the turn. Empty for a
    /// project skill: a repository cannot approve calls.
    pub allowed_tools: Vec<Rule>,
    pub scope: Scope,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct OutputStyle {
    pub name: String,
    pub description: String,
    pub body: String,
    pub scope: Scope,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct Extensions {
    pub skills: Vec<Skill>,
    pub agent_types: Vec<AgentType>,
    pub output_styles: Vec<OutputStyle>,
}

impl Extensions {
    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    pub fn output_style(&self, name: &str) -> Option<&OutputStyle> {
        self.output_styles.iter().find(|style| style.name == name)
    }
}

pub struct LoadParams<'a> {
    pub config_dir: &'a Path,
    /// The primary root; its `.fontaine/` folder is the project scope.
    pub project_root: &'a Path,
    /// The MCP servers configured for this session. A project agent file may name their tools
    /// and no others: it cannot reach for a tool its parent does not have.
    pub servers: &'a [String],
    /// Enabled, installed plugins, loaded after the user root and before the project root.
    pub plugins: &'a [PluginDir],
    /// Non-fatal degradation the caller shows (for example an agent file skipped over an
    /// external MCP server).
    pub warnings: &'a mut Vec<String>,
}

/// The Fontaine tool a Claude Code tool name stands for.
fn claude_tool(name: &str) -> Option<&'static str> {
    fontaine_tools::descriptor::find_claude(name).map(|d| d.name)
}

/// One entry of `tools`, `disallowedTools` or `allowed-tools` in Fontaine's names, with its
/// `(specifier)` kept as written and the result checked by the rule validator.
pub fn map_tool_entry(entry: &str) -> Result<String, String> {
    let entry = entry.trim();
    let (name, rest) = match entry.find('(') {
        Some(open) => (entry[..open].trim(), &entry[open..]),
        None => (entry, ""),
    };
    let mapped = match claude_tool(name) {
        Some(mapped) => mapped.to_string(),
        None if is_builtin(name) || is_mcp(name) => name.to_string(),
        None => {
            let known: Vec<&str> = fontaine_tools::descriptor::claude_names().collect();
            let (last, rest) = known.split_last().expect("the table has Claude names");
            return Err(format!(
                "`{name}` is not a tool Fontaine has; the Claude Code names it knows are {} and {last}",
                rest.join(", ")
            ));
        }
    };
    let rule = format!("{mapped}{rest}");
    parse_rule(&rule).map_err(|e| e.to_string())?;
    Ok(rule)
}

fn map_tool_list(doc: &Document, key: &str) -> Result<Option<Vec<String>>, String> {
    if matches!(doc.get(key), Some(Value::Nested)) {
        return Err(format!(
            "`{key}` must be a list or a comma-separated string"
        ));
    }
    let Some(items) = doc.list(key) else {
        return Ok(None);
    };
    items
        .iter()
        .map(|item| map_tool_entry(item).map_err(|e| format!("`{key}`: {e}")))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// `model: inherit` is no model; anything else is a model reference or a splice marker.
fn model_of(doc: &Document) -> Result<Option<ModelSpec>, String> {
    match doc.text("model").as_deref().map(str::trim) {
        None | Some("inherit") | Some("") => Ok(None),
        Some(text) => ModelSpec::parse(text).map(Some).map_err(|e| {
            // The parse error is a message with no source.
            format!("`model`: {e} (Claude Code model aliases such as `sonnet` are not models here)")
        }),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Keys that would run something or change permissions from inside a file.
fn refuse_executable_keys(doc: &Document, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        if doc.has(key) {
            return Err(format!(
                "`{key}` is not supported in this kind of file: files load without workspace trust, so they cannot carry executables or permission settings"
            ));
        }
    }
    Ok(())
}

fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

fn read_document(path: &Path) -> Result<Document, ExtensionError> {
    let fail = |message: String| ExtensionError {
        file: path.to_path_buf(),
        message,
    };
    let text = std::fs::read_to_string(path).map_err(|e| {
        fail(format!(
            "cannot read it: {}",
            fontaine_tools::error_chain::chain(&e)
        ))
    })?;
    frontmatter::parse(&text).map_err(fail)
}

fn load_skill(path: &Path, default_name: &str, scope: Scope) -> Result<Skill, ExtensionError> {
    let doc = read_document(path)?;
    let fail = |message: String| ExtensionError {
        file: path.to_path_buf(),
        message,
    };
    refuse_executable_keys(&doc, &["hooks", "mcpServers", "permissionMode"]).map_err(fail)?;
    let name = doc.text("name").unwrap_or_else(|| default_name.to_string());
    if !valid_name(&name) {
        return Err(fail(format!(
            "`{name}` is not a skill name: use letters, digits, `-` and `_`"
        )));
    }
    model_of(&doc).map_err(fail)?;
    let allowed = map_tool_list(&doc, "allowed-tools").map_err(fail)?;
    let allowed_tools = match (scope, allowed) {
        (Scope::User, Some(entries)) => entries
            .iter()
            .map(|entry| parse_rule(entry).expect("validated when the definition was loaded"))
            .collect(),
        // Validated above; a repository's or a plugin's skill cannot approve anything.
        _ => Vec::new(),
    };
    let flag = |key: &str, default: bool| {
        doc.flag(key)
            .map(|value| value.unwrap_or(default))
            .map_err(fail)
    };
    Ok(Skill {
        description: doc
            .text("description")
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| first_line(&doc.body)),
        // `[issue-number]` is how Claude Code files write a hint, which a YAML reader would
        // take for a one-item list.
        argument_hint: match doc.get("argument-hint") {
            Some(frontmatter::Value::List(items)) => Some(format!("[{}]", items.join(", "))),
            Some(frontmatter::Value::Scalar(text)) => Some(text.clone()),
            _ => None,
        }
        .filter(|hint| !hint.is_empty()),
        user_invocable: flag("user-invocable", true)?,
        disable_model_invocation: flag("disable-model-invocation", false)?,
        body: doc.body.clone(),
        allowed_tools,
        scope,
        path: path.to_path_buf(),
        name,
    })
}

fn names_a_known_tool(entry: &str, servers: &[String]) -> bool {
    let name = crate::extensions::entry_name(entry);
    if is_builtin(name) {
        return true;
    }
    match name.strip_prefix(fontaine_tools::names::MCP_TOOL_PREFIX) {
        Some(rest) => {
            let server = rest.split("__").next().unwrap_or("");
            servers.iter().any(|configured| configured == server)
        }
        None => false,
    }
}

/// The tool a mapped entry names: `shell(git *)` is `shell`.
pub fn entry_name(entry: &str) -> &str {
    fontaine_agents::registry::entry_tool(entry)
}

fn load_agent(path: &Path, scope: Scope, servers: &[String]) -> Result<AgentType, ExtensionError> {
    agent_from(path, scope, servers, None, &mut Vec::new())
        .map(|agent| agent.expect("an agent file without a plugin rewrite never skips"))
}

/// Loads one agent file. A plugin file follows the project rules, with bare references to the
/// plugin's own MCP servers rewritten to the composed name first. A plugin entry naming an MCP
/// server neither the plugin ships nor the session has skips the file with a warning instead of
/// failing startup: whether that server exists is the user's configuration, not a defect in the
/// file.
fn agent_from(
    path: &Path,
    scope: Scope,
    servers: &[String],
    mcp_names: Option<&BTreeMap<String, String>>,
    warnings: &mut Vec<String>,
) -> Result<Option<AgentType>, ExtensionError> {
    let doc = read_document(path)?;
    let fail = |message: String| ExtensionError {
        file: path.to_path_buf(),
        message,
    };
    refuse_executable_keys(&doc, &["hooks", "mcpServers"]).map_err(fail)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = doc.text("name").unwrap_or(stem);
    if !valid_name(&name) {
        return Err(fail(format!(
            "`{name}` is not an agent name: use letters, digits, `-` and `_`"
        )));
    }
    let model = model_of(&doc).map_err(fail)?;
    if scope.is_tightened() && model.is_some() {
        return Err(fail(format!(
            "{} cannot choose a model: it runs on the session model (leave `model` out or write `inherit`)",
            tightened_label(scope)
        )));
    }
    let mode = match (scope, doc.text("permissionMode").as_deref().map(str::trim)) {
        (Scope::User, None | Some("inherit")) => ModeSetting::Inherit,
        (Scope::User, Some("default" | "manual")) => ModeSetting::Manual,
        (_, Some("dontAsk")) => ModeSetting::DontAsk,
        (_, Some("inherit")) if scope.is_tightened() => {
            return Err(fail(format!(
                "{} cannot inherit its mode: write `permissionMode: default` or `dontAsk`",
                tightened_label(scope)
            )));
        }
        (_, None | Some("default" | "manual")) if scope.is_tightened() => ModeSetting::Manual,
        (_, Some(other)) => {
            return Err(fail(format!(
                "`permissionMode: {other}` is not allowed in an agent file: only `inherit`, `default` and `dontAsk` are, so an agent never runs looser than the session"
            )));
        }
        _ => unreachable!("every (scope, permissionMode) pair is covered"),
    };
    let mut tools = map_tool_list(&doc, "tools").map_err(fail)?;
    let mut disallowed = map_tool_list(&doc, "disallowedTools")
        .map_err(fail)?
        .unwrap_or_default();
    if let Some(names) = mcp_names {
        for entry in tools.iter_mut().flatten() {
            *entry = rewrite_mcp_reference(entry, names);
        }
        for entry in &mut disallowed {
            *entry = rewrite_mcp_reference(entry, names);
        }
    }
    if scope.is_tightened()
        && let Some(entry) = tools
            .iter()
            .flatten()
            .find(|entry| !names_a_known_tool(entry, servers))
    {
        if scope == Scope::Plugin
            && let Some(server) = mcp_server_of(entry)
            && let Some(names) = mcp_names
            && !names.values().any(|composed| composed == server)
        {
            warnings.push(format!(
                "{}: the agent file names `{entry}`, an MCP server neither the plugin nor the session has; it is skipped",
                path.display()
            ));
            return Ok(None);
        }
        return Err(fail(format!(
            "`tools` names `{entry}`, which the session does not have: {} can only use tools its parent has",
            tightened_label(scope)
        )));
    }
    let body = doc.body.trim();
    Ok(Some(AgentType {
        description: doc
            .text("description")
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| first_line(body)),
        prompt: (!body.is_empty()).then(|| body.to_string()),
        tools,
        disallowed,
        model,
        mode,
        origin: match scope {
            Scope::User => TypeOrigin::UserFile(path.to_path_buf()),
            Scope::Project => TypeOrigin::ProjectFile(path.to_path_buf()),
            Scope::Plugin => TypeOrigin::PluginFile(path.to_path_buf()),
        },
        inherits_context: false,
        name,
    }))
}

fn tightened_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Project => "a project agent file",
        Scope::Plugin => "a plugin agent file",
        Scope::User => "an agent file",
    }
}

/// The MCP server an entry names, for `mcp__<server>` and `mcp__<server>__<tool>` entries.
fn mcp_server_of(entry: &str) -> Option<&str> {
    entry_name(entry)
        .strip_prefix(fontaine_tools::names::MCP_TOOL_PREFIX)
        .and_then(|rest| rest.split("__").next())
}

fn load_style(path: &Path, scope: Scope) -> Result<OutputStyle, ExtensionError> {
    let doc = read_document(path)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = doc.text("name").unwrap_or(stem);
    if !valid_name(&name) {
        return Err(ExtensionError {
            file: path.to_path_buf(),
            message: format!(
                "`{name}` is not an output style name: use letters, digits, `-` and `_`"
            ),
        });
    }
    Ok(OutputStyle {
        description: doc
            .text("description")
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| first_line(&doc.body)),
        body: doc.body.trim().to_string(),
        scope,
        path: path.to_path_buf(),
        name,
    })
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    sorted_entries(dir)
        .into_iter()
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect()
}

/// Loads every extension file of both scopes and every enabled plugin. The first file that
/// cannot be loaded stops the load.
pub fn load(params: &mut LoadParams<'_>) -> Result<Extensions, ExtensionError> {
    let project = params.project_root.join(crate::config::PROJECT_DIR);
    let mut out = Extensions::default();
    let warnings: &mut Vec<String> = &mut *params.warnings;
    load_scope(params.config_dir, Scope::User, params.servers, &mut out)?;
    for plugin in params.plugins {
        load_plugin(plugin, params.servers, warnings, &mut out)?;
    }
    load_scope(project.as_path(), Scope::Project, params.servers, &mut out)?;
    out.skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn load_scope(
    base: &Path,
    scope: Scope,
    servers: &[String],
    out: &mut Extensions,
) -> Result<(), ExtensionError> {
    for dir in sorted_entries(&base.join(SKILLS_DIR)) {
        let file = dir.join(SKILL_FILE);
        if !dir.is_dir() || !file.is_file() {
            continue;
        }
        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let skill = load_skill(&file, &dir_name, scope)?;
        // A personal skill outranks a project one of the same name, as in Claude Code.
        match out.skills.iter().position(|s| s.name == skill.name) {
            Some(at) if out.skills[at].scope == Scope::User => {}
            Some(at) => out.skills[at] = skill,
            None => out.skills.push(skill),
        }
    }
    for file in markdown_files(&base.join(AGENTS_DIR)) {
        let agent = load_agent(&file, scope, servers)?;
        // A project definition replaces a user one of the same name, and may only narrow.
        out.agent_types.retain(|t| t.name != agent.name);
        out.agent_types.push(agent);
    }
    for file in markdown_files(&base.join(STYLES_DIR)) {
        let style = load_style(&file, scope)?;
        out.output_styles.retain(|s| s.name != style.name);
        out.output_styles.push(style);
    }
    Ok(())
}

fn load_plugin(
    plugin: &PluginDir,
    servers: &[String],
    warnings: &mut Vec<String>,
    out: &mut Extensions,
) -> Result<(), ExtensionError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut take = |name: String, path: &Path| -> Result<(), ExtensionError> {
        if !valid_namespaced(&name) {
            return Err(ExtensionError {
                file: path.to_path_buf(),
                message: format!(
                    "`{name}` is not a plugin component name: each `:`-separated segment must use letters, digits, `-` and `_`"
                ),
            });
        }
        if !seen.insert(name.clone()) {
            return Err(ExtensionError {
                file: path.to_path_buf(),
                message: format!("two components of the plugin load as `{name}`"),
            });
        }
        Ok(())
    };

    for file in &plugin.skills {
        // `skills/<name>/SKILL.md` names the skill after its directory; a manifest path naming
        // a bare file names it after the file stem.
        let default_name = if file.path.file_name().is_some_and(|n| n == SKILL_FILE) {
            file.path
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            file.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut skill = load_skill(&file.path, &default_name, Scope::Plugin)?;
        let name = namespaced(&plugin.name, &file.segments, &skill.name);
        take(name.clone(), &file.path)?;
        skill.name = name;
        out.skills.push(skill);
    }
    for file in &plugin.commands {
        let stem = file
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut skill = load_skill(&file.path, &stem, Scope::Plugin)?;
        let name = namespaced(&plugin.name, &file.segments, &skill.name);
        take(name.clone(), &file.path)?;
        skill.name = name;
        out.skills.push(skill);
    }
    for file in &plugin.agents {
        let Some(mut agent) = agent_from(
            &file.path,
            Scope::Plugin,
            servers,
            Some(&plugin.mcp_names),
            warnings,
        )?
        else {
            continue;
        };
        let name = namespaced(&plugin.name, &file.segments, &agent.name);
        take(name.clone(), &file.path)?;
        agent.name = name;
        out.agent_types.retain(|t| t.name != agent.name);
        out.agent_types.push(agent);
    }
    for file in &plugin.styles {
        let mut style = load_style(&file.path, Scope::Plugin)?;
        let name = namespaced(&plugin.name, &file.segments, &style.name);
        take(name.clone(), &file.path)?;
        style.name = name;
        out.output_styles.retain(|s| s.name != style.name);
        out.output_styles.push(style);
    }
    Ok(())
}

fn namespaced(plugin: &str, segments: &[String], base: &str) -> String {
    let mut parts = Vec::with_capacity(2 + segments.len());
    parts.push(plugin.to_string());
    parts.extend(segments.iter().cloned());
    parts.push(base.to_string());
    parts.join(":")
}

fn valid_namespaced(name: &str) -> bool {
    !name.is_empty() && name.split(':').all(valid_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_tool_error_names_every_claude_code_alias() {
        let message = map_tool_entry("Nope").unwrap_err();
        for name in fontaine_tools::descriptor::claude_names() {
            assert!(message.contains(name), "{message} omits {name}");
        }
    }

    #[test]
    fn claude_names_map_and_specifiers_survive() {
        assert_eq!(map_tool_entry("Read").unwrap(), "read");
        assert_eq!(map_tool_entry("MultiEdit").unwrap(), "edit");
        assert_eq!(map_tool_entry("Bash(git *)").unwrap(), "shell(git *)");
        assert_eq!(map_tool_entry("Task").unwrap(), "agent");
        assert_eq!(map_tool_entry("shell").unwrap(), "shell");
        assert_eq!(
            map_tool_entry("mcp__gh__create_issue").unwrap(),
            "mcp__gh__create_issue"
        );
    }

    #[test]
    fn the_job_tools_map_from_their_claude_code_names() {
        assert_eq!(map_tool_entry("Monitor").unwrap(), "monitor");
        assert_eq!(
            map_tool_entry("Monitor(tail *)").unwrap(),
            "monitor(tail *)"
        );
        for name in ["BashOutput", "TaskOutput"] {
            assert_eq!(map_tool_entry(name).unwrap(), "job_output");
        }
        for name in ["KillShell", "KillBash", "TaskStop"] {
            assert_eq!(map_tool_entry(name).unwrap(), "job_cancel");
        }
    }

    #[test]
    fn web_search_maps_to_websearch() {
        assert_eq!(map_tool_entry("WebSearch").unwrap(), "websearch");
    }

    #[test]
    fn todo_write_maps_to_todo_write() {
        assert_eq!(map_tool_entry("TodoWrite").unwrap(), "todo_write");
    }

    #[test]
    fn unmapped_names_and_bad_specifiers_are_errors() {
        assert!(
            map_tool_entry("MadeUpTool")
                .unwrap_err()
                .contains("MadeUpTool")
        );
        assert!(map_tool_entry("WebSearch(domain:example.com)").is_err());
        assert!(map_tool_entry("Bash(git add:*)").is_err());
        assert!(map_tool_entry("Bash(").is_err());
    }

    #[test]
    fn a_nested_tool_list_is_an_error_and_comments_in_a_block_list_are_skipped() {
        let nested = frontmatter::parse("---\ndisallowedTools:\n  Bash: no\n---\n").unwrap();
        assert!(
            map_tool_list(&nested, "disallowedTools")
                .unwrap_err()
                .contains("disallowedTools")
        );
        let commented =
            frontmatter::parse("---\ndisallowedTools:\n  - Bash\n  # - WebFetch\n  - Write\n---\n")
                .unwrap();
        assert_eq!(
            map_tool_list(&commented, "disallowedTools")
                .unwrap()
                .unwrap(),
            ["shell", "write"]
        );
    }

    #[test]
    fn a_tool_list_continued_on_the_next_line_keeps_every_entry() {
        let doc = frontmatter::parse("---\ndisallowedTools: Bash,\n  Write\n---\n").unwrap();
        assert_eq!(
            map_tool_list(&doc, "disallowedTools").unwrap().unwrap(),
            ["shell", "write"]
        );
    }
}
